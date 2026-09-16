use crate::{
    password::{DUMMY_PASSWORD_HASH, hash_password, verify_password, verify_password_async},
    principal::{Permission, Principal, PrincipalKind, Role},
    secret::{generate_secret, verify_secret},
};
use argon2::PasswordHash;
use std::collections::BTreeSet;

/// The dummy hash only equalises login timing if it actually parses. A
/// malformed value makes `verify_password` return before Argon2 runs, which
/// silently reopens the account enumeration oracle.
#[test]
fn dummy_password_hash_is_a_usable_argon2_hash() {
    let parsed = PasswordHash::new(DUMMY_PASSWORD_HASH)
        .expect("dummy hash must parse, or no Argon2 work happens");
    assert_eq!(parsed.algorithm.as_str(), "argon2id");
    // Wrong password against a well-formed hash: rejected, but only after a
    // full verification.
    assert!(!verify_password("anything", DUMMY_PASSWORD_HASH));
}

/// A missing account must consume comparable CPU to a wrong password.
#[tokio::test]
async fn unknown_accounts_still_pay_for_a_verification() {
    let real = hash_password("correct horse battery staple").unwrap();

    let wrong_password = std::time::Instant::now();
    assert!(!verify_password_async("wrong".into(), Some(real)).await);
    let wrong_password = wrong_password.elapsed();

    let unknown_account = std::time::Instant::now();
    assert!(!verify_password_async("wrong".into(), None).await);
    let unknown_account = unknown_account.elapsed();

    // Same order of magnitude. A skipped Argon2 would be 10x+ faster, so
    // this catches a regression without being flaky on a loaded machine.
    let ratio = wrong_password.as_secs_f64() / unknown_account.as_secs_f64().max(1e-9);
    assert!(
        (0.2..=5.0).contains(&ratio),
        "timing should be comparable, got {ratio:.2}x (known {wrong_password:?} vs unknown \
         {unknown_account:?})"
    );
}

#[test]
fn passwords_use_salted_argon2_hashes() {
    let first = hash_password("correct horse battery staple").unwrap();
    let second = hash_password("correct horse battery staple").unwrap();
    assert_ne!(first, second);
    assert!(verify_password("correct horse battery staple", &first));
    assert!(!verify_password("incorrect horse battery staple", &first));
}

#[test]
fn secrets_are_prefixed_hashed_and_verifiable() {
    let key = generate_secret("nsk");
    assert!(key.plaintext.starts_with("nsk_"));
    assert!(!key.hash.contains(&key.plaintext));
    assert!(verify_secret(&key.plaintext, &key.hash));
    assert!(!verify_secret("nsk_wrong", &key.hash));
}

#[test]
fn api_key_scope_cannot_bypass_role() {
    let principal = Principal {
        kind:            PrincipalKind::ApiKey,
        actor_id:        "key".into(),
        user_id:         Some("user".into()),
        organization_id: "org".into(),
        role:            Role::Member,
        scopes:          BTreeSet::from(["members:manage".into(), "jobs:write".into()]),
    };
    assert!(principal.allows(Permission::JobsWrite));
    assert!(!principal.allows(Permission::MembersManage));
}

#[test]
fn role_matrix_matches_product_policy() {
    assert!(Role::Viewer.allows(Permission::WorkflowsRead));
    assert!(!Role::Viewer.allows(Permission::JobsWrite));
    assert!(Role::Operator.allows(Permission::WorkersManage));
    assert!(!Role::Operator.allows(Permission::MembersManage));
    assert!(Role::Admin.allows(Permission::AuditRead));
    assert!(!Role::Admin.allows(Permission::OrganizationDelete));
    assert!(Role::Owner.allows(Permission::OrganizationDelete));
}
