//! Allowlisted workflow catalog: load, validate, and render API-format templates.

use crate::{
    error::WorkflowError,
    manifest::build_manifest,
    normalize::{NormalizedWorkflow, normalize_workflow},
    pointer::{insert_pointer, pointer_field, validate_pointer},
};
use nagisalake_protocol::{DispatchJob, WorkflowCapability, WorkflowManifest};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

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

fn required(field: &'static str, value: &str) -> Result<(), WorkflowError> {
    if value.trim().is_empty() {
        Err(WorkflowError::Required(field))
    } else {
        Ok(())
    }
}
