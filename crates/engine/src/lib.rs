use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use protocol::{GpuCmdMsg, GpuFeedbackFrame, InputControlEvent, InputRingSample};
use ringbuf::traits::{Consumer, RingBuffer};
use ringbuf::HeapRb;

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
    sender: mpsc::Sender<InputControlEvent>,
}

impl MainInputControlQueue {
    pub fn push(
        &self,
        control: InputControlEvent,
    ) -> Result<(), mpsc::SendError<InputControlEvent>> {
        self.sender.send(control)
    }
}

pub struct EngineInputControlQueue {
    receiver: mpsc::Receiver<InputControlEvent>,
}

impl EngineInputControlQueue {
    pub fn pop(&self) -> Result<InputControlEvent, mpsc::RecvError> {
        self.receiver.recv()
    }

    pub fn try_pop(&self) -> Result<InputControlEvent, mpsc::TryRecvError> {
        self.receiver.try_recv()
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
    let (input_control_sender, input_control_receiver) = mpsc::channel();
    let (gpu_command_sender, gpu_command_receiver) = mpsc::channel();
    let (gpu_feedback_sender, gpu_feedback_receiver) = mpsc::channel();

    let main_thread_channels = MainThreadChannels {
        input_control_queue: MainInputControlQueue {
            sender: input_control_sender,
        },
        input_ring_producer: MainInputRingProducer {
            ring: input_ring.clone(),
        },
        gpu_command_receiver,
        gpu_feedback_sender,
    };

    let engine_thread_channels = EngineThreadChannels {
        input_control_queue: EngineInputControlQueue {
            receiver: input_control_receiver,
        },
        input_ring_consumer: EngineInputRingConsumer { ring: input_ring },
        gpu_command_sender,
        gpu_feedback_receiver,
    };

    (main_thread_channels, engine_thread_channels)
}
