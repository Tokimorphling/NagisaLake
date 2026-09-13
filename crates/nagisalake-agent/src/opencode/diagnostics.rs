use serde_json::Value;

/// Log only allowlisted classes and numeric status. Provider messages can
/// contain prompts, API keys, internal URLs or arbitrary tool output.
pub(super) fn report_provider_error(error: &Value) {
    let kind = match error.get("name").and_then(Value::as_str) {
        Some("ProviderAuthError") => "provider_auth",
        Some("APIError") => "provider_api",
        Some("MessageOutputLengthError") => "output_length",
        Some("ContextOverflowError") => "context_overflow",
        Some("ContentFilterError") => "content_filter",
        Some("MessageAbortedError") => "aborted",
        _ => "unknown",
    };
    let status = error.pointer("/data/statusCode").and_then(Value::as_u64);
    tracing::warn!(
        error_kind = kind,
        status_code = status,
        "OpenCode provider reported an error"
    );
}
