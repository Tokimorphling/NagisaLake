//! Parsing of ComfyUI `/history` and `/queue` responses.

use crate::error::ComfyUiError;
use nagisalake_core::{ComfyHistoryResponse, ComfyPromptStatus, OutputRef};
use serde_json::Value as JsonValue;
use std::collections::BTreeSet;

pub(super) fn parse_history(
    prompt_id: &str,
    payload: JsonValue,
) -> Result<ComfyHistoryResponse, ComfyUiError> {
    let Some(entry) = payload.get(prompt_id) else {
        return Ok(ComfyHistoryResponse::Pending);
    };
    if entry
        .pointer("/status/status_str")
        .and_then(JsonValue::as_str)
        .is_some_and(|status| status.eq_ignore_ascii_case("error"))
    {
        return Ok(ComfyHistoryResponse::Failed(
            entry
                .get("status")
                .map(JsonValue::to_string)
                .unwrap_or_else(|| "unknown execution error".into()),
        ));
    }
    let outputs = extract_outputs(entry.get("outputs"));
    if outputs.is_empty() {
        if entry
            .pointer("/status/completed")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false)
        {
            return Ok(ComfyHistoryResponse::Failed(
                "workflow completed without image, video, or audio artifacts".into(),
            ));
        }
        return Ok(ComfyHistoryResponse::Pending);
    }
    Ok(ComfyHistoryResponse::Complete(outputs))
}

pub(super) fn parse_queue_status(prompt_id: &str, payload: JsonValue) -> ComfyPromptStatus {
    if queue_position(payload.get("queue_running"), prompt_id).is_some() {
        return ComfyPromptStatus::Running;
    }
    queue_position(payload.get("queue_pending"), prompt_id)
        .map_or(ComfyPromptStatus::Unknown, |position| {
            ComfyPromptStatus::Queued { position }
        })
}

fn queue_position(entries: Option<&JsonValue>, prompt_id: &str) -> Option<u32> {
    entries
        .and_then(JsonValue::as_array)?
        .iter()
        .position(|entry| {
            entry
                .as_array()
                .and_then(|fields| fields.get(1))
                .and_then(JsonValue::as_str)
                == Some(prompt_id)
        })
        .map(|index| u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX))
}

fn extract_outputs(outputs: Option<&JsonValue>) -> Vec<OutputRef> {
    let Some(outputs) = outputs.and_then(JsonValue::as_object) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut unique = BTreeSet::new();
    for node in outputs.values().filter_map(JsonValue::as_object) {
        for kind in ["images", "gifs", "videos", "audio"] {
            let Some(items) = node.get(kind).and_then(JsonValue::as_array) else {
                continue;
            };
            for item in items.iter().filter_map(JsonValue::as_object) {
                let Some(filename) = item.get("filename").and_then(JsonValue::as_str) else {
                    continue;
                };
                let subfolder = item
                    .get("subfolder")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("");
                let storage_type = item
                    .get("type")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("output");
                let identity = (storage_type, subfolder, filename);
                if unique.insert(identity) {
                    found.push(OutputRef {
                        filename:     filename.into(),
                        subfolder:    subfolder.into(),
                        storage_type: storage_type.into(),
                        content_type: content_type_for(filename, kind).into(),
                    });
                }
            }
        }
    }
    found
}

fn content_type_for(filename: &str, kind: &str) -> &'static str {
    match filename.rsplit_once('.').map(|(_, extension)| extension) {
        Some(ext) if ext.eq_ignore_ascii_case("png") => "image/png",
        Some(ext) if ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg") => {
            "image/jpeg"
        }
        Some(ext) if ext.eq_ignore_ascii_case("webp") => "image/webp",
        Some(ext) if ext.eq_ignore_ascii_case("gif") => "image/gif",
        Some(ext) if ext.eq_ignore_ascii_case("mp4") => "video/mp4",
        Some(ext) if ext.eq_ignore_ascii_case("webm") => "video/webm",
        Some(ext) if ext.eq_ignore_ascii_case("mov") => "video/quicktime",
        Some(ext) if ext.eq_ignore_ascii_case("wav") => "audio/wav",
        Some(ext) if ext.eq_ignore_ascii_case("mp3") => "audio/mpeg",
        _ if kind == "videos" => "video/mp4",
        _ if kind == "audio" => "audio/wav",
        _ => "application/octet-stream",
    }
}
