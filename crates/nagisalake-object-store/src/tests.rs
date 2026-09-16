use crate::{
    config::{S3ObjectStoreConfig, default_presign_ttl_seconds, default_region},
    s3::validate_key,
};

#[test]
fn rejects_unsafe_keys() {
    assert!(validate_key("media/user/object.png").is_ok());
    assert!(validate_key("/absolute").is_err());
    assert!(validate_key("media/../secret").is_err());
}

#[test]
fn debug_output_redacts_static_credentials() {
    let config = S3ObjectStoreConfig {
        bucket:                "private".into(),
        region:                default_region(),
        endpoint_url:          None,
        access_key_id:         Some("visible-access-key".into()),
        access_key_id_env:     None,
        secret_access_key:     Some("visible-secret-key".into()),
        secret_access_key_env: None,
        session_token:         Some("visible-session-token".into()),
        session_token_env:     None,
        force_path_style:      true,
        presign_ttl_seconds:   default_presign_ttl_seconds(),
    };

    let output = format!("{config:?}");
    assert!(!output.contains("visible-access-key"));
    assert!(!output.contains("visible-secret-key"));
    assert!(!output.contains("visible-session-token"));
    assert!(output.contains("[redacted]"));
}
