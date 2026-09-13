use super::{
    diagnostics::report_provider_error,
    wire::{Message, MessageInfo, WireEvent},
};
use crate::{AgentError, Completion, EventSender, Progress};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Reconciles SSE and persisted snapshots without replaying completed tool
/// transitions or exposing reasoning deltas. Final text replaces the preview.
pub(super) struct Reconciler {
    assistants:   HashSet<String>,
    text:         HashMap<String, String>,
    tool_states:  HashMap<String, u8>,
    skill:        String,
    skill_loaded: bool,
    text_bytes:   usize,
    max_bytes:    usize,
}

impl Reconciler {
    pub fn new(skill: String, max_bytes: usize) -> Self {
        Self {
            assistants: HashSet::new(),
            text: HashMap::new(),
            tool_states: HashMap::new(),
            skill,
            skill_loaded: false,
            text_bytes: 0,
            max_bytes,
        }
    }

    pub async fn event(
        &mut self,
        event: &WireEvent,
        sender: &EventSender,
    ) -> Result<(), AgentError> {
        match event.kind.as_str() {
            "session.error" => {
                report_provider_error(event.properties.get("error").unwrap_or(&Value::Null));
                return Err(AgentError::Provider);
            }
            "message.updated" => {
                if let Some(info) = event.properties.get("info")
                    && let Ok(info) = serde_json::from_value::<MessageInfo>(info.clone())
                {
                    self.remember(&info)?;
                }
            }
            "message.part.updated" => {
                if let Some(part) = event.properties.get("part") {
                    self.part(part, sender).await?;
                }
            }
            "message.part.delta"
                if event.properties.get("field").and_then(Value::as_str) == Some("text") =>
            {
                let Some(part_id) = event.properties.get("partID").and_then(Value::as_str) else {
                    return Ok(());
                };
                let Some(delta) = event.properties.get("delta").and_then(Value::as_str) else {
                    return Ok(());
                };
                // Unknown/reasoning part ids are never forwarded. Polling still
                // recovers final output if SSE ordering hid a text part header.
                if self.text.contains_key(part_id) {
                    self.add_text_bytes(delta.len())?;
                    self.text
                        .get_mut(part_id)
                        .expect("checked above")
                        .push_str(delta);
                    if !delta.is_empty() {
                        sender
                            .send(Progress::TextDelta { text: delta.into() })
                            .await?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn remember(&mut self, info: &MessageInfo) -> Result<(), AgentError> {
        if info.role == "assistant" && !info.summary {
            self.assistants.insert(info.id.clone());
            if self.assistants.len() > 4096 {
                return Err(AgentError::OutputLimit);
            }
        }
        Ok(())
    }

    pub async fn messages(
        &mut self,
        messages: &[Message],
        sender: &EventSender,
    ) -> Result<Option<Completion>, AgentError> {
        for message in messages {
            self.remember(&message.info)?;
            if message.info.role != "assistant" || message.info.summary {
                continue;
            }
            for part in &message.parts {
                self.part(part, sender).await?;
            }
        }
        let Some(latest) = messages
            .iter()
            .filter(|message| message.info.role == "assistant" && !message.info.summary)
            .max_by(|a, b| {
                (a.info.time.created, &a.info.id).cmp(&(b.info.time.created, &b.info.id))
            })
        else {
            return Ok(None);
        };
        if let Some(error) = latest.info.error.as_ref().filter(|error| !error.is_null()) {
            report_provider_error(error);
            return Err(AgentError::Provider);
        }
        // A completed *tool-call message* is not a completed execution. Nor is
        // an idle notification: require a terminal assistant finish marker.
        if latest.info.time.completed.is_none() {
            return Ok(None);
        }
        match latest.info.finish.as_deref() {
            None | Some("tool-calls" | "tool_calls" | "unknown") => return Ok(None),
            Some("length") => return Err(AgentError::OutputLimit),
            Some("content-filter" | "error") => return Err(AgentError::Provider),
            Some(_) => {}
        }
        if !self.skill_loaded {
            return Err(AgentError::Protocol(
                "execution completed without loading the approved skill",
            ));
        }
        let mut text = String::new();
        for part in &latest.parts {
            if part.get("type").and_then(Value::as_str) == Some("text")
                && part.get("synthetic").and_then(Value::as_bool) != Some(true)
                && let Some(value) = part.get("text").and_then(Value::as_str)
            {
                if text.len().saturating_add(value.len()) > self.max_bytes {
                    return Err(AgentError::OutputLimit);
                }
                text.push_str(value);
            }
        }
        Ok(Some(Completion { text }))
    }

    async fn part(&mut self, part: &Value, sender: &EventSender) -> Result<(), AgentError> {
        match part.get("type").and_then(Value::as_str) {
            Some("tool") => self.tool(part, sender).await?,
            Some("text") => {
                let Some(message_id) = part.get("messageID").and_then(Value::as_str) else {
                    return Ok(());
                };
                if !self.assistants.contains(message_id)
                    || part.get("synthetic").and_then(Value::as_bool) == Some(true)
                {
                    return Ok(());
                }
                let Some(id) = part.get("id").and_then(Value::as_str) else {
                    return Ok(());
                };
                let snapshot = part.get("text").and_then(Value::as_str).unwrap_or_default();
                let previous = self.text.get(id).map(String::as_str).unwrap_or_default();
                // Ignore stale polling snapshots and replays. A different
                // final string is still delivered in Completed, authoritatively.
                if let Some(delta) = snapshot.strip_prefix(previous).map(str::to_owned) {
                    self.add_text_bytes(delta.len())?;
                    self.text.insert(id.into(), snapshot.into());
                    if !delta.is_empty() {
                        sender.send(Progress::TextDelta { text: delta }).await?;
                    }
                }
            }
            _ => {}
        }
        if self.text.len() + self.tool_states.len() > 4096 {
            return Err(AgentError::OutputLimit);
        }
        Ok(())
    }

    fn add_text_bytes(&mut self, bytes: usize) -> Result<(), AgentError> {
        self.text_bytes = self.text_bytes.saturating_add(bytes);
        if self.text_bytes > self.max_bytes {
            return Err(AgentError::OutputLimit);
        }
        Ok(())
    }

    async fn tool(&mut self, part: &Value, sender: &EventSender) -> Result<(), AgentError> {
        let Some(call_id) = part.get("callID").and_then(Value::as_str) else {
            return Ok(());
        };
        let Some(state) = part.get("state") else {
            return Ok(());
        };
        let Some(status) = state.get("status").and_then(Value::as_str) else {
            return Ok(());
        };
        let name = part.get("tool").and_then(Value::as_str).unwrap_or("tool");
        if name == "skill"
            && status == "completed"
            && state.pointer("/input/name").and_then(Value::as_str) == Some(&self.skill)
        {
            self.skill_loaded = true;
        }
        let rank = match status {
            "running" => 1,
            "completed" | "error" => 2,
            _ => return Ok(()),
        };
        if self
            .tool_states
            .get(call_id)
            .is_some_and(|&previous| previous >= rank)
        {
            return Ok(());
        }
        if call_id.len() > 256 || name.len() > 128 {
            return Err(AgentError::Protocol("invalid tool identity"));
        }
        self.tool_states.insert(call_id.into(), rank);
        let event = match status {
            "running" => Progress::StageStarted {
                call_id: call_id.into(),
                name:    name.into(),
            },
            "completed" => Progress::StageFinished {
                call_id: call_id.into(),
                name:    name.into(),
            },
            _ => Progress::StageFailed {
                call_id: call_id.into(),
                name:    name.into(),
            },
        };
        sender.send(event).await
    }
}
