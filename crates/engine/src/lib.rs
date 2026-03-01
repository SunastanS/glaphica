use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError, bounded};
use crossbeam_queue::ArrayQueue;
use protocol::{
    GpuCmdMsg, GpuFeedbackFrame, InputControlEvent, InputRingSample,
};
use rtrb::{Consumer, PopError, Producer, PushError, RingBuffer};

pub struct MainThreadChannels<Command, Receipt, Error> {
    pub input_control_queue: MainInputControlQueue,
    pub input_ring_producer: MainInputRingProducer,
    pub gpu_command_receiver: Consumer<GpuCmdMsg<Command>>,
    pub gpu_feedback_sender: Producer<GpuFeedbackFrame<Receipt, Error>>,
}

pub struct EngineThreadChannels<Command, Receipt, Error> {
    pub input_control_queue: EngineInputControlQueue,
    pub input_ring_consumer: EngineInputRingConsumer,
    pub gpu_command_sender: Producer<GpuCmdMsg<Command>>,
    pub gpu_feedback_receiver: Consumer<GpuFeedbackFrame<Receipt, Error>>,
}

// This ring is designed for single‑producer, single‑consumer use.
// The Arc inside MainInputRingProducer and EngineInputRingConsumer is not exposed,
// preventing accidental creation of additional producers or consumers.
struct SharedInputRing {
    // UI thread writes are lock-free; when full we evict oldest and keep newest.
    queue: ArrayQueue<InputRingSample>,
    notify_sender: Sender<()>,
    notify_receiver: Receiver<()>,
    dropped: AtomicU64,
    pushed: AtomicU64,
}

pub struct MainInputRingProducer {
    shared: Arc<SharedInputRing>,
    // Cell marker keeps this type !Sync to discourage sharing one producer across threads.
    _spsc_marker: Cell<()>,
}

impl MainInputRingProducer {
    pub fn push(&self, sample: InputRingSample) {
        let mut pending_sample = sample;
        loop {
            match self.shared.queue.push(pending_sample) {
                Ok(()) => {
                    self.shared.pushed.fetch_add(1, Ordering::Relaxed);
                    match self.shared.notify_sender.try_send(()) {
                        Ok(()) => {}
                        Err(TrySendError::Full(())) => {}
                        Err(TrySendError::Disconnected(())) => {
                            panic!("input ring notify channel disconnected")
                        }
                    }
                    return;
                }
                Err(returned_sample) => {
                    pending_sample = returned_sample;
                    // In extreme races, the item removed here may not be the globally oldest one,
                    // because producer/consumer interleave between failed push and pop. This is
                    // acceptable for lossy input semantics as long as newest data keeps flowing.
                    if self.shared.queue.pop().is_some() {
                        self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                    } else {
                        std::thread::yield_now();
                    }
                }
            }
        }
    }

    pub fn dropped_samples(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    pub fn pushed_samples(&self) -> u64 {
        self.shared.pushed.load(Ordering::Relaxed)
    }
}

pub struct EngineInputRingConsumer {
    shared: Arc<SharedInputRing>,
    // Cell marker keeps this type !Sync to discourage sharing one consumer across threads.
    _spsc_marker: Cell<()>,
}

/// Drain up to `max_items` samples into `output`.
///
/// NOTE:
/// - This function APPENDS to `output`.
/// - It does NOT clear the vector.
/// - Caller is responsible for calling `output.clear()` if needed.
/// - `output` capacity is reused to avoid reallocations.
impl EngineInputRingConsumer {
    pub fn drain_batch_with_wait(
        &self,
        output: &mut Vec<InputRingSample>,
        max_items: usize,
        wait_timeout: Duration,
    ) {
        if max_items == 0 {
            return;
        }

        let mut drained_count = 0;
        while drained_count < max_items {
            match self.shared.queue.pop() {
                Some(sample) => {
                    output.push(sample);
                    drained_count += 1;
                }
                None => break,
            }
        }
        if drained_count > 0 || wait_timeout.is_zero() {
            return;
        }

        let wait_deadline = Instant::now() + wait_timeout;
        loop {
            let now = Instant::now();
            if now >= wait_deadline {
                return;
            }
            let remaining = wait_deadline.saturating_duration_since(now);
            match self.shared.notify_receiver.recv_timeout(remaining) {
                Ok(()) => {
                    while drained_count < max_items {
                        match self.shared.queue.pop() {
                            Some(sample) => {
                                output.push(sample);
                                drained_count += 1;
                            }
                            None => break,
                        }
                    }
                    if drained_count > 0 {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => return,
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("input ring notify channel disconnected")
                }
            }
        }
    }

    pub fn dropped_samples(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    pub fn pushed_samples(&self) -> u64 {
        self.shared.pushed.load(Ordering::Relaxed)
    }
}

pub struct MainInputControlQueue {
    producer: Producer<InputControlEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockingPushTimeout {
    pub timeout: Duration,
    pub full_retries: u64,
}

impl MainInputControlQueue {
    pub fn push(&mut self, control: InputControlEvent) -> Result<(), PushError<InputControlEvent>> {
        self.producer.push(control)
    }

    pub fn blocking_push(
        &mut self,
        mut control: InputControlEvent,
        timeout: Duration,
    ) -> Result<(), BlockingPushTimeout> {
        let start_time = Instant::now();
        let deadline = start_time + timeout;
        let mut full_retries = 0_u64;
        let mut park_duration = Duration::from_micros(50);

        loop {
            match self.producer.push(control) {
                Ok(()) => return Ok(()),
                Err(PushError::Full(returned_control)) => {
                    control = returned_control;
                    full_retries += 1;
                    if Instant::now() >= deadline {
                        return Err(BlockingPushTimeout {
                            timeout,
                            full_retries,
                        });
                    }

                    // Back off in stages to reduce scheduler thrash under sustained pressure.
                    if full_retries <= 64 {
                        std::hint::spin_loop();
                        continue;
                    }
                    if full_retries <= 72 {
                        std::thread::yield_now();
                        continue;
                    }

                    let remaining = deadline.saturating_duration_since(Instant::now());
                    std::thread::park_timeout(remaining.min(park_duration));
                    park_duration = (park_duration * 2).min(Duration::from_millis(1));
                }
            }
        }
    }

    pub fn blocking_push_or_panic(
        &mut self,
        control: InputControlEvent,
        timeout: Duration,
    ) {
        if let Err(timeout_info) = self.blocking_push(control, timeout) {
            panic!(
                "input control blocking_push timed out: timeout={:?}, full_retries={}",
                timeout_info.timeout, timeout_info.full_retries
            );
        }
    }

    pub fn slots(&self) -> usize {
        self.producer.slots()
    }
}

pub struct EngineInputControlQueue {
    consumer: Consumer<InputControlEvent>,
}

impl EngineInputControlQueue {
    pub fn pop(&mut self) -> Result<InputControlEvent, PopError> {
        self.consumer.pop()
    }

    pub fn items(&self) -> usize {
        self.consumer.slots()
    }
}

pub fn create_thread_channels<Command, Receipt, Error>(
    input_ring_capacity: usize,
    input_control_capacity: usize,
    gpu_command_capacity: usize,
    gpu_feedback_capacity: usize,
) -> (
    MainThreadChannels<Command, Receipt, Error>,
    EngineThreadChannels<Command, Receipt, Error>,
) {
    assert!(
        input_ring_capacity > 0,
        "input ring capacity must be greater than zero"
    );
    assert!(
        input_control_capacity > 0,
        "input control capacity must be greater than zero"
    );
    assert!(
        gpu_command_capacity > 0,
        "gpu command capacity must be greater than zero"
    );
    assert!(
        gpu_feedback_capacity > 0,
        "gpu feedback capacity must be greater than zero"
    );

    let (notify_sender, notify_receiver) = bounded(1);
    let shared_input_ring = Arc::new(SharedInputRing {
        queue: ArrayQueue::new(input_ring_capacity),
        notify_sender,
        notify_receiver,
        dropped: AtomicU64::new(0),
        pushed: AtomicU64::new(0),
    });

    let (input_control_sender, input_control_receiver) = RingBuffer::new(input_control_capacity);
    let (gpu_command_sender, gpu_command_receiver) = RingBuffer::new(gpu_command_capacity);
    let (gpu_feedback_sender, gpu_feedback_receiver) = RingBuffer::new(gpu_feedback_capacity);

    let main_thread_channels = MainThreadChannels {
        input_control_queue: MainInputControlQueue {
            producer: input_control_sender,
        },
        input_ring_producer: MainInputRingProducer {
            shared: shared_input_ring.clone(),
            _spsc_marker: Cell::new(()),
        },
        gpu_command_receiver,
        gpu_feedback_sender,
    };

    let engine_thread_channels = EngineThreadChannels {
        input_control_queue: EngineInputControlQueue {
            consumer: input_control_receiver,
        },
        input_ring_consumer: EngineInputRingConsumer {
            shared: shared_input_ring,
            _spsc_marker: Cell::new(()),
        },
        gpu_command_sender,
        gpu_feedback_receiver,
    };

    (main_thread_channels, engine_thread_channels)
}

#[cfg(test)]
mod tests {
    use protocol::{
        CompleteWaterline, ExecutedBatchWaterline, GpuFeedbackFrame, GpuFeedbackMergeState,
        PresentFrameId, SubmitWaterline, merge_test_support::{TestError, TestReceipt},
    };

    #[test]
    fn mailbox_merge_uses_protocol_merge_mailbox() {
        let current = GpuFeedbackFrame {
            present_frame_id: PresentFrameId(2),
            submit_waterline: SubmitWaterline(3),
            executed_batch_waterline: ExecutedBatchWaterline(4),
            complete_waterline: CompleteWaterline(5),
            receipts: vec![TestReceipt {
                key: 10,
                payload_version: 1,
            }]
            .into(),
            errors: vec![TestError { key: 99 }].into(),
        };
        let newer = GpuFeedbackFrame {
            present_frame_id: PresentFrameId(1),
            submit_waterline: SubmitWaterline(30),
            executed_batch_waterline: ExecutedBatchWaterline(40),
            complete_waterline: CompleteWaterline(50),
            receipts: vec![TestReceipt {
                key: 10,
                payload_version: 2,
            }]
            .into(),
            errors: vec![TestError { key: 99 }, TestError { key: 100 }].into(),
        };

        let mut merge_state = GpuFeedbackMergeState::default();
        let merged = GpuFeedbackFrame::merge_mailbox(current, newer, &mut merge_state);

        assert_eq!(merged.submit_waterline, SubmitWaterline(30));
        assert_eq!(merged.executed_batch_waterline, ExecutedBatchWaterline(40));
        assert_eq!(merged.complete_waterline, CompleteWaterline(50));
        assert_eq!(merged.receipts[0].payload_version, 2);
        assert_eq!(merged.errors.len(), 2);
    }
}
