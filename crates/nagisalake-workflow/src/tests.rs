use crate::{
    catalog::{InputBinding, WorkflowCatalog, WorkflowConfig},
    error::WorkflowError,
    normalize::normalize_workflow,
};
use nagisalake_protocol::{DispatchJob, WorkflowInputKind};
use serde_json::{Value as JsonValue, json};
use std::{collections::BTreeMap, path::PathBuf};

fn catalog() -> WorkflowCatalog {
    WorkflowCatalog::from_templates([(
        WorkflowConfig {
            id:           "image-edit".into(),
            version:      "v1".into(),
            file:         PathBuf::new(),
            output_types: vec!["image/png".into()],
            parameters:   BTreeMap::from([("prompt".into(), "/6/inputs/text".into())]),
            inputs:       vec![InputBinding {
                index:        0,
                pointer:      "/10/inputs/image".into(),
                name:         None,
                content_type: None,
            }],
        },
        serde_json::json!({
            "6":{"inputs":{"text":"default"}},
            "10":{"inputs":{"image":"default.png"}}
        }),
    )])
    .unwrap()
}

fn dispatch(parameters: JsonValue) -> DispatchJob {
    DispatchJob {
        command_id: "command".into(),
        job_id: "job".into(),
        attempt: 1,
        workflow_id: "image-edit".into(),
        workflow_version: "v1".into(),
        parameters,
        inputs: vec![nagisalake_protocol::JobInput {
            artifact_id:  "input".into(),
            name:         "source.png".into(),
            content_type: "image/png".into(),
            size_bytes:   1,
            sha256:       "0".repeat(64),
            download:     nagisalake_protocol::PresignedRequest {
                method:             "GET".into(),
                url:                "https://example.invalid/input".into(),
                headers:            BTreeMap::new(),
                expires_at_unix_ms: 1,
            },
        }],
    }
}

#[test]
fn renders_only_allowlisted_bindings() {
    let rendered = catalog()
        .render(&dispatch(serde_json::json!({"prompt":"safe prompt"})), &[
            "uploaded.png".into(),
        ])
        .unwrap();
    assert_eq!(rendered.pointer("/6/inputs/text").unwrap(), "safe prompt");
    assert_eq!(
        rendered.pointer("/10/inputs/image").unwrap(),
        "uploaded.png"
    );
}

#[test]
fn rejects_unknown_parameters() {
    let error = catalog()
        .render(
            &dispatch(serde_json::json!({"arbitrary_node":"attack"})),
            &["uploaded.png".into()],
        )
        .unwrap_err();
    assert!(matches!(error, WorkflowError::UnknownParameter(_)));
}

#[test]
fn rejects_overlapping_parameter_and_input_bindings() {
    let result = WorkflowCatalog::from_templates([(
        WorkflowConfig {
            id:           "unsafe".into(),
            version:      "v1".into(),
            file:         PathBuf::new(),
            output_types: vec![],
            parameters:   BTreeMap::from([("node".into(), "/6/inputs".into())]),
            inputs:       vec![InputBinding {
                index:        0,
                pointer:      "/6/inputs/image".into(),
                name:         None,
                content_type: None,
            }],
        },
        serde_json::json!({"6":{"inputs":{"image":"default.png"}}}),
    )]);

    assert!(matches!(
        result,
        Err(WorkflowError::OverlappingPointers { .. })
    ));
}

#[test]
fn generated_manifest_describes_allowlisted_contract() {
    let config = WorkflowConfig {
        id:           "image-edit".into(),
        version:      "v1".into(),
        file:         PathBuf::new(),
        output_types: vec!["image/png".into()],
        parameters:   BTreeMap::from([("prompt".into(), "/6/inputs/text".into())]),
        inputs:       vec![InputBinding {
            index:        0,
            pointer:      "/10/inputs/image".into(),
            name:         None,
            content_type: None,
        }],
    };
    let catalog = WorkflowCatalog::from_templates([(
        config,
        serde_json::json!({
            "6": {"class_type":"CLIPTextEncode", "inputs":{"text":"default"}},
            "10": {"class_type":"LoadImage", "inputs":{"image":"placeholder.png"}}
        }),
    )])
    .unwrap();
    let manifest = catalog.capabilities().pop().unwrap().manifest.unwrap();
    assert_eq!(manifest.display_name, "image-edit");
    assert_eq!(manifest.inputs[0].name, "prompt");
    assert_eq!(manifest.inputs[0].value_type, "string");
    assert!(!manifest.inputs[0].required);
    assert_eq!(manifest.inputs[1].name, "image");
    assert_eq!(manifest.inputs[1].kind, WorkflowInputKind::Artifact);
    assert_eq!(manifest.outputs[0].content_type, "image/png");
}

#[test]
fn ui_workflow_is_normalized_for_manifest_generation_when_fixture_exists() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_workflows/scail2MultiRefSegmented_v6.json");
    if !path.exists() {
        return;
    }
    let catalog = WorkflowCatalog::load(&[WorkflowConfig {
        id:           "scail2".into(),
        version:      "v6".into(),
        file:         path,
        output_types: vec!["video/mp4".into()],
        parameters:   BTreeMap::from([("negative_prompt".into(), "/22/inputs/text".into())]),
        inputs:       vec![InputBinding {
            index:        0,
            pointer:      "/67/inputs/video".into(),
            name:         Some("source_video".into()),
            content_type: Some("video/*".into()),
        }],
    }])
    .unwrap();
    let manifest = catalog.capabilities().pop().unwrap().manifest.unwrap();
    assert!(
        manifest
            .warnings
            .iter()
            .any(|warning| warning.contains("editor format"))
    );
    assert_eq!(manifest.inputs[0].value_type, "string");
    assert_eq!(manifest.inputs[1].value_type, "video");
    assert_eq!(manifest.inputs[1].content_type.as_deref(), Some("video/*"));
    assert_eq!(
        manifest.inputs[1].node_type.as_deref(),
        Some("VHS_LoadVideo")
    );
}

#[test]
fn ui_widget_defaults_handle_auxiliary_arrays_and_named_objects() {
    let normalized = normalize_workflow(serde_json::json!({
        "nodes": [
            {
                "id": 1,
                "type": "KSampler",
                "inputs": [
                    {"name":"seed", "type":"INT", "link":null, "widget":{"name":"seed"}},
                    {"name":"steps", "type":"INT", "link":null, "widget":{"name":"steps"}},
                    {"name":"cfg", "type":"FLOAT", "link":null, "widget":{"name":"cfg"}},
                    {"name":"sampler_name", "type":"COMBO", "link":null, "widget":{"name":"sampler_name"}}
                ],
                "widgets_values": [123, "randomize", 20, 7.5, "euler"]
            },
            {
                "id": 2,
                "type": "VHS_LoadVideo",
                "inputs": [
                    {"name":"video", "type":"COMBO", "link":null, "widget":{"name":"video"}}
                ],
                "widgets_values": {"video":"input.mp4", "videopreview":{"paused":false}}
            }
        ],
        "links": []
    }))
    .unwrap();

    assert_eq!(
        normalized.template.pointer("/1/inputs/seed"),
        Some(&json!(123))
    );
    assert_eq!(
        normalized.template.pointer("/1/inputs/steps"),
        Some(&json!(20))
    );
    assert_eq!(
        normalized.template.pointer("/1/inputs/cfg"),
        Some(&json!(7.5))
    );
    assert_eq!(
        normalized.template.pointer("/1/inputs/sampler_name"),
        Some(&json!("euler"))
    );
    assert_eq!(
        normalized.template.pointer("/2/inputs/video"),
        Some(&json!("input.mp4"))
    );
    assert!(
        normalized
            .warnings
            .iter()
            .any(|warning| warning.contains("skipped 1 auxiliary"))
    );
}

#[test]
fn all_ui_workflow_fixtures_generate_manifests_without_execution() {
    let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test_workflows");
    if !fixture_dir.exists() {
        return;
    }
    let mut paths = std::fs::read_dir(&fixture_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();
    assert!(!paths.is_empty(), "expected at least one workflow fixture");

    let configs = paths
        .iter()
        .enumerate()
        .map(|(index, path)| WorkflowConfig {
            id:           format!("fixture-{index}"),
            version:      "mock".into(),
            file:         path.clone(),
            output_types: Vec::new(),
            parameters:   BTreeMap::new(),
            inputs:       Vec::new(),
        })
        .collect::<Vec<_>>();
    let catalog = WorkflowCatalog::load(&configs).unwrap();
    let capabilities = catalog.capabilities();
    assert_eq!(capabilities.len(), paths.len());
    assert!(capabilities.iter().all(|capability| {
        capability.manifest.as_ref().is_some_and(|manifest| {
            manifest.schema_version == 1
                && manifest
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("editor format"))
        })
    }));
}
