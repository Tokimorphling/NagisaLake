use crate::ConfigError;

/// Length-prefix, buffering, and write-batching settings.
#[derive(Clone, Copy, Debug)]
pub struct FrameConfig {
    /// Maximum payload size, excluding the four-byte length prefix.
    pub max_frame_bytes:      usize,
    /// Initial reusable read and write buffer allocation.
    pub initial_buffer_bytes: usize,
    /// Spare capacity reserved before each socket read.
    ///
    /// This is the upper bound on how much one read can deliver, so it sets how
    /// many small frames a single syscall can carry.
    pub read_chunk_bytes:     usize,
    /// Maximum frames coalesced into one write.
    pub max_batch_frames:     usize,
    /// Byte threshold that ends a batch, and the size at which a body is written
    /// directly instead of being copied into the batch buffer.
    pub max_batch_bytes:      usize,
}

impl Default for FrameConfig {
    fn default() -> Self {
        Self {
            max_frame_bytes:      8 * 1024 * 1024,
            initial_buffer_bytes: 16 * 1024,
            read_chunk_bytes:     32 * 1024,
            max_batch_frames:     64,
            max_batch_bytes:      256 * 1024,
        }
    }
}

impl FrameConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.max_frame_bytes == 0 || self.max_frame_bytes > u32::MAX as usize {
            return Err(ConfigError::InvalidFrameLimit(self.max_frame_bytes));
        }
        if self.initial_buffer_bytes == 0 {
            return Err(ConfigError::Zero("initial_buffer_bytes"));
        }
        if self.read_chunk_bytes == 0 {
            return Err(ConfigError::Zero("read_chunk_bytes"));
        }
        if self.max_batch_frames == 0 {
            return Err(ConfigError::Zero("max_batch_frames"));
        }
        if self.max_batch_bytes == 0 {
            return Err(ConfigError::Zero("max_batch_bytes"));
        }
        Ok(())
    }
}
