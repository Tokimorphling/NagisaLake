//! ComfyUI editor-format normalization.
//!
//! Normal ComfyUI saves an editor graph, not the `/prompt` API-format JSON the
//! worker executes. This is a best-effort static conversion that also produces
//! manifest warnings, so production execution should still prefer an
//! API-format export.

use crate::{error::WorkflowError, pointer::escape_pointer_segment};
use serde_json::{Map, Value as JsonValue, json};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Default)]
pub(crate) struct NormalizedWorkflow {
    pub(crate) template:  JsonValue,
    pub(crate) ui_inputs: BTreeMap<String, UiInputMetadata>,
    pub(crate) warnings:  Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct UiInputMetadata {
    pub(crate) input_type: String,
    pub(crate) node_type:  String,
}

pub(crate) fn normalize_workflow(raw: JsonValue) -> Result<NormalizedWorkflow, WorkflowError> {
    if raw.get("nodes").is_none() {
        return Ok(NormalizedWorkflow {
            template: raw,
            ..NormalizedWorkflow::default()
        });
    }
    let nodes = raw
        .get("nodes")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| WorkflowError::InvalidUiWorkflow("nodes must be an array".into()))?;
    let mut links = HashMap::new();
    for link in raw
        .get("links")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
    {
        let values = link
            .as_array()
            .ok_or_else(|| WorkflowError::InvalidUiWorkflow("link must be an array".into()))?;
        if values.len() < 5 {
            return Err(WorkflowError::InvalidUiWorkflow(
                "link must contain id, origin node, and origin slot".into(),
            ));
        }
        let Some(link_id) = values[0].as_u64() else {
            continue;
        };
        let Some(origin_node) = values[1].as_u64() else {
            continue;
        };
        let Some(origin_slot) = values[2].as_u64() else {
            continue;
        };
        links.insert(link_id, (origin_node, origin_slot));
    }

    let mut template = Map::new();
    let mut ui_inputs = BTreeMap::new();
    let mut warnings = vec![
        "workflow was converted from ComfyUI editor format; verify widget defaults and exposed \
         bindings"
            .into(),
    ];
    for node in nodes {
        let node_id = node_id(node)?;
        let node_type = node
            .get("type")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| WorkflowError::InvalidUiWorkflow("node type is required".into()))?;
        let mut inputs = Map::new();
        let widgets = node.get("widgets_values");
        let mut widget_index = 0usize;
        for input in node
            .get("inputs")
            .and_then(JsonValue::as_array)
            .into_iter()
            .flatten()
        {
            let name = input
                .get("name")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    WorkflowError::InvalidUiWorkflow("node input name is required".into())
                })?;
            let pointer = format!(
                "/{}/inputs/{}",
                escape_pointer_segment(&node_id),
                escape_pointer_segment(name)
            );
            ui_inputs.insert(pointer, UiInputMetadata {
                input_type: input
                    .get("type")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("UNKNOWN")
                    .into(),
                node_type:  node_type.into(),
            });
            if let Some(link_id) = input.get("link").and_then(JsonValue::as_u64) {
                let Some((origin_node, origin_slot)) = links.get(&link_id).copied() else {
                    warnings.push(format!(
                        "node {node_id} input {name} references unknown link {link_id}"
                    ));
                    continue;
                };
                inputs.insert(name.into(), json!([origin_node.to_string(), origin_slot]));
                continue;
            }
            if input.get("widget").is_some() {
                let input_type = input
                    .get("type")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("UNKNOWN");
                let value = next_widget_value(
                    &node_id,
                    name,
                    input_type,
                    input,
                    widgets,
                    &mut widget_index,
                    &mut warnings,
                )
                .unwrap_or(JsonValue::Null);
                inputs.insert(name.into(), value);
            } else {
                inputs.insert(name.into(), JsonValue::Null);
                warnings.push(format!(
                    "node {node_id} input {name} is unlinked and has no static widget default"
                ));
            }
        }
        template.insert(node_id, json!({"class_type": node_type, "inputs": inputs}));
    }
    if nodes.is_empty() {
        return Err(WorkflowError::InvalidUiWorkflow(
            "nodes must contain at least one node".into(),
        ));
    }
    Ok(NormalizedWorkflow {
        template: JsonValue::Object(template),
        ui_inputs,
        warnings,
    })
}

pub(crate) fn next_widget_value(
    node_id: &str,
    input_name: &str,
    input_type: &str,
    input: &JsonValue,
    widgets: Option<&JsonValue>,
    array_index: &mut usize,
    warnings: &mut Vec<String>,
) -> Option<JsonValue> {
    let value = match widgets {
        Some(JsonValue::Object(values)) => {
            let widget_name = input
                .pointer("/widget/name")
                .and_then(JsonValue::as_str)
                .unwrap_or(input_name);
            values
                .get(widget_name)
                .or_else(|| values.get(input_name))
                .cloned()
        }
        Some(JsonValue::Array(values)) => {
            let Some(relative_index) = values
                .get(*array_index..)
                .unwrap_or_default()
                .iter()
                .position(|value| widget_value_matches_type(value, input_type))
            else {
                warnings.push(format!(
                    "node {node_id} input {input_name} has no type-compatible widgets_values entry"
                ));
                return None;
            };
            if relative_index > 0 {
                warnings.push(format!(
                    "node {node_id} input {input_name} skipped {relative_index} auxiliary \
                     widgets_values entries"
                ));
            }
            let index = array_index.saturating_add(relative_index);
            *array_index = index.saturating_add(1);
            values.get(index).cloned()
        }
        Some(_) => {
            warnings.push(format!(
                "node {node_id} input {input_name} has unsupported widgets_values shape"
            ));
            None
        }
        None => None,
    };
    if value.is_none() {
        warnings.push(format!(
            "node {node_id} input {input_name} has no corresponding widgets_values entry"
        ));
    }
    value
}

fn widget_value_matches_type(value: &JsonValue, input_type: &str) -> bool {
    match input_type.to_ascii_uppercase().as_str() {
        "INT" => value.as_i64().is_some() || value.as_u64().is_some(),
        "FLOAT" => value.is_number(),
        "BOOLEAN" => value.is_boolean(),
        "STRING" | "COMBO" => value.is_string(),
        _ => true,
    }
}

fn node_id(node: &JsonValue) -> Result<String, WorkflowError> {
    if let Some(id) = node.get("id").and_then(JsonValue::as_u64) {
        return Ok(id.to_string());
    }
    node.get("id")
        .and_then(JsonValue::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| WorkflowError::InvalidUiWorkflow("node id is required".into()))
}
