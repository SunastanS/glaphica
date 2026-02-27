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

// Phase 4.5: Ring buffer sample source (future)
// pub struct RingSampleSource { ... }
// impl SampleSource for RingSampleSource { ... }
