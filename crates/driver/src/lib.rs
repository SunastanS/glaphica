pub mod no_smoothing_uniform_resampling;

pub use no_smoothing_uniform_resampling::{
    NoSmoothingUniformResampling, NoSmoothingUniformResamplingConfig,
};

pub type StrokeSessionId = u64;
pub type EventTimestampMicros = u64;
pub type FrameSequenceId = u64;

pub const SAMPLE_QUEUE_CHUNK_CAPACITY: usize = 16;

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
pub struct StrokeSample {
    pub timestamp_micros: EventTimestampMicros,
    pub canvas_x: f32,
    pub canvas_y: f32,
    pub pressure: f32,
    pub velocity_pixels_per_second: f32,
    pub tilt_x_degrees: f32,
    pub tilt_y_degrees: f32,
    pub twist_degrees: f32,
}

impl Default for StrokeSample {
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
    pub sample_points: Vec<StrokeSample>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameDispatchSignal {
    pub frame_sequence_id: FrameSequenceId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FramedSampleChunk {
    pub frame_sequence_id: FrameSequenceId,
    pub chunk: SampleChunk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleChunkBuildError {
    TooManySamples,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SampleChunk {
    pub stroke_session_id: StrokeSessionId,
    pub pointer_id: u64,
    pub starts_stroke: bool,
    pub ends_stroke: bool,
    pub discontinuity_before: bool,
    pub dropped_chunk_count_before: u16,
    len: u8,
    timestamp_micros: [EventTimestampMicros; SAMPLE_QUEUE_CHUNK_CAPACITY],
    canvas_x: [f32; SAMPLE_QUEUE_CHUNK_CAPACITY],
    canvas_y: [f32; SAMPLE_QUEUE_CHUNK_CAPACITY],
    pressure: [f32; SAMPLE_QUEUE_CHUNK_CAPACITY],
    velocity_pixels_per_second: [f32; SAMPLE_QUEUE_CHUNK_CAPACITY],
    tilt_x_degrees: [f32; SAMPLE_QUEUE_CHUNK_CAPACITY],
    tilt_y_degrees: [f32; SAMPLE_QUEUE_CHUNK_CAPACITY],
    twist_degrees: [f32; SAMPLE_QUEUE_CHUNK_CAPACITY],
}

impl SampleChunk {
    pub fn from_samples(
        stroke_session_id: StrokeSessionId,
        pointer_id: u64,
        starts_stroke: bool,
        ends_stroke: bool,
        samples: &[StrokeSample],
    ) -> Result<Self, SampleChunkBuildError> {
        if samples.len() > SAMPLE_QUEUE_CHUNK_CAPACITY {
            return Err(SampleChunkBuildError::TooManySamples);
        }
        let mut timestamp_micros = [0; SAMPLE_QUEUE_CHUNK_CAPACITY];
        let mut canvas_x = [0.0; SAMPLE_QUEUE_CHUNK_CAPACITY];
        let mut canvas_y = [0.0; SAMPLE_QUEUE_CHUNK_CAPACITY];
        let mut pressure = [0.0; SAMPLE_QUEUE_CHUNK_CAPACITY];
        let mut velocity_pixels_per_second = [0.0; SAMPLE_QUEUE_CHUNK_CAPACITY];
        let mut tilt_x_degrees = [0.0; SAMPLE_QUEUE_CHUNK_CAPACITY];
        let mut tilt_y_degrees = [0.0; SAMPLE_QUEUE_CHUNK_CAPACITY];
        let mut twist_degrees = [0.0; SAMPLE_QUEUE_CHUNK_CAPACITY];

        for (index, sample) in samples.iter().enumerate() {
            timestamp_micros[index] = sample.timestamp_micros;
            canvas_x[index] = sample.canvas_x;
            canvas_y[index] = sample.canvas_y;
            pressure[index] = sample.pressure;
            velocity_pixels_per_second[index] = sample.velocity_pixels_per_second;
            tilt_x_degrees[index] = sample.tilt_x_degrees;
            tilt_y_degrees[index] = sample.tilt_y_degrees;
            twist_degrees[index] = sample.twist_degrees;
        }

        Ok(Self {
            stroke_session_id,
            pointer_id,
            starts_stroke,
            ends_stroke,
            discontinuity_before: false,
            dropped_chunk_count_before: 0,
            len: samples.len() as u8,
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

    pub fn sample_count(&self) -> usize {
        self.len as usize
    }

    pub fn timestamp_micros(&self) -> &[EventTimestampMicros] {
        &self.timestamp_micros[..self.sample_count()]
    }

    pub fn canvas_x(&self) -> &[f32] {
        &self.canvas_x[..self.sample_count()]
    }

    pub fn canvas_y(&self) -> &[f32] {
        &self.canvas_y[..self.sample_count()]
    }

    pub fn pressure(&self) -> &[f32] {
        &self.pressure[..self.sample_count()]
    }

    pub fn velocity_pixels_per_second(&self) -> &[f32] {
        &self.velocity_pixels_per_second[..self.sample_count()]
    }

    pub fn tilt_x_degrees(&self) -> &[f32] {
        &self.tilt_x_degrees[..self.sample_count()]
    }

    pub fn tilt_y_degrees(&self) -> &[f32] {
        &self.tilt_y_degrees[..self.sample_count()]
    }

    pub fn twist_degrees(&self) -> &[f32] {
        &self.twist_degrees[..self.sample_count()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleChunkQueueCreateError {
    ZeroCapacity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleChunkQueuePushError {
    Full,
}

#[derive(Debug)]
pub struct SampleChunkSender {
    producer: rtrb::Producer<SampleChunk>,
}

#[derive(Debug)]
pub struct SampleChunkReceiver {
    consumer: rtrb::Consumer<SampleChunk>,
}

#[derive(Debug)]
pub struct SampleChunkRealtimeQueue {
    producer: rtrb::Producer<SampleChunk>,
    consumer: rtrb::Consumer<SampleChunk>,
}

pub fn create_sample_chunk_ring_buffer(
    capacity: usize,
) -> Result<(SampleChunkSender, SampleChunkReceiver), SampleChunkQueueCreateError> {
    if capacity == 0 {
        return Err(SampleChunkQueueCreateError::ZeroCapacity);
    }
    let (producer, consumer) = rtrb::RingBuffer::new(capacity);
    Ok((
        SampleChunkSender { producer },
        SampleChunkReceiver { consumer },
    ))
}

pub fn create_sample_chunk_realtime_queue(
    capacity: usize,
) -> Result<SampleChunkRealtimeQueue, SampleChunkQueueCreateError> {
    if capacity == 0 {
        return Err(SampleChunkQueueCreateError::ZeroCapacity);
    }
    let (producer, consumer) = rtrb::RingBuffer::new(capacity);
    Ok(SampleChunkRealtimeQueue { producer, consumer })
}

pub trait SampleChunkSink {
    fn push_sample_chunk(&mut self, chunk: SampleChunk) -> Result<(), SampleChunkQueuePushError>;
}

pub trait SampleEmitter {
    fn emit_sample(&mut self, sample: StrokeSample) -> Result<(), SampleProcessingError>;
}

impl SampleChunkSink for SampleChunkSender {
    fn push_sample_chunk(&mut self, chunk: SampleChunk) -> Result<(), SampleChunkQueuePushError> {
        self.producer
            .push(chunk)
            .map_err(|_| SampleChunkQueuePushError::Full)
    }
}

impl SampleChunkReceiver {
    pub fn pop_sample_chunk(&mut self) -> Option<SampleChunk> {
        self.consumer.pop().ok()
    }
}

impl SampleChunkRealtimeQueue {
    pub fn pop_sample_chunk(&mut self) -> Option<SampleChunk> {
        self.consumer.pop().ok()
    }
}

impl SampleChunkSink for SampleChunkRealtimeQueue {
    fn push_sample_chunk(&mut self, chunk: SampleChunk) -> Result<(), SampleChunkQueuePushError> {
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
                            return Err(SampleChunkQueuePushError::Full);
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
pub enum SampleProcessingError {
    InvalidInput,
    NonMonotonicTimestamp,
    ChunkCapacityExceeded,
    QueueFull,
}

impl From<SampleChunkBuildError> for SampleProcessingError {
    fn from(error: SampleChunkBuildError) -> Self {
        match error {
            SampleChunkBuildError::TooManySamples => Self::ChunkCapacityExceeded,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SampleChunkBuilder {
    chunk: SampleChunk,
}

impl SampleChunkBuilder {
    pub fn new(
        stroke_session_id: StrokeSessionId,
        pointer_id: u64,
        starts_stroke: bool,
        ends_stroke: bool,
    ) -> Self {
        Self {
            chunk: SampleChunk {
                stroke_session_id,
                pointer_id,
                starts_stroke,
                ends_stroke,
                discontinuity_before: false,
                dropped_chunk_count_before: 0,
                len: 0,
                timestamp_micros: [0; SAMPLE_QUEUE_CHUNK_CAPACITY],
                canvas_x: [0.0; SAMPLE_QUEUE_CHUNK_CAPACITY],
                canvas_y: [0.0; SAMPLE_QUEUE_CHUNK_CAPACITY],
                pressure: [0.0; SAMPLE_QUEUE_CHUNK_CAPACITY],
                velocity_pixels_per_second: [0.0; SAMPLE_QUEUE_CHUNK_CAPACITY],
                tilt_x_degrees: [0.0; SAMPLE_QUEUE_CHUNK_CAPACITY],
                tilt_y_degrees: [0.0; SAMPLE_QUEUE_CHUNK_CAPACITY],
                twist_degrees: [0.0; SAMPLE_QUEUE_CHUNK_CAPACITY],
            },
        }
    }

    pub fn sample_count(&self) -> usize {
        self.chunk.sample_count()
    }

    pub fn is_empty(&self) -> bool {
        self.sample_count() == 0
    }

    pub fn is_full(&self) -> bool {
        self.sample_count() >= SAMPLE_QUEUE_CHUNK_CAPACITY
    }

    pub fn push_sample(&mut self, sample: StrokeSample) -> Result<(), SampleChunkBuildError> {
        let next_index = self.chunk.sample_count();
        if next_index >= SAMPLE_QUEUE_CHUNK_CAPACITY {
            return Err(SampleChunkBuildError::TooManySamples);
        }
        self.chunk.timestamp_micros[next_index] = sample.timestamp_micros;
        self.chunk.canvas_x[next_index] = sample.canvas_x;
        self.chunk.canvas_y[next_index] = sample.canvas_y;
        self.chunk.pressure[next_index] = sample.pressure;
        self.chunk.velocity_pixels_per_second[next_index] = sample.velocity_pixels_per_second;
        self.chunk.tilt_x_degrees[next_index] = sample.tilt_x_degrees;
        self.chunk.tilt_y_degrees[next_index] = sample.tilt_y_degrees;
        self.chunk.twist_degrees[next_index] = sample.twist_degrees;
        self.chunk.len = self
            .chunk
            .len
            .checked_add(1)
            .expect("sample chunk len overflow");
        Ok(())
    }

    pub fn finish(self) -> SampleChunk {
        self.chunk
    }
}

impl SampleEmitter for SampleChunkBuilder {
    fn emit_sample(&mut self, sample: StrokeSample) -> Result<(), SampleProcessingError> {
        self.push_sample(sample)?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct StrokeChunkSplitter {
    stroke_context: StrokeContext,
    current_builder: SampleChunkBuilder,
    emitted_chunk_count: usize,
}

impl StrokeChunkSplitter {
    pub fn new(stroke_context: StrokeContext) -> Self {
        Self {
            stroke_context,
            current_builder: SampleChunkBuilder::new(
                stroke_context.stroke_session_id,
                stroke_context.pointer_id,
                true,
                false,
            ),
            emitted_chunk_count: 0,
        }
    }

    pub fn push_sample(
        &mut self,
        sample: StrokeSample,
        queue: &mut impl SampleChunkSink,
    ) -> Result<(), SampleProcessingError> {
        if self.current_builder.is_full() {
            self.flush_current_chunk(queue, false)?;
        }
        self.current_builder.push_sample(sample)?;
        Ok(())
    }

    pub fn finish_stroke(
        &mut self,
        queue: &mut impl SampleChunkSink,
    ) -> Result<(), SampleProcessingError> {
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
        queue: &mut impl SampleChunkSink,
        ends_stroke: bool,
    ) -> Result<(), SampleProcessingError> {
        let next_builder = SampleChunkBuilder::new(
            self.stroke_context.stroke_session_id,
            self.stroke_context.pointer_id,
            false,
            false,
        );
        let builder = std::mem::replace(&mut self.current_builder, next_builder);
        let mut chunk = builder.finish();
        if chunk.sample_count() == 0 {
            return Ok(());
        }
        chunk.ends_stroke = ends_stroke;
        queue.push_sample_chunk(chunk).map_err(|_| {
            // Queue saturation should be handled by brush execution with a low-latency policy
            // (for example: drop oldest queued chunks and fast-forward to newer stroke data).
            SampleProcessingError::QueueFull
        })?;
        self.emitted_chunk_count = self
            .emitted_chunk_count
            .checked_add(1)
            .expect("stroke emitted chunk count overflow");
        Ok(())
    }
}

pub trait InputSamplingAlgorithm {
    type Config;

    fn begin_stroke(
        &mut self,
        context: StrokeContext,
        config: &Self::Config,
    ) -> Result<(), SampleProcessingError>;

    fn feed_input<E>(
        &mut self,
        input: RawPointerInput,
        emitter: &mut E,
    ) -> Result<(), SampleProcessingError>
    where
        E: SampleEmitter;

    fn end_stroke<E>(&mut self, emitter: &mut E) -> Result<(), SampleProcessingError>
    where
        E: SampleEmitter;
}

#[derive(Debug)]
pub struct DriverSamplingPipeline<A>
where
    A: InputSamplingAlgorithm,
{
    algorithm: A,
    config: A::Config,
}

impl<A> DriverSamplingPipeline<A>
where
    A: InputSamplingAlgorithm,
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

    pub fn begin_stroke(&mut self, context: StrokeContext) -> Result<(), SampleProcessingError> {
        self.algorithm.begin_stroke(context, &self.config)
    }

    pub fn feed_input<E>(
        &mut self,
        input: RawPointerInput,
        emitter: &mut E,
    ) -> Result<(), SampleProcessingError>
    where
        E: SampleEmitter,
    {
        self.algorithm.feed_input(input, emitter)
    }

    pub fn end_stroke<E>(&mut self, emitter: &mut E) -> Result<(), SampleProcessingError>
    where
        E: SampleEmitter,
    {
        self.algorithm.end_stroke(emitter)
    }
}

struct ChunkingEmitter<'a, S>
where
    S: SampleChunkSink,
{
    splitter: &'a mut StrokeChunkSplitter,
    queue: &'a mut S,
}

impl<S> SampleEmitter for ChunkingEmitter<'_, S>
where
    S: SampleChunkSink,
{
    fn emit_sample(&mut self, sample: StrokeSample) -> Result<(), SampleProcessingError> {
        self.splitter.push_sample(sample, self.queue)
    }
}

pub struct DriverStrokeSession<A>
where
    A: InputSamplingAlgorithm,
{
    context: StrokeContext,
    sampling_pipeline: DriverSamplingPipeline<A>,
    splitter: StrokeChunkSplitter,
}

impl<A> DriverStrokeSession<A>
where
    A: InputSamplingAlgorithm,
{
    pub fn new(
        context: StrokeContext,
        algorithm: A,
        config: A::Config,
    ) -> Result<Self, SampleProcessingError> {
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
    ) -> Result<(), SampleProcessingError>
    where
        S: SampleChunkSink,
    {
        let mut emitter = ChunkingEmitter {
            splitter: &mut self.splitter,
            queue,
        };
        self.sampling_pipeline.feed_input(input, &mut emitter)
    }

    pub fn end_stroke<S>(&mut self, queue: &mut S) -> Result<(), SampleProcessingError>
    where
        S: SampleChunkSink,
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
    QueueCreate(SampleChunkQueueCreateError),
    Sampling(SampleProcessingError),
}

impl From<SampleProcessingError> for DriverEventError {
    fn from(error: SampleProcessingError) -> Self {
        Self::Sampling(error)
    }
}

pub struct DriverEngine<A>
where
    A: InputSamplingAlgorithm,
    A::Config: Clone,
{
    queue: SampleChunkRealtimeQueue,
    active_stroke: Option<DriverStrokeSession<A>>,
    next_stroke_session_id: StrokeSessionId,
    algorithm_factory: Box<dyn Fn() -> A>,
    algorithm_config: A::Config,
}

impl<A> DriverEngine<A>
where
    A: InputSamplingAlgorithm,
    A::Config: Clone,
{
    pub fn new(
        queue_capacity: usize,
        algorithm_factory: impl Fn() -> A + 'static,
        algorithm_config: A::Config,
    ) -> Result<Self, DriverEventError> {
        let queue = create_sample_chunk_realtime_queue(queue_capacity)
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

    pub fn dispatch_frame(&mut self, signal: FrameDispatchSignal) -> Vec<FramedSampleChunk> {
        let mut output = Vec::new();
        while let Some(chunk) = self.queue.pop_sample_chunk() {
            output.push(FramedSampleChunk {
                frame_sequence_id: signal.frame_sequence_id,
                chunk,
            });
        }
        output
    }

    pub fn has_active_stroke(&self) -> bool {
        self.active_stroke.is_some()
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

    fn test_sample(index: usize) -> StrokeSample {
        StrokeSample {
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
    fn splitter_emits_single_chunk_for_one_sample() {
        let (mut queue_sender, mut queue_receiver) =
            create_sample_chunk_ring_buffer(8).expect("create queue");
        let mut splitter = StrokeChunkSplitter::new(test_context());

        splitter
            .push_sample(test_sample(0), &mut queue_sender)
            .expect("push sample");
        splitter
            .finish_stroke(&mut queue_sender)
            .expect("finish stroke");

        assert_eq!(splitter.emitted_chunk_count(), 1);
        let chunk = queue_receiver
            .pop_sample_chunk()
            .expect("single chunk expected");
        assert_eq!(chunk.sample_count(), 1);
        assert!(chunk.starts_stroke);
        assert!(chunk.ends_stroke);
        assert!(queue_receiver.pop_sample_chunk().is_none());
    }

    #[test]
    fn splitter_emits_two_chunks_for_seventeen_samples() {
        let (mut queue_sender, mut queue_receiver) =
            create_sample_chunk_ring_buffer(8).expect("create queue");
        let mut splitter = StrokeChunkSplitter::new(test_context());

        for index in 0..17 {
            splitter
                .push_sample(test_sample(index), &mut queue_sender)
                .expect("push sample");
        }
        splitter
            .finish_stroke(&mut queue_sender)
            .expect("finish stroke");

        assert_eq!(splitter.emitted_chunk_count(), 2);
        let first_chunk = queue_receiver
            .pop_sample_chunk()
            .expect("first chunk expected");
        let second_chunk = queue_receiver
            .pop_sample_chunk()
            .expect("second chunk expected");

        assert_eq!(first_chunk.sample_count(), 16);
        assert!(first_chunk.starts_stroke);
        assert!(!first_chunk.ends_stroke);

        assert_eq!(second_chunk.sample_count(), 1);
        assert!(!second_chunk.starts_stroke);
        assert!(second_chunk.ends_stroke);
        assert!(queue_receiver.pop_sample_chunk().is_none());
    }

    #[test]
    fn splitter_emits_two_chunks_for_thirty_two_samples() {
        let (mut queue_sender, mut queue_receiver) =
            create_sample_chunk_ring_buffer(8).expect("create queue");
        let mut splitter = StrokeChunkSplitter::new(test_context());

        for index in 0..32 {
            splitter
                .push_sample(test_sample(index), &mut queue_sender)
                .expect("push sample");
        }
        splitter
            .finish_stroke(&mut queue_sender)
            .expect("finish stroke");

        assert_eq!(splitter.emitted_chunk_count(), 2);
        let first_chunk = queue_receiver
            .pop_sample_chunk()
            .expect("first chunk expected");
        let second_chunk = queue_receiver
            .pop_sample_chunk()
            .expect("second chunk expected");

        assert_eq!(first_chunk.sample_count(), 16);
        assert!(first_chunk.starts_stroke);
        assert!(!first_chunk.ends_stroke);

        assert_eq!(second_chunk.sample_count(), 16);
        assert!(!second_chunk.starts_stroke);
        assert!(second_chunk.ends_stroke);
        assert!(queue_receiver.pop_sample_chunk().is_none());
    }

    #[test]
    fn finish_stroke_without_samples_emits_no_chunk() {
        let (mut queue_sender, mut queue_receiver) =
            create_sample_chunk_ring_buffer(8).expect("create queue");
        let mut splitter = StrokeChunkSplitter::new(test_context());

        splitter
            .finish_stroke(&mut queue_sender)
            .expect("finish stroke");

        assert_eq!(splitter.emitted_chunk_count(), 0);
        assert!(queue_receiver.pop_sample_chunk().is_none());
    }

    #[test]
    fn create_ring_buffer_rejects_zero_capacity() {
        let error = create_sample_chunk_ring_buffer(0).expect_err("zero capacity should fail");
        assert_eq!(error, SampleChunkQueueCreateError::ZeroCapacity);
    }

    #[test]
    fn realtime_queue_marks_discontinuity_when_dropping_old_chunks() {
        let mut queue = create_sample_chunk_realtime_queue(1).expect("create realtime queue");
        let mut splitter = StrokeChunkSplitter::new(test_context());

        for index in 0..17 {
            splitter
                .push_sample(test_sample(index), &mut queue)
                .expect("push sample");
        }
        splitter.finish_stroke(&mut queue).expect("finish stroke");

        let only_chunk = queue
            .pop_sample_chunk()
            .expect("realtime queue should retain newest chunk");
        assert_eq!(only_chunk.sample_count(), 1);
        assert!(only_chunk.discontinuity_before);
        assert_eq!(only_chunk.dropped_chunk_count_before, 1);
        assert!(queue.pop_sample_chunk().is_none());
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
        assert!(
            first_dispatch
                .iter()
                .all(|chunk| chunk.frame_sequence_id == 10)
        );

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
        assert!(
            second_dispatch
                .iter()
                .all(|chunk| chunk.frame_sequence_id == 11)
        );
    }
}
