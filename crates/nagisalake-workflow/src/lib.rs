//! Allowlisted ComfyUI API workflow catalog.

use nagisalake_protocol::{
    DispatchJob, WorkflowCapability, WorkflowInput, WorkflowInputKind, WorkflowManifest,
    WorkflowOutput,
};
use serde::Deserialize;
use serde_json::{Map, Value as JsonValue, json};
use service_async::{
    MakeService, Service,
    layer::{FactoryLayer, layer_fn},
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    convert::Infallible,
    path::PathBuf,
    sync::Arc,
};
use thiserror::Error;

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowConfig {
    pub id:           String,
    pub version:      String,
    pub file:         PathBuf,
    #[serde(default)]
    pub output_types: Vec<String>,
    /// User parameter name to RFC 6901 JSON Pointer.
    #[serde(default)]
    pub parameters:   BTreeMap<String, String>,
    #[serde(default)]
    pub inputs:       Vec<InputBinding>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InputBinding {
    pub index:        usize,
    pub pointer:      String,
    #[serde(default)]
    pub name:         Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WorkflowCatalog {
    entries: BTreeMap<(String, String), WorkflowDefinition>,
}

#[derive(Debug, Clone)]
struct WorkflowDefinition {
    config:   WorkflowConfig,
    template: Arc<JsonValue>,
    manifest: WorkflowManifest,
}

#[derive(Debug, Default)]
struct NormalizedWorkflow {
    template:  JsonValue,
    ui_inputs: BTreeMap<String, UiInputMetadata>,
    warnings:  Vec<String>,
}

#[derive(Debug, Clone)]
struct UiInputMetadata {
    input_type: String,
    node_type:  String,
}

impl WorkflowCatalog {
    pub fn load(configs: &[WorkflowConfig]) -> Result<Self, WorkflowError> {
        let mut entries = BTreeMap::new();
        for config in configs {
            let raw = std::fs::read_to_string(&config.file).map_err(|source| {
                WorkflowError::ReadTemplate {
                    path: config.file.clone(),
                    source,
                }
            })?;
            let raw_template =
                serde_json::from_str(&raw).map_err(|source| WorkflowError::ParseTemplate {
                    path: config.file.clone(),
                    source,
                })?;
            let normalized = normalize_workflow(raw_template)?;
            Self::insert(&mut entries, config.clone(), normalized)?;
        }
        if entries.is_empty() {
            return Err(WorkflowError::EmptyCatalog);
        }
        Ok(Self { entries })
    }

    pub fn from_templates(
        templates: impl IntoIterator<Item = (WorkflowConfig, JsonValue)>,
    ) -> Result<Self, WorkflowError> {
        let mut entries = BTreeMap::new();
        for (config, template) in templates {
            Self::insert(&mut entries, config, NormalizedWorkflow {
                template,
                ..NormalizedWorkflow::default()
            })?;
        }
        if entries.is_empty() {
            return Err(WorkflowError::EmptyCatalog);
        }
        Ok(Self { entries })
    }

    pub fn capabilities(&self) -> Vec<WorkflowCapability> {
        self.entries
            .values()
            .map(|entry| WorkflowCapability {
                id:           entry.config.id.clone(),
                version:      entry.config.version.clone(),
                output_types: entry.config.output_types.clone(),
                manifest:     Some(entry.manifest.clone()),
            })
            .collect()
    }

    pub fn validate(&self, dispatch: &DispatchJob) -> Result<(), WorkflowError> {
        let entry = self.entry(dispatch)?;
        let parameters = dispatch
            .parameters
            .as_object()
            .ok_or(WorkflowError::ParametersMustBeObject)?;
        for name in parameters.keys() {
            if !entry.config.parameters.contains_key(name) {
                return Err(WorkflowError::UnknownParameter(name.clone()));
            }
        }
        let bound = entry
            .config
            .inputs
            .iter()
            .map(|binding| binding.index)
            .collect::<BTreeSet<_>>();
        if dispatch.inputs.len() != bound.len()
            || bound.iter().copied().ne(0..dispatch.inputs.len())
        {
            return Err(WorkflowError::InputCount {
                expected: bound.len(),
                actual:   dispatch.inputs.len(),
            });
        }
        Ok(())
    }

    pub fn render(
        &self,
        dispatch: &DispatchJob,
        input_names: &[String],
    ) -> Result<JsonValue, WorkflowError> {
        self.validate(dispatch)?;
        if input_names.len() != dispatch.inputs.len() {
            return Err(WorkflowError::InputCount {
                expected: dispatch.inputs.len(),
                actual:   input_names.len(),
            });
        }
        let entry = self.entry(dispatch)?;
        // Deep clone only at the point we actually need to mutate. The catalog
        // keeps the template behind Arc, so validate/lookup/capabilities share
        // one allocation instead of cloning the whole JSON tree per dispatch.
        let mut workflow = (*entry.template).clone();
        for (name, value) in dispatch
            .parameters
            .as_object()
            .expect("validated parameters")
        {
            let pointer = &entry.config.parameters[name];
            *workflow
                .pointer_mut(pointer)
                .expect("binding validated when catalog was built") = value.clone();
        }
        for binding in &entry.config.inputs {
            *workflow
                .pointer_mut(&binding.pointer)
                .expect("binding validated when catalog was built") =
                JsonValue::String(input_names[binding.index].clone());
        }
        Ok(workflow)
    }

    fn insert(
        entries: &mut BTreeMap<(String, String), WorkflowDefinition>,
        config: WorkflowConfig,
        normalized: NormalizedWorkflow,
    ) -> Result<(), WorkflowError> {
        required("id", &config.id)?;
        required("version", &config.version)?;
        let NormalizedWorkflow {
            template,
            ui_inputs,
            warnings,
        } = normalized;
        if !template.is_object() {
            return Err(WorkflowError::TemplateMustBeObject(config.id));
        }
        let mut pointers = Vec::new();
        let mut public_inputs = BTreeSet::new();
        for (name, pointer) in &config.parameters {
            required("parameter name", name)?;
            if !public_inputs.insert(name.clone()) {
                return Err(WorkflowError::DuplicatePublicInput(name.clone()));
            }
            validate_pointer(&template, pointer)?;
            insert_pointer(&mut pointers, pointer)?;
        }
        let mut indices = BTreeSet::new();
        for binding in &config.inputs {
            validate_pointer(&template, &binding.pointer)?;
            insert_pointer(&mut pointers, &binding.pointer)?;
            if let Some(name) = &binding.name {
                required("input name", name)?;
            }
            if let Some(content_type) = &binding.content_type {
                required("input content_type", content_type)?;
            }
            let public_name = binding
                .name
                .clone()
                .or_else(|| pointer_field(&binding.pointer))
                .unwrap_or_else(|| format!("input_{}", binding.index));
            if !public_inputs.insert(public_name.clone()) {
                return Err(WorkflowError::DuplicatePublicInput(public_name));
            }
            if !indices.insert(binding.index) {
                return Err(WorkflowError::DuplicateInputIndex(binding.index));
            }
        }
        if indices.iter().copied().ne(0..indices.len()) {
            return Err(WorkflowError::NonContiguousInputs);
        }
        let key = (config.id.clone(), config.version.clone());
        if entries
            .insert(key.clone(), WorkflowDefinition {
                manifest: build_manifest(&config, &template, &ui_inputs, warnings),
                config,
                template: Arc::new(template),
            })
            .is_some()
        {
            return Err(WorkflowError::DuplicateWorkflow {
                id:      key.0,
                version: key.1,
            });
        }
        Ok(())
    }

    fn entry(&self, dispatch: &DispatchJob) -> Result<&WorkflowDefinition, WorkflowError> {
        self.entries
            .get(&(
                dispatch.workflow_id.clone(),
                dispatch.workflow_version.clone(),
            ))
            .ok_or_else(|| WorkflowError::NotInstalled {
                id:      dispatch.workflow_id.clone(),
                version: dispatch.workflow_version.clone(),
            })
    }
}

fn normalize_workflow(raw: JsonValue) -> Result<NormalizedWorkflow, WorkflowError> {
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

fn next_widget_value(
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

fn build_manifest(
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

fn pointer_node_id(pointer: &str) -> Option<String> {
    pointer_segments(pointer).first().cloned()
}

fn pointer_field(pointer: &str) -> Option<String> {
    let segments = pointer_segments(pointer);
    (segments.len() >= 3 && segments[1] == "inputs").then(|| segments[2].clone())
}

fn pointer_segments(pointer: &str) -> Vec<String> {
    pointer
        .split('/')
        .skip(1)
        .map(unescape_pointer_segment)
        .collect()
}

fn unescape_pointer_segment(segment: &str) -> String {
    segment.replace("~1", "/").replace("~0", "~")
}

fn escape_pointer_segment(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

#[derive(Debug, Clone)]
pub struct RenderWorkflow {
    pub dispatch:    DispatchJob,
    pub input_names: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct WorkflowService {
    catalog: Arc<WorkflowCatalog>,
}

impl WorkflowService {
    pub fn new(catalog: Arc<WorkflowCatalog>) -> Self {
        Self { catalog }
    }

    pub fn layer<C>(
        catalog: Arc<WorkflowCatalog>,
    ) -> impl FactoryLayer<C, (), Factory = WorkflowServiceFactory> {
        layer_fn(move |_config: &C, ()| WorkflowServiceFactory {
            catalog: Arc::clone(&catalog),
        })
    }
}

impl Service<RenderWorkflow> for WorkflowService {
    type Response = JsonValue;
    type Error = WorkflowError;

    async fn call(&self, request: RenderWorkflow) -> Result<Self::Response, Self::Error> {
        self.catalog.render(&request.dispatch, &request.input_names)
    }
}

#[derive(Debug, Clone)]
pub struct WorkflowServiceFactory {
    catalog: Arc<WorkflowCatalog>,
}

impl MakeService for WorkflowServiceFactory {
    type Service = WorkflowService;
    type Error = Infallible;

    fn make_via_ref(&self, _old: Option<&Self::Service>) -> Result<Self::Service, Self::Error> {
        Ok(WorkflowService::new(Arc::clone(&self.catalog)))
    }
}

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("at least one workflow must be installed")]
    EmptyCatalog,
    #[error("failed to read workflow template {path}: {source}")]
    ReadTemplate {
        path:   PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse workflow template {path}: {source}")]
    ParseTemplate {
        path:   PathBuf,
        source: serde_json::Error,
    },
    #[error("workflow {0} must be a ComfyUI API JSON object")]
    TemplateMustBeObject(String),
    #[error("invalid ComfyUI editor workflow: {0}")]
    InvalidUiWorkflow(String),
    #[error("workflow field {0} must not be empty")]
    Required(&'static str),
    #[error("JSON pointer {0:?} does not exist in the workflow template")]
    InvalidPointer(String),
    #[error("duplicate input binding index {0}")]
    DuplicateInputIndex(usize),
    #[error("duplicate public workflow input name {0:?}")]
    DuplicatePublicInput(String),
    #[error("workflow bindings {first:?} and {second:?} overlap")]
    OverlappingPointers { first: String, second: String },
    #[error("input binding indices must be contiguous and start at zero")]
    NonContiguousInputs,
    #[error("duplicate workflow {id} version {version}")]
    DuplicateWorkflow { id: String, version: String },
    #[error("workflow {id} version {version} is not installed")]
    NotInstalled { id: String, version: String },
    #[error("job parameters must be a JSON object")]
    ParametersMustBeObject,
    #[error("parameter {0:?} is not allowlisted by the workflow")]
    UnknownParameter(String),
    #[error("workflow expects {expected} ordered inputs, got {actual}")]
    InputCount { expected: usize, actual: usize },
}

fn required(field: &'static str, value: &str) -> Result<(), WorkflowError> {
    if value.trim().is_empty() {
        Err(WorkflowError::Required(field))
    } else {
        Ok(())
    }
}

fn validate_pointer(template: &JsonValue, pointer: &str) -> Result<(), WorkflowError> {
    if !pointer.starts_with('/') || template.pointer(pointer).is_none() {
        Err(WorkflowError::InvalidPointer(pointer.into()))
    } else {
        Ok(())
    }
}

fn insert_pointer(pointers: &mut Vec<String>, pointer: &str) -> Result<(), WorkflowError> {
    if let Some(existing) = pointers
        .iter()
        .find(|existing| pointers_overlap(existing, pointer))
    {
        return Err(WorkflowError::OverlappingPointers {
            first:  existing.clone(),
            second: pointer.into(),
        });
    }
    pointers.push(pointer.into());
    Ok(())
}

fn pointers_overlap(left: &str, right: &str) -> bool {
    left == right
        || (right.starts_with(left) && right.as_bytes().get(left.len()) == Some(&b'/'))
        || (left.starts_with(right) && left.as_bytes().get(right.len()) == Some(&b'/'))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
