/// Sample Source module.
///
/// Abstracts the source of input samples for the engine thread.
/// This allows Phase 4 to use simple channels, while Phase 4.5 can switch to ring buffers.
use protocol::InputRingSample;

/// Trait for draining input samples in batches.
pub trait SampleSource {
    /// Drain up to `budget` samples into `output`.
    ///
    /// NOTE:
    /// - This function APPENDS to `output`.
    /// - It does NOT clear the vector.
    /// - Caller is responsible for calling `output.clear()` if needed.
    fn drain_batch(&mut self, output: &mut Vec<InputRingSample>, budget: usize);
}

/// Phase 4: No-op sample source (temporary implementation for testing).
///
/// Always returns empty samples. Used during Phase 4 transition
/// before Phase 4.5 enables the actual input ring buffer.
pub struct NoOpSampleSource;

impl NoOpSampleSource {
    /// Create a new NoOpSampleSource.
    pub fn new() -> Self {
        Self
    }
}

impl Default for NoOpSampleSource {
    fn default() -> Self {
        Self::new()
    }
}

impl SampleSource for NoOpSampleSource {
    fn drain_batch(&mut self, _output: &mut Vec<InputRingSample>, _budget: usize) {
        // No-op: never produce any samples
        // This is intentional for Phase 4 testing
    }
}

/// Phase 4: Simple channel-based sample source.
pub struct ChannelSampleSource {
    receiver: crossbeam_channel::Receiver<InputRingSample>,
}

impl ChannelSampleSource {
    /// Create a new ChannelSampleSource from a receiver.
    pub fn new(receiver: crossbeam_channel::Receiver<InputRingSample>) -> Self {
        Self { receiver }
    }
}

impl SampleSource for ChannelSampleSource {
    fn drain_batch(&mut self, output: &mut Vec<InputRingSample>, budget: usize) {
        output.clear();
        for _ in 0..budget {
            match self.receiver.try_recv() {
                Ok(sample) => output.push(sample),
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => return,
            }
        }
    }
}
