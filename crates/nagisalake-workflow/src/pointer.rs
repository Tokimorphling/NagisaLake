//! RFC 6901 JSON Pointer helpers shared by catalog binding and manifest naming.

use crate::error::WorkflowError;
use serde_json::Value as JsonValue;

pub(crate) fn pointer_node_id(pointer: &str) -> Option<String> {
    pointer_segments(pointer).first().cloned()
}

pub(crate) fn pointer_field(pointer: &str) -> Option<String> {
    let segments = pointer_segments(pointer);
    (segments.len() >= 3 && segments[1] == "inputs").then(|| segments[2].clone())
}

pub(crate) fn pointer_segments(pointer: &str) -> Vec<String> {
    pointer
        .split('/')
        .skip(1)
        .map(unescape_pointer_segment)
        .collect()
}

pub(crate) fn unescape_pointer_segment(segment: &str) -> String {
    segment.replace("~1", "/").replace("~0", "~")
}

pub(crate) fn escape_pointer_segment(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

pub(crate) fn validate_pointer(template: &JsonValue, pointer: &str) -> Result<(), WorkflowError> {
    if !pointer.starts_with('/') || template.pointer(pointer).is_none() {
        Err(WorkflowError::InvalidPointer(pointer.into()))
    } else {
        Ok(())
    }
}

pub(crate) fn insert_pointer(
    pointers: &mut Vec<String>,
    pointer: &str,
) -> Result<(), WorkflowError> {
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
