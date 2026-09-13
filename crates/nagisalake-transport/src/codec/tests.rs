use super::*;
use serde_json::{Value, json};

struct Chunked {
    input:    Vec<u8>,
    position: usize,
    output:   Vec<u8>,
    chunk:    usize,
}
impl TunnelStream for Chunked {
    async fn read(
        &mut self,
        buffer: &mut [u8],
    ) -> Result<usize, tokilake_core::error::TunnelError> {
        let size = buffer
            .len()
            .min(self.chunk)
            .min(self.input.len() - self.position);
        buffer[..size].copy_from_slice(&self.input[self.position..self.position + size]);
        self.position += size;
        Ok(size)
    }
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, tokilake_core::error::TunnelError> {
        let size = buffer.len().min(self.chunk);
        self.output.extend_from_slice(&buffer[..size]);
        Ok(size)
    }
    async fn flush(&mut self) -> Result<(), tokilake_core::error::TunnelError> {
        Ok(())
    }
    async fn close(&mut self) -> Result<(), tokilake_core::error::TunnelError> {
        Ok(())
    }
}
fn stream(input: Vec<u8>, chunk: usize) -> Chunked {
    Chunked {
        input,
        position: 0,
        output: Vec::new(),
        chunk,
    }
}

#[tokio::test]
async fn fragmented_large_frames_blank_lines_and_eof_reset_the_scan_offset() {
    let large = json!({"text":"湖".repeat(20_000)});
    let input = format!("\n{}\n \n42\n\"tail\"", large).into_bytes();
    let mut codec = JsonLineCodec::new(stream(input, 7), 128 * 1024).unwrap();
    assert_eq!(codec.receive::<Value>().await.unwrap(), Some(large));
    assert_eq!(codec.receive::<Value>().await.unwrap(), Some(json!(42)));
    assert_eq!(codec.receive::<Value>().await.unwrap(), Some(json!("tail")));
    assert!(codec.receive::<Value>().await.unwrap().is_none());
}

#[tokio::test]
async fn send_reuses_storage_handles_short_writes_and_keeps_frame_limits() {
    let mut codec = JsonLineCodec::new(stream(Vec::new(), 2), 64).unwrap();
    codec.send(&json!({"x":1})).await.unwrap();
    let capacity = codec.write_buffer.capacity();
    codec.send(&json!({"x":2})).await.unwrap();
    assert_eq!(codec.write_buffer.capacity(), capacity);
    assert!(matches!(
        codec.send(&"x".repeat(65)).await,
        Err(TransportError::FrameTooLarge { .. })
    ));
    assert_eq!(codec.into_inner().output, b"{\"x\":1}\n{\"x\":2}\n");
    let mut codec = JsonLineCodec::new(stream(b"123456789\n".to_vec(), 1), 8).unwrap();
    assert!(matches!(
        codec.receive::<Value>().await,
        Err(TransportError::FrameTooLarge { .. })
    ));
}
