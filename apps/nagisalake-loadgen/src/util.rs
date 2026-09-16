//! Small time helpers shared by the load and mock-worker paths.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub(crate) fn elapsed_micros(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

pub(crate) fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
