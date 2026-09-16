//! Manifest generation from the workflow config, normalized template, and
//! best-effort UI metadata.

use crate::{
    catalog::WorkflowConfig,
    normalize::UiInputMetadata,
    pointer::{pointer_field, pointer_node_id},
};
use nagisalake_protocol::{WorkflowInput, WorkflowInputKind, WorkflowManifest, WorkflowOutput};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;

pub(crate) fn build_manifest(
    config: &WorkflowConfig,
    template: &JsonValue,
    ui_inputs: &BTreeMap<String, UiInputMetadata>,
    mut warnings: Vec<String>,
) -> WorkflowManifest {
    let mut inputs = Vec::with_capacity(config.parameters.len() + config.inputs.len());
    for (name, pointer) in &config.parameters {
        let metadata = ui_inputs.get(pointer);
        let default = template
            .pointer(pointer)
            .filter(|value| !value.is_null())
            .cloned();
        inputs.push(WorkflowInput {
            name: name.clone(),
            kind: WorkflowInputKind::Parameter,
            value_type: metadata
                .map(|metadata| public_value_type(&metadata.input_type))
                .unwrap_or_else(|| infer_value_type(default.as_ref())),
            content_type: None,
            pointer: pointer.clone(),
            required: default.is_none(),
            default,
            options: Vec::new(),
            node_id: pointer_node_id(pointer),
            node_type: metadata.map(|metadata| metadata.node_type.clone()),
            field: pointer_field(pointer),
        });
    }
    for binding in &config.inputs {
        let metadata = ui_inputs.get(&binding.pointer);
        let value_type = binding
            .content_type
            .as_deref()
            .and_then(artifact_type_for_content_type)
            .map(str::to_string)
            .or_else(|| metadata.map(|metadata| public_artifact_type(&metadata.input_type)))
            .unwrap_or_else(|| "artifact".into());
        inputs.push(WorkflowInput {
            name: binding
                .name
                .clone()
                .or_else(|| pointer_field(&binding.pointer))
                .unwrap_or_else(|| format!("input_{}", binding.index)),
            kind: WorkflowInputKind::Artifact,
            content_type: binding
                .content_type
                .clone()
                .or_else(|| content_type_for_input_type(&value_type).map(str::to_string)),
            value_type,
            pointer: binding.pointer.clone(),
            required: true,
            default: None,
            options: Vec::new(),
            node_id: pointer_node_id(&binding.pointer),
            node_type: metadata.map(|metadata| metadata.node_type.clone()),
            field: pointer_field(&binding.pointer),
        });
    }
    let outputs = config
        .output_types
        .iter()
        .enumerate()
        .map(|(index, content_type)| WorkflowOutput {
            name:         format!("output_{index}"),
            content_type: content_type.clone(),
        })
        .collect();
    if config.output_types.is_empty() {
        warnings.push("workflow has no declared output_types".into());
    }
    WorkflowManifest {
        schema_version: 1,
        display_name: config.id.clone(),
        description: None,
        inputs,
        outputs,
        warnings: {
            warnings.sort();
            warnings.dedup();
            warnings
        },
    }
}

fn infer_value_type(value: Option<&JsonValue>) -> String {
    match value {
        Some(JsonValue::Bool(_)) => "boolean",
        Some(JsonValue::Number(number)) if number.is_i64() || number.is_u64() => "integer",
        Some(JsonValue::Number(_)) => "number",
        Some(JsonValue::String(_)) => "string",
        Some(JsonValue::Array(_)) => "array",
        Some(JsonValue::Object(_)) => "object",
        _ => "unknown",
    }
    .into()
}

fn public_value_type(input_type: &str) -> String {
    match input_type.to_ascii_uppercase().as_str() {
        "INT" => "integer".into(),
        "FLOAT" => "number".into(),
        "BOOLEAN" => "boolean".into(),
        "COMBO" => "enum".into(),
        "STRING" => "string".into(),
        other => other.to_ascii_lowercase().replace([' ', '-'], "_"),
    }
}

fn public_artifact_type(input_type: &str) -> String {
    match input_type.to_ascii_uppercase().as_str() {
        "IMAGE" => "image".into(),
        "MASK" => "mask".into(),
        "VIDEO" | "VHS_FILENAMES" => "video".into(),
        "AUDIO" => "audio".into(),
        other => other.to_ascii_lowercase().replace([' ', '-'], "_"),
    }
}

fn content_type_for_input_type(value_type: &str) -> Option<&'static str> {
    match value_type {
        "image" => Some("image/*"),
        "mask" => Some("image/*"),
        "video" => Some("video/*"),
        "audio" => Some("audio/*"),
        _ => None,
    }
}

fn artifact_type_for_content_type(content_type: &str) -> Option<&'static str> {
    match content_type
        .split_once('/')
        .map(|(kind, _)| kind.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("image") => Some("image"),
        Some("video") => Some("video"),
        Some("audio") => Some("audio"),
        _ => None,
    }
}
