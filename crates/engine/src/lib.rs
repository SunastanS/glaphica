use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::{Condvar, Mutex};
use protocol::{GpuCmdMsg, GpuFeedbackFrame, InputControlEvent, InputRingSample};
use rtrb::{Consumer, PopError, Producer, PushError, RingBuffer};

/// Compose engine-owned aggregate enums from feature-specific message types.
///
/// Correct assembly pattern for this refactor:
/// 1. Every feature crate defines its own message type (for example `renderer::RendererMsg`,
///    `document::DocumentMsg`) without depending on `engine`.
/// 2. `engine` owns the aggregate enum (for example `EngineMsg`) that contains those variants.
/// 3. `engine` implements `From<FeatureMsg> for EngineMsg` for each feature message type.
/// 4. Callers use this trait (or plain `From`) to convert feature-local messages into the
///    engine aggregate enum at the thread boundary.
pub trait ComposeIntoEngine<EngineAggregate> {
    fn compose_into_engine(self) -> EngineAggregate;
}

impl<EngineAggregate, FeatureMessage> ComposeIntoEngine<EngineAggregate> for FeatureMessage
where
    EngineAggregate: From<FeatureMessage>,
{
    fn compose_into_engine(self) -> EngineAggregate {
        EngineAggregate::from(self)
    }
}

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

struct SharedInputRing {
    // This ring intentionally does not use rtrb: our write policy is "drop oldest, keep newest"
    // on overflow, and that overwrite operation requires coarse-grained exclusive access.
    queue: Mutex<VecDeque<InputRingSample>>,
    not_empty: Condvar,
    capacity: usize,
    dropped: AtomicU64,
    pushed: AtomicU64,
}

pub struct MainInputRingProducer {
    shared: Arc<SharedInputRing>,
}

impl MainInputRingProducer {
    pub fn push(&self, sample: InputRingSample) {
        let mut queue = self.shared.queue.lock();
        let was_empty = queue.is_empty();
        if queue.len() == self.shared.capacity {
            queue.pop_front();
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
        }
        queue.push_back(sample);
        self.shared.pushed.fetch_add(1, Ordering::Relaxed);
        if was_empty {
            self.shared.not_empty.notify_one();
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

        let mut queue = self.shared.queue.lock();
        while queue.is_empty() {
            if self
                .shared
                .not_empty
                .wait_for(&mut queue, wait_timeout)
                .timed_out()
            {
                break;
            }
        }

        let mut drained_count = 0;
        while drained_count < max_items {
            match queue.pop_front() {
                Some(sample) => {
                    output.push(sample);
                    drained_count += 1;
                }
                None => break,
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

impl MainInputControlQueue {
    pub fn push(&mut self, control: InputControlEvent) -> Result<(), PushError<InputControlEvent>> {
        self.producer.push(control)
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

    let shared_input_ring = Arc::new(SharedInputRing {
        queue: Mutex::new(VecDeque::with_capacity(input_ring_capacity)),
        not_empty: Condvar::new(),
        capacity: input_ring_capacity,
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
        },
        gpu_command_sender,
        gpu_feedback_receiver,
    };

    (main_thread_channels, engine_thread_channels)
}
