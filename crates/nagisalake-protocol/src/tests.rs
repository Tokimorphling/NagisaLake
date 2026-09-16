use crate::{
    messages::{
        DispatchJob, HubMessage, JobInput, MAX_RECOVERY_JOB_IDS, PROTOCOL_VERSION,
        PresignedRequest, Register, WorkerCapabilities, WorkflowCapability,
    },
    validate::{MAX_IDENTITY_CHARS, Validate},
};
use std::collections::BTreeMap;

fn register(namespace: &str, node_name: &str) -> Register {
    Register {
        protocol_version: PROTOCOL_VERSION,
        namespace:        namespace.into(),
        node_name:        node_name.into(),
        worker_version:   "0.1.0".into(),
        capabilities:     WorkerCapabilities {
            workflows: vec![WorkflowCapability {
                id:           "portrait".into(),
                version:      "v1".into(),
                output_types: vec!["image/png".into()],
                manifest:     None,
            }],
            parallelism: 1,
            queue_depth: 0,
            supports_queued_job_cancellation: false,
            labels: BTreeMap::new(),
        },
        recovery_job_ids: Vec::new(),
    }
}

/// Worker ids are built by joining namespace and node_name, so two distinct
/// identities must never be able to produce the same id. Sanitising would
/// collapse `a_b` and `a/b` together; validation has to reject instead.
#[test]
fn worker_identities_cannot_collide_after_sanitisation() {
    assert!(register("home-gpu", "comfyui-01").validate().is_ok());
    assert!(register("home_gpu", "comfy.ui_01").validate().is_ok());

    // Each of these used to sanitise down to an already-valid identity.
    for (namespace, node_name, why) in [
        ("home-gpu", "comfyui/01", "slash would become an underscore"),
        (
            "home-gpu",
            "comfyui?01",
            "question mark would become an underscore",
        ),
        ("home-gpu", "comfyui 01", "space would become an underscore"),
        ("home/gpu", "comfyui-01", "slash in the namespace"),
        ("home-gpu", "comfyui:01", "colon would become an underscore"),
    ] {
        let error = register(namespace, node_name)
            .validate()
            .expect_err(&format!("{namespace}/{node_name} must be rejected: {why}"));
        assert!(
            error.to_string().contains("ASCII letters"),
            "unexpected error for {namespace}/{node_name}: {error}"
        );
    }

    // Empty and over-long identities stay rejected.
    assert!(register("", "comfyui-01").validate().is_err());
    assert!(register("home-gpu", "").validate().is_err());
    assert!(
        register("home-gpu", &"a".repeat(MAX_IDENTITY_CHARS + 1))
            .validate()
            .is_err()
    );
    assert!(
        register("home-gpu", &"a".repeat(MAX_IDENTITY_CHARS))
            .validate()
            .is_ok()
    );

    // A leading dot would read as a relative path segment.
    assert!(register("home-gpu", ".hidden").validate().is_err());
}

#[test]
fn recovery_inventory_is_bounded_and_has_unique_nonempty_ids() {
    let mut value = register("home-gpu", "comfyui-01");
    value.recovery_job_ids = vec!["job-a".into(), "job-b".into()];
    assert!(value.validate().is_ok());

    value.recovery_job_ids = vec!["job-a".into(), "job-a".into()];
    assert!(value.validate().is_err());

    value.recovery_job_ids = vec![" ".into()];
    assert!(value.validate().is_err());

    value.recovery_job_ids = (0..=MAX_RECOVERY_JOB_IDS)
        .map(|index| format!("job-{index}"))
        .collect();
    assert!(value.validate().is_err());
}

fn dispatch() -> DispatchJob {
    DispatchJob {
        command_id:       "command-1".into(),
        job_id:           "job-1".into(),
        attempt:          1,
        workflow_id:      "portrait".into(),
        workflow_version: "v1".into(),
        parameters:       serde_json::json!({"prompt": "hello"}),
        inputs:           vec![JobInput {
            artifact_id:  "input-1".into(),
            name:         "source.png".into(),
            content_type: "image/png".into(),
            size_bytes:   42,
            sha256:       "a".repeat(64),
            download:     PresignedRequest {
                method:             "GET".into(),
                url:                "https://objects.example/input".into(),
                headers:            BTreeMap::new(),
                expires_at_unix_ms: 100,
            },
        }],
    }
}

#[test]
fn dispatch_round_trips_without_model_or_binary_body() {
    let message = HubMessage::DispatchJob(dispatch());
    let json = serde_json::to_string(&message).expect("serialize");
    assert!(!json.contains("\"model\""));
    assert!(!json.contains("body"));
    assert_eq!(serde_json::from_str::<HubMessage>(&json).unwrap(), message);
    message.validate().unwrap();
}

#[test]
fn rejects_invalid_artifact_hash() {
    let mut message = dispatch();
    message.inputs[0].sha256 = "not-a-hash".into();
    let error = message.validate().unwrap_err();
    assert_eq!(error.field, "input.sha256");
}

#[test]
fn rejects_duplicate_workflow_capabilities() {
    let capability = WorkflowCapability {
        id:           "portrait".into(),
        version:      "v1".into(),
        output_types: vec!["image/png".into()],
        manifest:     None,
    };
    let capabilities = WorkerCapabilities {
        workflows: vec![capability.clone(), capability],
        parallelism: 1,
        queue_depth: 0,
        supports_queued_job_cancellation: false,
        labels: BTreeMap::new(),
    };
    assert!(capabilities.validate().is_err());
}

#[test]
fn worker_capabilities_keep_the_v2_concurrency_wire_field() {
    let capabilities: WorkerCapabilities = serde_json::from_value(serde_json::json!({
        "workflows": [{"id": "portrait", "version": "v1"}],
        "concurrency": 3,
        "labels": {}
    }))
    .unwrap();

    assert_eq!(capabilities.parallelism, 3);
    assert_eq!(capabilities.queue_depth, 0);
    assert!(!capabilities.supports_queued_job_cancellation);
    assert_eq!(capabilities.total_capacity(), 3);

    let encoded = serde_json::to_value(&capabilities).unwrap();
    assert_eq!(encoded["concurrency"], 3);
    assert!(encoded.get("parallelism").is_none());
}

#[test]
fn worker_capabilities_include_queue_capacity() {
    let capabilities = WorkerCapabilities {
        parallelism: 2,
        queue_depth: 8,
        supports_queued_job_cancellation: true,
        ..register("home", "gpu").capabilities
    };

    assert_eq!(capabilities.total_capacity(), 10);
    capabilities.validate().unwrap();
}
