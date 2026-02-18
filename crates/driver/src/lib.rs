pub mod no_smoothing_uniform_resampling;

pub use no_smoothing_uniform_resampling::{
    NoSmoothingUniformResampling, NoSmoothingUniformResamplingConfig,
};

pub type StrokeSessionId = u64;
pub type EventTimestampMicros = u64;
pub type FrameSequenceId = u64;

pub const DAB_QUEUE_CHUNK_CAPACITY: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerDeviceKind {
    Mouse,
    Pen,
    Touch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerEventPhase {
    Hover,
    Down,
    Move,
    Up,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawPointerInput {
    pub pointer_id: u64,
    pub device_kind: PointerDeviceKind,
    pub phase: PointerEventPhase,
    pub timestamp_micros: EventTimestampMicros,
    pub screen_x: f32,
    pub screen_y: f32,
    pub pressure: Option<f32>,
    pub tilt_x_degrees: Option<f32>,
    pub tilt_y_degrees: Option<f32>,
    pub twist_degrees: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DabPoint {
    pub timestamp_micros: EventTimestampMicros,
    pub canvas_x: f32,
    pub canvas_y: f32,
    pub pressure: f32,
    pub velocity_pixels_per_second: f32,
    pub tilt_x_degrees: f32,
    pub tilt_y_degrees: f32,
    pub twist_degrees: f32,
}

impl Default for DabPoint {
    fn default() -> Self {
        Self {
            timestamp_micros: 0,
            canvas_x: 0.0,
            canvas_y: 0.0,
            pressure: 0.0,
            velocity_pixels_per_second: 0.0,
            tilt_x_degrees: 0.0,
            tilt_y_degrees: 0.0,
            twist_degrees: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DriverOutputBatch {
    pub stroke_session_id: StrokeSessionId,
    pub pointer_id: u64,
    pub ended: bool,
    pub dab_points: Vec<DabPoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameDispatchSignal {
    pub frame_sequence_id: FrameSequenceId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FramedDabChunk {
    pub frame_sequence_id: FrameSequenceId,
    pub chunk: DabChunk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DabChunkBuildError {
    TooManyDabs,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DabChunk {
    pub stroke_session_id: StrokeSessionId,
    pub pointer_id: u64,
    pub starts_stroke: bool,
    pub ends_stroke: bool,
    pub discontinuity_before: bool,
    pub dropped_chunk_count_before: u16,
    len: u8,
    timestamp_micros: [EventTimestampMicros; DAB_QUEUE_CHUNK_CAPACITY],
    canvas_x: [f32; DAB_QUEUE_CHUNK_CAPACITY],
    canvas_y: [f32; DAB_QUEUE_CHUNK_CAPACITY],
    pressure: [f32; DAB_QUEUE_CHUNK_CAPACITY],
    velocity_pixels_per_second: [f32; DAB_QUEUE_CHUNK_CAPACITY],
    tilt_x_degrees: [f32; DAB_QUEUE_CHUNK_CAPACITY],
    tilt_y_degrees: [f32; DAB_QUEUE_CHUNK_CAPACITY],
    twist_degrees: [f32; DAB_QUEUE_CHUNK_CAPACITY],
}

impl DabChunk {
    pub fn from_dabs(
        stroke_session_id: StrokeSessionId,
        pointer_id: u64,
        starts_stroke: bool,
        ends_stroke: bool,
        dabs: &[DabPoint],
    ) -> Result<Self, DabChunkBuildError> {
        if dabs.len() > DAB_QUEUE_CHUNK_CAPACITY {
            return Err(DabChunkBuildError::TooManyDabs);
        }
        let mut timestamp_micros = [0; DAB_QUEUE_CHUNK_CAPACITY];
        let mut canvas_x = [0.0; DAB_QUEUE_CHUNK_CAPACITY];
        let mut canvas_y = [0.0; DAB_QUEUE_CHUNK_CAPACITY];
        let mut pressure = [0.0; DAB_QUEUE_CHUNK_CAPACITY];
        let mut velocity_pixels_per_second = [0.0; DAB_QUEUE_CHUNK_CAPACITY];
        let mut tilt_x_degrees = [0.0; DAB_QUEUE_CHUNK_CAPACITY];
        let mut tilt_y_degrees = [0.0; DAB_QUEUE_CHUNK_CAPACITY];
        let mut twist_degrees = [0.0; DAB_QUEUE_CHUNK_CAPACITY];

        for (index, dab) in dabs.iter().enumerate() {
            timestamp_micros[index] = dab.timestamp_micros;
            canvas_x[index] = dab.canvas_x;
            canvas_y[index] = dab.canvas_y;
            pressure[index] = dab.pressure;
            velocity_pixels_per_second[index] = dab.velocity_pixels_per_second;
            tilt_x_degrees[index] = dab.tilt_x_degrees;
            tilt_y_degrees[index] = dab.tilt_y_degrees;
            twist_degrees[index] = dab.twist_degrees;
        }

        Ok(Self {
            stroke_session_id,
            pointer_id,
            starts_stroke,
            ends_stroke,
            discontinuity_before: false,
            dropped_chunk_count_before: 0,
            len: dabs.len() as u8,
            timestamp_micros,
            canvas_x,
            canvas_y,
            pressure,
            velocity_pixels_per_second,
            tilt_x_degrees,
            tilt_y_degrees,
            twist_degrees,
        })
    }

    pub fn dab_count(&self) -> usize {
        self.len as usize
    }

    pub fn timestamp_micros(&self) -> &[EventTimestampMicros] {
        &self.timestamp_micros[..self.dab_count()]
    }

    pub fn canvas_x(&self) -> &[f32] {
        &self.canvas_x[..self.dab_count()]
    }

    pub fn canvas_y(&self) -> &[f32] {
        &self.canvas_y[..self.dab_count()]
    }

    pub fn pressure(&self) -> &[f32] {
        &self.pressure[..self.dab_count()]
    }

    pub fn velocity_pixels_per_second(&self) -> &[f32] {
        &self.velocity_pixels_per_second[..self.dab_count()]
    }

    pub fn tilt_x_degrees(&self) -> &[f32] {
        &self.tilt_x_degrees[..self.dab_count()]
    }

    pub fn tilt_y_degrees(&self) -> &[f32] {
        &self.tilt_y_degrees[..self.dab_count()]
    }

    pub fn twist_degrees(&self) -> &[f32] {
        &self.twist_degrees[..self.dab_count()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DabChunkQueueCreateError {
    ZeroCapacity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DabChunkQueuePushError {
    Full,
}

#[derive(Debug)]
pub struct DabChunkSender {
    producer: rtrb::Producer<DabChunk>,
}

#[derive(Debug)]
pub struct DabChunkReceiver {
    consumer: rtrb::Consumer<DabChunk>,
}

#[derive(Debug)]
pub struct DabChunkRealtimeQueue {
    producer: rtrb::Producer<DabChunk>,
    consumer: rtrb::Consumer<DabChunk>,
}

pub fn create_dab_chunk_ring_buffer(
    capacity: usize,
) -> Result<(DabChunkSender, DabChunkReceiver), DabChunkQueueCreateError> {
    if capacity == 0 {
        return Err(DabChunkQueueCreateError::ZeroCapacity);
    }
    let (producer, consumer) = rtrb::RingBuffer::new(capacity);
    Ok((DabChunkSender { producer }, DabChunkReceiver { consumer }))
}

pub fn create_dab_chunk_realtime_queue(
    capacity: usize,
) -> Result<DabChunkRealtimeQueue, DabChunkQueueCreateError> {
    if capacity == 0 {
        return Err(DabChunkQueueCreateError::ZeroCapacity);
    }
    let (producer, consumer) = rtrb::RingBuffer::new(capacity);
    Ok(DabChunkRealtimeQueue { producer, consumer })
}

pub trait DabChunkSink {
    fn push_dab_chunk(&mut self, chunk: DabChunk) -> Result<(), DabChunkQueuePushError>;
}

pub trait DabEmitter {
    fn emit_dab(&mut self, dab: DabPoint) -> Result<(), DabSamplingError>;
}

impl DabChunkSink for DabChunkSender {
    fn push_dab_chunk(&mut self, chunk: DabChunk) -> Result<(), DabChunkQueuePushError> {
        self.producer
            .push(chunk)
            .map_err(|_| DabChunkQueuePushError::Full)
    }
}

impl DabChunkReceiver {
    pub fn pop_dab_chunk(&mut self) -> Option<DabChunk> {
        self.consumer.pop().ok()
    }
}

impl DabChunkRealtimeQueue {
    pub fn pop_dab_chunk(&mut self) -> Option<DabChunk> {
        self.consumer.pop().ok()
    }
}

impl DabChunkSink for DabChunkRealtimeQueue {
    fn push_dab_chunk(&mut self, chunk: DabChunk) -> Result<(), DabChunkQueuePushError> {
        let mut pending_chunk = chunk;
        let mut dropped_chunk_count = 0u16;
        loop {
            match self.producer.push(pending_chunk) {
                Ok(()) => {
                    return Ok(());
                }
                Err(rtrb::PushError::Full(returned_chunk)) => {
                    pending_chunk = returned_chunk;
                    match self.consumer.pop() {
                        Ok(_) => {
                            dropped_chunk_count = dropped_chunk_count
                                .checked_add(1)
                                .expect("dropped chunk count overflow");
                            pending_chunk.discontinuity_before = true;
                            pending_chunk.dropped_chunk_count_before = dropped_chunk_count;
                        }
                        Err(rtrb::PopError::Empty) => {
                            return Err(DabChunkQueuePushError::Full);
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrokeContext {
    pub stroke_session_id: StrokeSessionId,
    pub pointer_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DabSamplingError {
    InvalidInput,
    NonMonotonicTimestamp,
    ChunkCapacityExceeded,
    QueueFull,
}

impl From<DabChunkBuildError> for DabSamplingError {
    fn from(error: DabChunkBuildError) -> Self {
        match error {
            DabChunkBuildError::TooManyDabs => Self::ChunkCapacityExceeded,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DabChunkBuilder {
    chunk: DabChunk,
}

impl DabChunkBuilder {
    pub fn new(
        stroke_session_id: StrokeSessionId,
        pointer_id: u64,
        starts_stroke: bool,
        ends_stroke: bool,
    ) -> Self {
        Self {
            chunk: DabChunk {
                stroke_session_id,
                pointer_id,
                starts_stroke,
                ends_stroke,
                discontinuity_before: false,
                dropped_chunk_count_before: 0,
                len: 0,
                timestamp_micros: [0; DAB_QUEUE_CHUNK_CAPACITY],
                canvas_x: [0.0; DAB_QUEUE_CHUNK_CAPACITY],
                canvas_y: [0.0; DAB_QUEUE_CHUNK_CAPACITY],
                pressure: [0.0; DAB_QUEUE_CHUNK_CAPACITY],
                velocity_pixels_per_second: [0.0; DAB_QUEUE_CHUNK_CAPACITY],
                tilt_x_degrees: [0.0; DAB_QUEUE_CHUNK_CAPACITY],
                tilt_y_degrees: [0.0; DAB_QUEUE_CHUNK_CAPACITY],
                twist_degrees: [0.0; DAB_QUEUE_CHUNK_CAPACITY],
            },
        }
    }

    pub fn dab_count(&self) -> usize {
        self.chunk.dab_count()
    }

    pub fn is_empty(&self) -> bool {
        self.dab_count() == 0
    }

    pub fn is_full(&self) -> bool {
        self.dab_count() >= DAB_QUEUE_CHUNK_CAPACITY
    }

    pub fn push_dab(&mut self, dab: DabPoint) -> Result<(), DabChunkBuildError> {
        let next_index = self.chunk.dab_count();
        if next_index >= DAB_QUEUE_CHUNK_CAPACITY {
            return Err(DabChunkBuildError::TooManyDabs);
        }
        self.chunk.timestamp_micros[next_index] = dab.timestamp_micros;
        self.chunk.canvas_x[next_index] = dab.canvas_x;
        self.chunk.canvas_y[next_index] = dab.canvas_y;
        self.chunk.pressure[next_index] = dab.pressure;
        self.chunk.velocity_pixels_per_second[next_index] = dab.velocity_pixels_per_second;
        self.chunk.tilt_x_degrees[next_index] = dab.tilt_x_degrees;
        self.chunk.tilt_y_degrees[next_index] = dab.tilt_y_degrees;
        self.chunk.twist_degrees[next_index] = dab.twist_degrees;
        self.chunk.len = self
            .chunk
            .len
            .checked_add(1)
            .expect("dab chunk len overflow");
        Ok(())
    }

    pub fn finish(self) -> DabChunk {
        self.chunk
    }
}

impl DabEmitter for DabChunkBuilder {
    fn emit_dab(&mut self, dab: DabPoint) -> Result<(), DabSamplingError> {
        self.push_dab(dab)?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct StrokeChunkSplitter {
    stroke_context: StrokeContext,
    current_builder: DabChunkBuilder,
    emitted_chunk_count: usize,
}

impl StrokeChunkSplitter {
    pub fn new(stroke_context: StrokeContext) -> Self {
        Self {
            stroke_context,
            current_builder: DabChunkBuilder::new(
                stroke_context.stroke_session_id,
                stroke_context.pointer_id,
                true,
                false,
            ),
            emitted_chunk_count: 0,
        }
    }

    pub fn push_dab(
        &mut self,
        dab: DabPoint,
        queue: &mut impl DabChunkSink,
    ) -> Result<(), DabSamplingError> {
        if self.current_builder.is_full() {
            self.flush_current_chunk(queue, false)?;
        }
        self.current_builder.push_dab(dab)?;
        Ok(())
    }

    pub fn finish_stroke(&mut self, queue: &mut impl DabChunkSink) -> Result<(), DabSamplingError> {
        if self.current_builder.is_empty() {
            return Ok(());
        }
        self.flush_current_chunk(queue, true)
    }

    pub fn emitted_chunk_count(&self) -> usize {
        self.emitted_chunk_count
    }

    fn flush_current_chunk(
        &mut self,
        queue: &mut impl DabChunkSink,
        ends_stroke: bool,
    ) -> Result<(), DabSamplingError> {
        let next_builder = DabChunkBuilder::new(
            self.stroke_context.stroke_session_id,
            self.stroke_context.pointer_id,
            false,
            false,
        );
        let builder = std::mem::replace(&mut self.current_builder, next_builder);
        let mut chunk = builder.finish();
        if chunk.dab_count() == 0 {
            return Ok(());
        }
        chunk.ends_stroke = ends_stroke;
        queue.push_dab_chunk(chunk).map_err(|_| {
            // Queue saturation should be handled by brush execution with a low-latency policy
            // (for example: drop oldest queued chunks and fast-forward to newer stroke data).
            DabSamplingError::QueueFull
        })?;
        self.emitted_chunk_count = self
            .emitted_chunk_count
            .checked_add(1)
            .expect("stroke emitted chunk count overflow");
        Ok(())
    }
}

pub trait DabSamplingAlgorithm {
    type Config;

    fn begin_stroke(
        &mut self,
        context: StrokeContext,
        config: &Self::Config,
    ) -> Result<(), DabSamplingError>;

    fn feed_input<E>(
        &mut self,
        input: RawPointerInput,
        emitter: &mut E,
    ) -> Result<(), DabSamplingError>
    where
        E: DabEmitter;

    fn end_stroke<E>(&mut self, emitter: &mut E) -> Result<(), DabSamplingError>
    where
        E: DabEmitter;
}

#[derive(Debug)]
pub struct DriverSamplingPipeline<A>
where
    A: DabSamplingAlgorithm,
{
    algorithm: A,
    config: A::Config,
}

impl<A> DriverSamplingPipeline<A>
where
    A: DabSamplingAlgorithm,
{
    pub fn new(algorithm: A, config: A::Config) -> Self {
        Self { algorithm, config }
    }

    pub fn config(&self) -> &A::Config {
        &self.config
    }

    pub fn set_config(&mut self, config: A::Config) {
        self.config = config;
    }

    pub fn algorithm_mut(&mut self) -> &mut A {
        &mut self.algorithm
    }

    pub fn begin_stroke(&mut self, context: StrokeContext) -> Result<(), DabSamplingError> {
        self.algorithm.begin_stroke(context, &self.config)
    }

    pub fn feed_input<E>(
        &mut self,
        input: RawPointerInput,
        emitter: &mut E,
    ) -> Result<(), DabSamplingError>
    where
        E: DabEmitter,
    {
        self.algorithm.feed_input(input, emitter)
    }

    pub fn end_stroke<E>(&mut self, emitter: &mut E) -> Result<(), DabSamplingError>
    where
        E: DabEmitter,
    {
        self.algorithm.end_stroke(emitter)
    }
}

struct ChunkingEmitter<'a, S>
where
    S: DabChunkSink,
{
    splitter: &'a mut StrokeChunkSplitter,
    queue: &'a mut S,
}

impl<S> DabEmitter for ChunkingEmitter<'_, S>
where
    S: DabChunkSink,
{
    fn emit_dab(&mut self, dab: DabPoint) -> Result<(), DabSamplingError> {
        self.splitter.push_dab(dab, self.queue)
    }
}

pub struct DriverStrokeSession<A>
where
    A: DabSamplingAlgorithm,
{
    context: StrokeContext,
    sampling_pipeline: DriverSamplingPipeline<A>,
    splitter: StrokeChunkSplitter,
}

impl<A> DriverStrokeSession<A>
where
    A: DabSamplingAlgorithm,
{
    pub fn new(
        context: StrokeContext,
        algorithm: A,
        config: A::Config,
    ) -> Result<Self, DabSamplingError> {
        let mut sampling_pipeline = DriverSamplingPipeline::new(algorithm, config);
        sampling_pipeline.begin_stroke(context)?;
        Ok(Self {
            context,
            sampling_pipeline,
            splitter: StrokeChunkSplitter::new(context),
        })
    }

    pub fn stroke_context(&self) -> StrokeContext {
        self.context
    }

    pub fn emitted_chunk_count(&self) -> usize {
        self.splitter.emitted_chunk_count()
    }

    pub fn feed_input<S>(
        &mut self,
        input: RawPointerInput,
        queue: &mut S,
    ) -> Result<(), DabSamplingError>
    where
        S: DabChunkSink,
    {
        let mut emitter = ChunkingEmitter {
            splitter: &mut self.splitter,
            queue,
        };
        self.sampling_pipeline.feed_input(input, &mut emitter)
    }

    pub fn end_stroke<S>(&mut self, queue: &mut S) -> Result<(), DabSamplingError>
    where
        S: DabChunkSink,
    {
        let mut emitter = ChunkingEmitter {
            splitter: &mut self.splitter,
            queue,
        };
        self.sampling_pipeline.end_stroke(&mut emitter)?;
        self.splitter.finish_stroke(queue)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverEventError {
    StrokeAlreadyActive,
    NoActiveStroke,
    PointerIdMismatch,
    QueueCreate(DabChunkQueueCreateError),
    Sampling(DabSamplingError),
}

impl From<DabSamplingError> for DriverEventError {
    fn from(error: DabSamplingError) -> Self {
        Self::Sampling(error)
    }
}

pub struct DriverEngine<A>
where
    A: DabSamplingAlgorithm,
    A::Config: Clone,
{
    queue: DabChunkRealtimeQueue,
    active_stroke: Option<DriverStrokeSession<A>>,
    next_stroke_session_id: StrokeSessionId,
    algorithm_factory: Box<dyn Fn() -> A>,
    algorithm_config: A::Config,
}

impl<A> DriverEngine<A>
where
    A: DabSamplingAlgorithm,
    A::Config: Clone,
{
    pub fn new(
        queue_capacity: usize,
        algorithm_factory: impl Fn() -> A + 'static,
        algorithm_config: A::Config,
    ) -> Result<Self, DriverEventError> {
        let queue = create_dab_chunk_realtime_queue(queue_capacity)
            .map_err(DriverEventError::QueueCreate)?;
        Ok(Self {
            queue,
            active_stroke: None,
            next_stroke_session_id: 1,
            algorithm_factory: Box::new(algorithm_factory),
            algorithm_config,
        })
    }

    pub fn handle_pointer_event(&mut self, input: RawPointerInput) -> Result<(), DriverEventError> {
        match input.phase {
            PointerEventPhase::Down => self.handle_pointer_down(input),
            PointerEventPhase::Move => self.handle_pointer_move(input),
            PointerEventPhase::Up => self.handle_pointer_up(input),
            PointerEventPhase::Cancel => self.handle_pointer_cancel(input),
            PointerEventPhase::Hover => Ok(()),
        }
    }

    pub fn dispatch_frame(&mut self, signal: FrameDispatchSignal) -> Vec<FramedDabChunk> {
        let mut output = Vec::new();
        while let Some(chunk) = self.queue.pop_dab_chunk() {
            output.push(FramedDabChunk {
                frame_sequence_id: signal.frame_sequence_id,
                chunk,
            });
        }
        output
    }

    fn handle_pointer_down(&mut self, input: RawPointerInput) -> Result<(), DriverEventError> {
        if self.active_stroke.is_some() {
            return Err(DriverEventError::StrokeAlreadyActive);
        }

        let stroke_context = StrokeContext {
            stroke_session_id: self.next_stroke_session_id,
            pointer_id: input.pointer_id,
        };
        self.next_stroke_session_id = self
            .next_stroke_session_id
            .checked_add(1)
            .expect("stroke session id overflow");

        let mut session = DriverStrokeSession::new(
            stroke_context,
            (self.algorithm_factory)(),
            self.algorithm_config.clone(),
        )?;
        session.feed_input(input, &mut self.queue)?;
        self.active_stroke = Some(session);
        Ok(())
    }

    fn handle_pointer_move(&mut self, input: RawPointerInput) -> Result<(), DriverEventError> {
        let session = self
            .active_stroke
            .as_mut()
            .ok_or(DriverEventError::NoActiveStroke)?;
        if session.stroke_context().pointer_id != input.pointer_id {
            return Err(DriverEventError::PointerIdMismatch);
        }
        session.feed_input(input, &mut self.queue)?;
        Ok(())
    }

    fn handle_pointer_up(&mut self, input: RawPointerInput) -> Result<(), DriverEventError> {
        let mut session = self
            .active_stroke
            .take()
            .ok_or(DriverEventError::NoActiveStroke)?;
        if session.stroke_context().pointer_id != input.pointer_id {
            self.active_stroke = Some(session);
            return Err(DriverEventError::PointerIdMismatch);
        }
        session.feed_input(input, &mut self.queue)?;
        session.end_stroke(&mut self.queue)?;
        Ok(())
    }

    fn handle_pointer_cancel(&mut self, input: RawPointerInput) -> Result<(), DriverEventError> {
        let mut session = self
            .active_stroke
            .take()
            .ok_or(DriverEventError::NoActiveStroke)?;
        if session.stroke_context().pointer_id != input.pointer_id {
            self.active_stroke = Some(session);
            return Err(DriverEventError::PointerIdMismatch);
        }
        session.end_stroke(&mut self.queue)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dab(index: usize) -> DabPoint {
        DabPoint {
            timestamp_micros: index as u64,
            canvas_x: index as f32,
            canvas_y: index as f32,
            pressure: 0.5,
            velocity_pixels_per_second: 120.0,
            tilt_x_degrees: 0.0,
            tilt_y_degrees: 0.0,
            twist_degrees: 0.0,
        }
    }

    fn test_context() -> StrokeContext {
        StrokeContext {
            stroke_session_id: 42,
            pointer_id: 7,
        }
    }

    fn test_pointer_input(
        phase: PointerEventPhase,
        timestamp_micros: u64,
        pointer_id: u64,
        x: f32,
        y: f32,
    ) -> RawPointerInput {
        RawPointerInput {
            pointer_id,
            device_kind: PointerDeviceKind::Mouse,
            phase,
            timestamp_micros,
            screen_x: x,
            screen_y: y,
            pressure: Some(1.0),
            tilt_x_degrees: None,
            tilt_y_degrees: None,
            twist_degrees: None,
        }
    }

    #[test]
    fn splitter_emits_single_chunk_for_one_dab() {
        let (mut queue_sender, mut queue_receiver) =
            create_dab_chunk_ring_buffer(8).expect("create queue");
        let mut splitter = StrokeChunkSplitter::new(test_context());

        splitter
            .push_dab(test_dab(0), &mut queue_sender)
            .expect("push dab");
        splitter
            .finish_stroke(&mut queue_sender)
            .expect("finish stroke");

        assert_eq!(splitter.emitted_chunk_count(), 1);
        let chunk = queue_receiver
            .pop_dab_chunk()
            .expect("single chunk expected");
        assert_eq!(chunk.dab_count(), 1);
        assert!(chunk.starts_stroke);
        assert!(chunk.ends_stroke);
        assert!(queue_receiver.pop_dab_chunk().is_none());
    }

    #[test]
    fn splitter_emits_two_chunks_for_seventeen_dabs() {
        let (mut queue_sender, mut queue_receiver) =
            create_dab_chunk_ring_buffer(8).expect("create queue");
        let mut splitter = StrokeChunkSplitter::new(test_context());

        for index in 0..17 {
            splitter
                .push_dab(test_dab(index), &mut queue_sender)
                .expect("push dab");
        }
        splitter
            .finish_stroke(&mut queue_sender)
            .expect("finish stroke");

        assert_eq!(splitter.emitted_chunk_count(), 2);
        let first_chunk = queue_receiver
            .pop_dab_chunk()
            .expect("first chunk expected");
        let second_chunk = queue_receiver
            .pop_dab_chunk()
            .expect("second chunk expected");

        assert_eq!(first_chunk.dab_count(), 16);
        assert!(first_chunk.starts_stroke);
        assert!(!first_chunk.ends_stroke);

        assert_eq!(second_chunk.dab_count(), 1);
        assert!(!second_chunk.starts_stroke);
        assert!(second_chunk.ends_stroke);
        assert!(queue_receiver.pop_dab_chunk().is_none());
    }

    #[test]
    fn splitter_emits_two_chunks_for_thirty_two_dabs() {
        let (mut queue_sender, mut queue_receiver) =
            create_dab_chunk_ring_buffer(8).expect("create queue");
        let mut splitter = StrokeChunkSplitter::new(test_context());

        for index in 0..32 {
            splitter
                .push_dab(test_dab(index), &mut queue_sender)
                .expect("push dab");
        }
        splitter
            .finish_stroke(&mut queue_sender)
            .expect("finish stroke");

        assert_eq!(splitter.emitted_chunk_count(), 2);
        let first_chunk = queue_receiver
            .pop_dab_chunk()
            .expect("first chunk expected");
        let second_chunk = queue_receiver
            .pop_dab_chunk()
            .expect("second chunk expected");

        assert_eq!(first_chunk.dab_count(), 16);
        assert!(first_chunk.starts_stroke);
        assert!(!first_chunk.ends_stroke);

        assert_eq!(second_chunk.dab_count(), 16);
        assert!(!second_chunk.starts_stroke);
        assert!(second_chunk.ends_stroke);
        assert!(queue_receiver.pop_dab_chunk().is_none());
    }

    #[test]
    fn finish_stroke_without_dabs_emits_no_chunk() {
        let (mut queue_sender, mut queue_receiver) =
            create_dab_chunk_ring_buffer(8).expect("create queue");
        let mut splitter = StrokeChunkSplitter::new(test_context());

        splitter
            .finish_stroke(&mut queue_sender)
            .expect("finish stroke");

        assert_eq!(splitter.emitted_chunk_count(), 0);
        assert!(queue_receiver.pop_dab_chunk().is_none());
    }

    #[test]
    fn create_ring_buffer_rejects_zero_capacity() {
        let error = create_dab_chunk_ring_buffer(0).expect_err("zero capacity should fail");
        assert_eq!(error, DabChunkQueueCreateError::ZeroCapacity);
    }

    #[test]
    fn realtime_queue_marks_discontinuity_when_dropping_old_chunks() {
        let mut queue = create_dab_chunk_realtime_queue(1).expect("create realtime queue");
        let mut splitter = StrokeChunkSplitter::new(test_context());

        for index in 0..17 {
            splitter
                .push_dab(test_dab(index), &mut queue)
                .expect("push dab");
        }
        splitter.finish_stroke(&mut queue).expect("finish stroke");

        let only_chunk = queue
            .pop_dab_chunk()
            .expect("realtime queue should retain newest chunk");
        assert_eq!(only_chunk.dab_count(), 1);
        assert!(only_chunk.discontinuity_before);
        assert_eq!(only_chunk.dropped_chunk_count_before, 1);
        assert!(queue.pop_dab_chunk().is_none());
    }

    #[test]
    fn driver_engine_requires_down_before_move() {
        let mut engine = DriverEngine::new(
            8,
            NoSmoothingUniformResampling::new,
            NoSmoothingUniformResamplingConfig {
                spacing_pixels: 1.0,
            },
        )
        .expect("create driver engine");

        let error = engine
            .handle_pointer_event(test_pointer_input(PointerEventPhase::Move, 1, 1, 0.0, 0.0))
            .expect_err("move without down should fail");
        assert_eq!(error, DriverEventError::NoActiveStroke);
    }

    #[test]
    fn driver_engine_assigns_frame_id_on_dispatch() {
        let mut engine = DriverEngine::new(
            8,
            NoSmoothingUniformResampling::new,
            NoSmoothingUniformResamplingConfig {
                spacing_pixels: 1.0,
            },
        )
        .expect("create driver engine");

        engine
            .handle_pointer_event(test_pointer_input(PointerEventPhase::Down, 1, 1, 0.0, 0.0))
            .expect("stroke one down");
        engine
            .handle_pointer_event(test_pointer_input(PointerEventPhase::Move, 2, 1, 2.0, 0.0))
            .expect("stroke one move");
        engine
            .handle_pointer_event(test_pointer_input(PointerEventPhase::Up, 3, 1, 3.0, 0.0))
            .expect("stroke one up");

        let first_dispatch = engine.dispatch_frame(FrameDispatchSignal {
            frame_sequence_id: 10,
        });
        assert!(!first_dispatch.is_empty());
        assert!(first_dispatch
            .iter()
            .all(|chunk| chunk.frame_sequence_id == 10));

        engine
            .handle_pointer_event(test_pointer_input(PointerEventPhase::Down, 4, 1, 10.0, 0.0))
            .expect("stroke two down");
        engine
            .handle_pointer_event(test_pointer_input(PointerEventPhase::Move, 5, 1, 12.0, 0.0))
            .expect("stroke two move");
        engine
            .handle_pointer_event(test_pointer_input(PointerEventPhase::Up, 6, 1, 13.0, 0.0))
            .expect("stroke two up");

        let second_dispatch = engine.dispatch_frame(FrameDispatchSignal {
            frame_sequence_id: 11,
        });
        assert!(!second_dispatch.is_empty());
        assert!(second_dispatch
            .iter()
            .all(|chunk| chunk.frame_sequence_id == 11));
    }
}
