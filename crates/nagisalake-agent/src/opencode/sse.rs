use crate::AgentError;

/// Incremental, byte-bounded SSE data decoder. Handles UTF-8 split across TCP
/// reads and LF, CRLF (including split CR/LF), and bare CR line endings. JSON is
/// decoded only after a complete event, not once per transport chunk.
pub(super) struct Decoder {
    line:        Vec<u8>,
    data:        Vec<u8>,
    skip_lf:     bool,
    event_bytes: usize,
    limit:       usize,
}

impl Decoder {
    pub fn new(limit: usize) -> Self {
        Self {
            line: Vec::new(),
            data: Vec::new(),
            skip_lf: false,
            event_bytes: 0,
            limit,
        }
    }

    pub fn push(&mut self, mut input: &[u8]) -> Result<Vec<Vec<u8>>, AgentError> {
        let mut events = Vec::new();
        while !input.is_empty() {
            if self.skip_lf {
                self.skip_lf = false;
                if input[0] == b'\n' {
                    input = &input[1..];
                    continue;
                }
            }
            let end = input.iter().position(|b| *b == b'\n' || *b == b'\r');
            let length = end.unwrap_or(input.len());
            self.event_bytes = self
                .event_bytes
                .saturating_add(length + usize::from(end.is_some()));
            if self.event_bytes > self.limit {
                return Err(AgentError::Protocol("SSE event exceeds size limit"));
            }
            self.line.extend_from_slice(&input[..length]);
            let Some(end) = end else {
                break;
            };
            self.skip_lf = input[end] == b'\r';
            input = &input[end + 1..];
            if self.line.is_empty() {
                if !self.data.is_empty() {
                    self.data.pop(); // trailing data-line newline
                    events.push(std::mem::take(&mut self.data));
                }
                self.event_bytes = 0;
            } else if self.line == b"data" {
                self.data.push(b'\n');
            } else if let Some(value) = self.line.strip_prefix(b"data:") {
                self.data
                    .extend_from_slice(value.strip_prefix(b" ").unwrap_or(value));
                self.data.push(b'\n');
            }
            self.line.clear();
        }
        Ok(events)
    }
}
