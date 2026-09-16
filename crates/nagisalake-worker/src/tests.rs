use crate::{
    config::WorkerConfig,
    error::WorkerError,
    worker::{now_unix_ms, prepare_sqlite_parent},
};
use std::{env, path::Path};

#[test]
fn resolves_relative_sqlite_urls_from_config_directory() {
    // Build the expected value from the same platform primitives instead of
    // hardcoding a separator, then assert the URL uses '/' regardless.
    let base = Path::new("/config");
    let expected = format!(
        "sqlite://{}",
        base.join("state/worker.db")
            .display()
            .to_string()
            .replace('\\', "/")
    );
    let resolved = crate::config::resolve_sqlite_url("sqlite://state/worker.db", base);
    assert_eq!(resolved, expected);
    assert!(
        !resolved.contains('\\'),
        "a SQLite URI must not carry backslash separators: {resolved}"
    );
    assert!(resolved.ends_with("/state/worker.db"), "{resolved}");

    // Already-final forms pass through untouched.
    assert_eq!(
        crate::config::resolve_sqlite_url("sqlite::memory:", base),
        "sqlite::memory:"
    );
    assert_eq!(
        crate::config::resolve_sqlite_url("sqlite://:memory:", base),
        "sqlite://:memory:"
    );
    assert_eq!(
        crate::config::resolve_sqlite_url("postgres://localhost/db", base),
        "postgres://localhost/db"
    );
}

#[test]
fn example_worker_config_is_parseable() {
    let config: WorkerConfig =
        toml::from_str(include_str!("../../../examples/nagisalake-worker.toml")).unwrap();
    assert_eq!(config.worker.node_name, "comfyui-01");
    assert_eq!(config.workflows.len(), 1);
}

#[tokio::test]
async fn creates_sqlite_parent_directory() {
    let directory = env::temp_dir().join(format!(
        "nagisalake-worker-test-{}-{}",
        std::process::id(),
        now_unix_ms()
    ));
    let database = directory.join("nested/state.db");
    prepare_sqlite_parent(&format!("sqlite://{}", database.display()))
        .await
        .unwrap();
    assert!(database.parent().unwrap().is_dir());
    tokio::fs::remove_dir_all(directory).await.unwrap();
}

#[test]
fn resolves_relative_ca_bundles_from_the_config_directory() {
    let directory = env::temp_dir().join(format!(
        "nagisalake-worker-tls-{}-{}",
        std::process::id(),
        now_unix_ms()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("worker.toml");
    std::fs::write(
        &path,
        tls_config_toml("wss://hub.example.com/v1/worker/connect"),
    )
    .unwrap();

    let config = WorkerConfig::load(&path).unwrap();
    // The worker's cwd is wherever systemd or the operator happened to start
    // it, so a relative bundle has to anchor on the config file like
    // `work_dir` and the workflow files do.
    assert_eq!(config.hub.tls.ca_certificates, vec![
        directory.join("tls/hub-ca.pem")
    ]);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_hub_url_that_cannot_be_dialled_is_rejected_before_connecting() {
    // Each of these would otherwise loop forever behind the reconnect
    // backoff: the console's scheme, the API's scheme, and a bare host.
    for url in [
        "https://hub.example.com/v1/worker/connect",
        "http://127.0.0.1:9091/v1/worker/connect",
        "hub.example.com/v1/worker/connect",
    ] {
        let config: WorkerConfig = toml::from_str(&tls_config_toml(url)).unwrap();
        let error = config.validate().unwrap_err();
        assert!(
            matches!(&error, WorkerError::InvalidConfig(message)
                if message.contains("ws:// or wss://")),
            "{url} should be rejected, got {error:?}"
        );
    }
}

#[test]
fn ca_bundles_on_a_cleartext_hub_url_are_rejected() {
    // Trust material plus `ws://` means someone believes the connection is
    // encrypted when nothing about it is.
    let config: WorkerConfig =
        toml::from_str(&tls_config_toml("ws://127.0.0.1:9091/v1/worker/connect")).unwrap();
    let error = config.validate().unwrap_err();
    assert!(
        matches!(&error, WorkerError::InvalidConfig(message)
            if message.contains("hub.tls.ca_certificates")),
        "{error:?}"
    );

    // The same url without a bundle is the ordinary LAN setup.
    let mut config = config;
    config.hub.tls.ca_certificates.clear();
    config.validate().unwrap();
}

/// A minimal worker config carrying one relative CA bundle.
fn tls_config_toml(url: &str) -> String {
    format!(
        r#"
work_dir = "./work"

[hub]
url = "{url}"
token = "development-worker-token"

[hub.tls]
ca_certificates = ["tls/hub-ca.pem"]

[worker]
namespace = "home-gpu"
node_name = "comfyui-01"

[[workflows]]
id = "sdxl-txt2img"
version = "v1"
file = "./workflows/sdxl-txt2img-api.json"
output_types = ["image/png"]
"#
    )
}
