use crate::AgentError;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub(super) struct WireEvent {
    #[serde(rename = "type")]
    pub kind:       String,
    #[serde(default)]
    pub properties: Value,
}

impl WireEvent {
    pub fn parse(data: &[u8]) -> Result<Self, AgentError> {
        let mut value: Value =
            serde_json::from_slice(data).map_err(|_| AgentError::Protocol("invalid SSE JSON"))?;
        // Move, rather than deep-clone, payload-wrapped events.
        let payload = value.get_mut("payload").map(Value::take).unwrap_or(value);
        serde_json::from_value(payload).map_err(|_| AgentError::Protocol("invalid SSE event"))
    }

    pub fn session_id(&self) -> Option<&str> {
        self.properties
            .get("sessionID")
            .and_then(Value::as_str)
            .or_else(|| {
                self.properties
                    .pointer("/part/sessionID")
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                self.properties
                    .pointer("/info/sessionID")
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                self.kind
                    .starts_with("session.")
                    .then(|| self.properties.pointer("/info/id").and_then(Value::as_str))
                    .flatten()
            })
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct Message {
    pub info:  MessageInfo,
    #[serde(default)]
    pub parts: Vec<Value>,
}

#[derive(Debug, Deserialize)]
pub(super) struct MessageInfo {
    pub id:      String,
    pub role:    String,
    #[serde(default)]
    pub time:    MessageTime,
    #[serde(default)]
    pub finish:  Option<String>,
    #[serde(default)]
    pub error:   Option<Value>,
    // UserMessage.summary is an object, AssistantMessage.summary is a bool.
    // Treat only the literal true as an assistant compaction summary.
    #[serde(default, deserialize_with = "summary_flag")]
    pub summary: bool,
}

fn summary_flag<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(Value::deserialize(deserializer)?.as_bool().unwrap_or(false))
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct MessageTime {
    #[serde(default)]
    pub created:   u64,
    pub completed: Option<u64>,
}
