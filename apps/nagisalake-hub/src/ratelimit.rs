//! Request throttling for the credential and submission endpoints.
//!
//! Argon2 is deliberately expensive, which protects a stolen hash but does
//! nothing to stop an attacker trying millions of passwords against the live
//! endpoint. Bounding the hashing pool keeps the server responsive; it does not
//! keep the account safe. That needs a limit on attempts.
//!
//! Two dimensions, both required:
//!
//! - **Per source address**, to slow a single host walking a password list.
//! - **Per account**, so distributing the attempt across many addresses does not
//!   buy unlimited guesses against one victim.
//!
//! State is in memory, which is correct for the single-Hub deployment this
//! serves. A multi-Hub setup would need a shared counter, and until then a
//! second replica would multiply every limit by the number of replicas.

use hashbrown::HashTable;
use std::{
    collections::{VecDeque, hash_map::RandomState},
    hash::BuildHasher,
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

/// A token bucket: `capacity` attempts, refilled at `refill_per_second`.
///
/// Chosen over a fixed window because a fixed window lets an attacker spend the
/// whole quota at the end of one window and again at the start of the next,
/// doubling the intended rate at the boundary.
#[derive(Debug, Clone, Copy)]
pub struct Quota {
    pub capacity:          f64,
    pub refill_per_second: f64,
}

impl Quota {
    pub const fn new(capacity: u32, refill_per_second: f64) -> Self {
        Self {
            capacity: capacity as f64,
            refill_per_second,
        }
    }

    /// How long until one token is available again.
    fn retry_after(&self, tokens: f64) -> Duration {
        if self.refill_per_second <= 0.0 {
            return Duration::from_secs(3600);
        }
        let missing = (1.0 - tokens).max(0.0);
        Duration::from_secs_f64(missing / self.refill_per_second)
    }
}

#[derive(Debug)]
struct Bucket {
    /// Stored so a bucket can be rehashed on resize and compared on lookup
    /// without materializing a joined `scope\0key` string per request.
    scope:     Box<str>,
    key:       Box<str>,
    tokens:    f64,
    last_seen: Instant,
    sequence:  u64,
}

impl Bucket {
    fn consume(&mut self, now: Instant, quota: Quota) -> Decision {
        let elapsed = now.saturating_duration_since(self.last_seen).as_secs_f64();
        self.tokens = (self.tokens + elapsed * quota.refill_per_second).min(quota.capacity);
        self.last_seen = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Decision::Allow
        } else {
            Decision::Deny {
                retry_after_seconds: quota.retry_after(self.tokens).as_secs().max(1),
            }
        }
    }
}

#[derive(Debug, Default)]
struct Shard {
    buckets:       HashTable<Bucket>,
    order:         VecDeque<(u64, u64)>, // (access sequence, hash), no duplicated strings
    next_sequence: u64,
}

impl Shard {
    fn record_access(&mut self, sequence: u64, hash: u64) {
        self.order.push_back((sequence, hash));
        // A hot key can leave stale slots behind an untouched cold key. Keep
        // queue storage bounded as well as bucket storage, amortizing cleanup.
        if self.order.len() > MAX_BUCKETS_PER_SHARD * 2 {
            let buckets = &self.buckets;
            self.order.retain(|(seq, hash)| {
                buckets
                    .find(*hash, |bucket| bucket.sequence == *seq)
                    .is_some()
            });
        }
    }

    fn evict_oldest(&mut self) {
        while let Some((sequence, hash)) = self.order.pop_front() {
            if let Ok(entry) = self
                .buckets
                .find_entry(hash, |bucket| bucket.sequence == sequence)
            {
                entry.remove();
                break;
            }
        }
    }
}

/// Outcome of a rate limit check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Denied; the caller should send `Retry-After` with this many seconds.
    Deny {
        retry_after_seconds: u64,
    },
}

impl Decision {
    #[cfg(test)]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// Named limits, each with its own bucket namespace.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Sign-in attempts from one address.
    pub login_per_ip:      Quota,
    /// Sign-in attempts against one account, from anywhere.
    pub login_per_account: Quota,
    /// Account creation from one address. The tightest limit here: this is what
    /// stops a script from filling the user table.
    pub register_per_ip:   Quota,
    /// Session rotation from one address. Generous, because a legitimate tab
    /// refreshes on a timer and several tabs share an address.
    pub refresh_per_ip:    Quota,
    /// Job submissions per organization. Quota already bounds concurrency; this
    /// bounds the request rate, including the submissions that get rejected.
    pub submit_per_org:    Quota,
    /// Federated sign-in starts per address, to stop the callback table being
    /// filled with pending authorizations.
    pub oauth_per_ip:      Quota,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            // 10 attempts, then one more every 6 seconds.
            login_per_ip:      Quota::new(10, 1.0 / 6.0),
            // Match the durable lockout threshold: ten verified failures can
            // reach the store, then the account is locked for fifteen minutes.
            login_per_account: Quota::new(10, 1.0 / 30.0),
            // 3 accounts per address, then one every 10 minutes.
            register_per_ip:   Quota::new(3, 1.0 / 600.0),
            refresh_per_ip:    Quota::new(60, 1.0),
            submit_per_org:    Quota::new(60, 2.0),
            oauth_per_ip:      Quota::new(10, 1.0 / 10.0),
        }
    }
}

/// In-memory limiter.
///
/// Sharded because every authenticated request and every job submission passes
/// through here: a single `Mutex<HashMap>` made this the one point all of them
/// serialize on, and each check also allocated a `format!("{scope}\0{key}")`
/// lookup key. Lookups now hash `(scope, key)` in place and only touch one
/// shard, so unrelated addresses and organizations no longer contend.
#[derive(Clone)]
pub struct RateLimiter {
    limits:  Limits,
    shards:  Arc<[Mutex<Shard>]>,
    /// One instance-wide seed. A fixed hasher would let a caller that controls
    /// keys (addresses, account ids) pick colliding ones on purpose.
    hasher:  RandomState,
    enabled: bool,
}

/// Buckets idle for this long are dropped, so the map cannot grow without bound
/// from one-off addresses.
const IDLE_EVICTION: Duration = Duration::from_secs(3600);
/// Ceiling on distinct buckets. Reached only under a spray across many addresses;
/// evicting the least recently seen keeps memory bounded at the cost of
/// forgetting the oldest offender first.
const MAX_BUCKETS: usize = 100_000;
/// Lock shards. Sized so a busy Hub spreads unrelated keys across locks while
/// each shard's map stays large enough for the eviction scan to be rare.
const SHARDS: usize = 16;
const MAX_BUCKETS_PER_SHARD: usize = MAX_BUCKETS / SHARDS;

impl RateLimiter {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            shards: Self::new_shards(),
            hasher: RandomState::new(),
            enabled: true,
        }
    }

    /// A limiter that allows everything, for tests and for the legacy
    /// no-database mode where there are no accounts to protect.
    pub fn disabled() -> Self {
        Self {
            limits:  Limits::default(),
            shards:  Self::new_shards(),
            hasher:  RandomState::new(),
            enabled: false,
        }
    }

    fn new_shards() -> Arc<[Mutex<Shard>]> {
        (0..SHARDS).map(|_| Mutex::new(Shard::default())).collect()
    }

    pub const fn limits(&self) -> &Limits {
        &self.limits
    }

    fn hash_of(&self, scope: &str, key: &str) -> u64 {
        self.hasher.hash_one((scope, key))
    }

    /// Uses the high bits: the low bits also pick the slot inside the shard's
    /// own table, so reusing them would correlate shard choice with slot choice.
    fn shard_for(&self, hash: u64) -> &Mutex<Shard> {
        &self.shards[usize::try_from(hash >> 32).unwrap_or(0) % SHARDS]
    }

    pub async fn check(&self, scope: &str, key: &str, quota: Quota) -> Decision {
        if !self.enabled {
            return Decision::Allow;
        }
        let hash = self.hash_of(scope, key);
        let mut shard = self.shard_for(hash).lock().await;
        let now = Instant::now();
        let sequence = shard.next_sequence;
        shard.next_sequence = sequence.wrapping_add(1);

        // An existing caller must never trigger eviction and replenish its own
        // quota just because the map is full. Hits need no allocated lookup
        // key; recency metadata is compacted only at its amortized size bound.
        let decision = if let Some(bucket) = shard.buckets.find_mut(hash, |bucket| {
            &*bucket.scope == scope && &*bucket.key == key
        }) {
            bucket.sequence = sequence;
            bucket.consume(now, quota)
        } else {
            if shard.buckets.len() >= MAX_BUCKETS_PER_SHARD {
                shard.evict_oldest();
            }
            let mut bucket = Bucket {
                scope: scope.into(),
                key: key.into(),
                tokens: quota.capacity,
                last_seen: now,
                sequence,
            };
            let decision = bucket.consume(now, quota);
            shard.buckets.insert_unique(hash, bucket, |bucket| {
                self.hash_of(&bucket.scope, &bucket.key)
            });
            decision
        };
        shard.record_access(sequence, hash);
        decision
    }

    /// Returns tokens after a successful sign-in.
    ///
    /// Without this a user who mistypes twice and then succeeds still carries the
    /// penalty, and a shared address such as an office NAT would throttle
    /// everyone behind it.
    pub async fn reset(&self, scope: &str, key: &str) {
        if !self.enabled {
            return;
        }
        let hash = self.hash_of(scope, key);
        let mut shard = self.shard_for(hash).lock().await;
        if let Ok(entry) = shard.buckets.find_entry(hash, |bucket| {
            &*bucket.scope == scope && &*bucket.key == key
        }) {
            entry.remove();
        }
    }

    /// Drops idle buckets. Called on a timer by the Hub.
    pub async fn evict_idle(&self) -> usize {
        if !self.enabled {
            return 0;
        }
        let now = Instant::now();
        let mut dropped = 0;
        for shard in self.shards.iter() {
            let mut shard = shard.lock().await;
            let before = shard.buckets.len();
            shard
                .buckets
                .retain(|bucket| now.duration_since(bucket.last_seen) < IDLE_EVICTION);
            dropped += before - shard.buckets.len();
        }
        dropped
    }

    #[cfg(test)]
    async fn bucket_count(&self) -> usize {
        let mut total = 0;
        for shard in self.shards.iter() {
            total += shard.lock().await.buckets.len();
        }
        total
    }
}

/// Extracts the client address for rate limiting.
///
/// `X-Forwarded-For` is only honoured when the deployment declares it sits behind
/// a proxy. Trusting it unconditionally would hand every attacker an unlimited
/// supply of identities: one spoofed header per request defeats a per-address
/// limit entirely.
///
/// The rightmost entry is taken because a proxy appends the peer it saw. Entries
/// to its left were supplied by the client and cannot be trusted.
pub fn client_address(
    headers: &axum::http::HeaderMap,
    peer: Option<IpAddr>,
    trust_forwarded_for: bool,
) -> String {
    if trust_forwarded_for
        && let Some(forwarded) = headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
        && let Some(address) = forwarded
            .split(',')
            .map(str::trim)
            .rfind(|value| !value.is_empty())
    {
        return address.to_owned();
    }
    peer.map(|address| address.to_string())
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    #[tokio::test]
    async fn a_full_shard_does_not_evict_or_refill_an_existing_caller() {
        let limiter = RateLimiter::new(Limits::default());
        let hash = limiter.hash_of("test", "victim");
        {
            let mut shard = limiter.shard_for(hash).lock().await;
            for index in 0..MAX_BUCKETS_PER_SHARD {
                let key = if index == 0 {
                    "victim".to_owned()
                } else {
                    format!("key-{index}")
                };
                let entry_hash = limiter.hash_of("test", &key);
                shard.buckets.insert_unique(
                    entry_hash,
                    Bucket {
                        scope:     "test".into(),
                        key:       key.into(),
                        tokens:    0.0,
                        last_seen: Instant::now(),
                        sequence:  index as u64,
                    },
                    |bucket| limiter.hash_of(&bucket.scope, &bucket.key),
                );
                shard.order.push_back((index as u64, entry_hash));
            }
            shard.next_sequence = MAX_BUCKETS_PER_SHARD as u64;
        }
        assert!(
            !limiter
                .check("test", "victim", Quota::new(1, 0.0))
                .await
                .is_allowed()
        );
        assert_eq!(
            limiter.shard_for(hash).lock().await.buckets.len(),
            MAX_BUCKETS_PER_SHARD
        );
    }

    #[tokio::test]
    async fn hot_keys_keep_the_recency_queue_bounded() {
        let limiter = RateLimiter::new(Limits::default());
        for _ in 0..MAX_BUCKETS_PER_SHARD * 3 {
            limiter.check("scope", "hot", Quota::new(1, 0.0)).await;
        }
        let hash = limiter.hash_of("scope", "hot");
        let shard = limiter.shard_for(hash).lock().await;
        assert_eq!(shard.buckets.len(), 1);
        assert!(shard.order.len() <= 2 * MAX_BUCKETS_PER_SHARD);
    }

    #[tokio::test]
    async fn a_bucket_allows_its_capacity_then_denies() {
        let limiter = RateLimiter::new(Limits::default());
        let quota = Quota::new(3, 0.0);

        for attempt in 1..=3 {
            assert!(
                limiter.check("login", "1.2.3.4", quota).await.is_allowed(),
                "attempt {attempt} should be allowed"
            );
        }
        let decision = limiter.check("login", "1.2.3.4", quota).await;
        assert!(!decision.is_allowed(), "capacity must be enforced");
        // A denial has to tell the caller when to come back.
        match decision {
            Decision::Deny {
                retry_after_seconds,
            } => assert!(retry_after_seconds >= 1),
            Decision::Allow => unreachable!(),
        }
    }

    #[tokio::test]
    async fn scopes_and_keys_are_independent() {
        let limiter = RateLimiter::new(Limits::default());
        let quota = Quota::new(1, 0.0);

        assert!(limiter.check("login", "a", quota).await.is_allowed());
        assert!(!limiter.check("login", "a", quota).await.is_allowed());
        // A different address is unaffected.
        assert!(limiter.check("login", "b", quota).await.is_allowed());
        // So is a different endpoint for the same address.
        assert!(limiter.check("register", "a", quota).await.is_allowed());
    }

    #[tokio::test]
    async fn tokens_refill_over_time() {
        let limiter = RateLimiter::new(Limits::default());
        // 1 token capacity, refilling fast enough to observe.
        let quota = Quota::new(1, 100.0);

        assert!(limiter.check("login", "ip", quota).await.is_allowed());
        assert!(!limiter.check("login", "ip", quota).await.is_allowed());
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            limiter.check("login", "ip", quota).await.is_allowed(),
            "a refilled bucket must allow again"
        );
    }

    #[tokio::test]
    async fn a_successful_sign_in_clears_the_penalty() {
        let limiter = RateLimiter::new(Limits::default());
        let quota = Quota::new(2, 0.0);

        assert!(limiter.check("login", "ip", quota).await.is_allowed());
        assert!(limiter.check("login", "ip", quota).await.is_allowed());
        assert!(!limiter.check("login", "ip", quota).await.is_allowed());

        // Two typos followed by a success must not leave the user throttled, and
        // a shared office address must not punish everyone behind it.
        limiter.reset("login", "ip").await;
        assert!(limiter.check("login", "ip", quota).await.is_allowed());
    }

    #[tokio::test]
    async fn a_disabled_limiter_allows_everything() {
        let limiter = RateLimiter::disabled();
        let quota = Quota::new(1, 0.0);
        for _ in 0..100 {
            assert!(limiter.check("login", "ip", quota).await.is_allowed());
        }
        assert_eq!(limiter.bucket_count().await, 0, "must not accumulate state");
    }

    #[tokio::test]
    async fn idle_buckets_are_evicted() {
        let limiter = RateLimiter::new(Limits::default());
        limiter.check("login", "ip", Quota::new(1, 0.0)).await;
        assert_eq!(limiter.bucket_count().await, 1);
        // Nothing is idle yet.
        assert_eq!(limiter.evict_idle().await, 0);
        assert_eq!(limiter.bucket_count().await, 1);
    }

    #[test]
    fn forwarded_for_is_ignored_unless_the_proxy_is_trusted() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.9, 198.51.100.4"),
        );
        let peer: IpAddr = "10.0.0.1".parse().unwrap();

        // Untrusted: a spoofed header would otherwise mint a fresh identity per
        // request and defeat the limit entirely.
        assert_eq!(
            client_address(&headers, Some(peer), false),
            "10.0.0.1",
            "the header must be ignored when no proxy is declared"
        );

        // Trusted: take the rightmost entry, which the proxy appended. Anything
        // to its left came from the client.
        assert_eq!(client_address(&headers, Some(peer), true), "198.51.100.4");
    }

    #[test]
    fn a_missing_address_still_produces_a_key() {
        // Better to throttle every unknown-peer request together than to skip
        // the check.
        assert_eq!(client_address(&HeaderMap::new(), None, true), "unknown");
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("   "));
        assert_eq!(client_address(&headers, None, true), "unknown");
    }
}
