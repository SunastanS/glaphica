use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use protocol::{GpuCmdMsg, GpuFeedbackFrame, InputRingSample, InputSignal, PrioritizedInputSignal};
use ringbuf::traits::{Consumer, RingBuffer};
use ringbuf::HeapRb;

pub struct MainThreadChannels<Command, Receipt, Error> {
    pub input_control_queue: MainInputControlQueue,
    pub input_ring_producer: MainInputRingProducer,
    pub gpu_command_receiver: mpsc::Receiver<GpuCmdMsg<Command>>,
    pub gpu_feedback_sender: mpsc::Sender<GpuFeedbackFrame<Receipt, Error>>,
}

pub struct EngineThreadChannels<Command, Receipt, Error> {
    pub input_control_queue: EngineInputControlQueue,
    pub input_ring_consumer: EngineInputRingConsumer,
    pub gpu_command_sender: mpsc::Sender<GpuCmdMsg<Command>>,
    pub gpu_feedback_receiver: mpsc::Receiver<GpuFeedbackFrame<Receipt, Error>>,
}

pub struct MainInputRingProducer {
    ring: Arc<Mutex<HeapRb<InputRingSample>>>,
}

impl MainInputRingProducer {
    pub fn push(&self, sample: InputRingSample) {
        let mut ring = self.ring.lock().expect("input ring lock poisoned");
        ring.push_overwrite(sample);
    }
}

pub struct EngineInputRingConsumer {
    ring: Arc<Mutex<HeapRb<InputRingSample>>>,
}

impl EngineInputRingConsumer {
    pub fn pop(&mut self) -> Option<InputRingSample> {
        self.ring
            .lock()
            .expect("input ring lock poisoned")
            .try_pop()
    }
}

pub struct MainInputControlQueue {
    shared_queue: Arc<Mutex<BinaryHeap<PrioritizedInputSignal<InputSignal>>>>,
    sequence_counter: Arc<AtomicU64>,
}

impl MainInputControlQueue {
    pub fn push(&self, priority: u8, control: InputSignal) {
        let sequence = self.sequence_counter.fetch_add(1, Ordering::Relaxed);
        let item = PrioritizedInputSignal::new(priority, sequence, control);
        self.shared_queue.lock().expect("queue poisoned").push(item);
    }
}

pub struct EngineInputControlQueue {
    shared_queue: Arc<Mutex<BinaryHeap<PrioritizedInputSignal<InputSignal>>>>,
}

impl EngineInputControlQueue {
    pub fn pop(&self) -> Option<InputSignal> {
        self.shared_queue
            .lock()
            .expect("queue poisoned")
            .pop()
            .map(|item| item.control)
    }
}

pub fn create_thread_channels<Command, Receipt, Error>(
    input_ring_capacity: usize,
) -> (
    MainThreadChannels<Command, Receipt, Error>,
    EngineThreadChannels<Command, Receipt, Error>,
) {
    let input_ring = Arc::new(Mutex::new(HeapRb::<InputRingSample>::new(
        input_ring_capacity,
    )));
    let shared_input_control_queue =
        Arc::new(Mutex::new(BinaryHeap::<PrioritizedInputSignal<InputSignal>>::new()));
    let sequence_counter = Arc::new(AtomicU64::new(0));
    let (gpu_command_sender, gpu_command_receiver) = mpsc::channel();
    let (gpu_feedback_sender, gpu_feedback_receiver) = mpsc::channel();

    let main_thread_channels = MainThreadChannels {
        input_control_queue: MainInputControlQueue {
            shared_queue: shared_input_control_queue.clone(),
            sequence_counter,
        },
        input_ring_producer: MainInputRingProducer {
            ring: input_ring.clone(),
        },
        gpu_command_receiver,
        gpu_feedback_sender,
    };

    let engine_thread_channels = EngineThreadChannels {
        input_control_queue: EngineInputControlQueue {
            shared_queue: shared_input_control_queue,
        },
        input_ring_consumer: EngineInputRingConsumer { ring: input_ring },
        gpu_command_sender,
        gpu_feedback_receiver,
    };

    (main_thread_channels, engine_thread_channels)
}
