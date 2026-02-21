use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use driver::FramedSampleChunk;
use render_protocol::{
    BrushBufferMerge, BrushBufferTileAllocate, BrushBufferTileRelease, BrushDabChunkF32, BrushId,
    BrushRenderCommand, BrushStrokeBegin, BrushStrokeEnd, BufferTileCoordinate, LayerId,
    ProgramRevision, ReferenceSetId,
};
use tiles::{BufferTileLifecycle, TileLifecycleManager};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushExecutionQueueCreateError {
    ZeroCapacity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushExecutionQueuePushError {
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrushExecutionStartError {
    InputQueue(BrushExecutionQueueCreateError),
    OutputQueue(BrushExecutionQueueCreateError),
    Config(BrushExecutionConfigError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushExecutionConfigError {
    BufferTileSizeZero,
    MaxAffectedRadiusInvalid,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrushExecutionConfig {
    pub brush_id: BrushId,
    pub program_revision: ProgramRevision,
    pub reference_set_id: ReferenceSetId,
    pub target_layer_id: LayerId,
    pub buffer_tile_size: u32,
    pub max_affected_radius: f32,
}

#[derive(Debug)]
pub struct BrushExecutionSampleSender {
    producer: rtrb::Producer<FramedSampleChunk>,
}

impl BrushExecutionSampleSender {
    pub fn push_chunk(
        &mut self,
        chunk: FramedSampleChunk,
    ) -> Result<(), BrushExecutionQueuePushError> {
        self.producer
            .push(chunk)
            .map_err(|_| BrushExecutionQueuePushError::Full)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrushExecutionMergeFeedback {
    MergeApplied {
        stroke_session_id: u64,
    },
    MergeFailed {
        stroke_session_id: u64,
        message: String,
    },
}

#[derive(Debug)]
pub struct BrushExecutionFeedbackSender {
    producer: rtrb::Producer<BrushExecutionMergeFeedback>,
}

impl BrushExecutionFeedbackSender {
    pub fn push_feedback(
        &mut self,
        feedback: BrushExecutionMergeFeedback,
    ) -> Result<(), BrushExecutionQueuePushError> {
        self.producer
            .push(feedback)
            .map_err(|_| BrushExecutionQueuePushError::Full)
    }
}

#[derive(Debug)]
pub struct BrushExecutionCommandReceiver {
    consumer: rtrb::Consumer<BrushRenderCommand>,
}

impl BrushExecutionCommandReceiver {
    pub fn pop_command(&mut self) -> Option<BrushRenderCommand> {
        self.consumer.pop().ok()
    }
}

pub struct BrushExecutionRuntime {
    stop_requested: Arc<AtomicBool>,
    join_handle: Option<std::thread::JoinHandle<()>>,
}

impl BrushExecutionRuntime {
    pub fn start(
        config: BrushExecutionConfig,
        input_queue_capacity: usize,
        output_queue_capacity: usize,
    ) -> Result<
        (
            Self,
            BrushExecutionSampleSender,
            BrushExecutionFeedbackSender,
            BrushExecutionCommandReceiver,
        ),
        BrushExecutionStartError,
    > {
        if input_queue_capacity == 0 {
            return Err(BrushExecutionStartError::InputQueue(
                BrushExecutionQueueCreateError::ZeroCapacity,
            ));
        }
        if output_queue_capacity == 0 {
            return Err(BrushExecutionStartError::OutputQueue(
                BrushExecutionQueueCreateError::ZeroCapacity,
            ));
        }
        if config.buffer_tile_size == 0 {
            return Err(BrushExecutionStartError::Config(
                BrushExecutionConfigError::BufferTileSizeZero,
            ));
        }
        if !config.max_affected_radius.is_finite() || config.max_affected_radius < 0.0 {
            return Err(BrushExecutionStartError::Config(
                BrushExecutionConfigError::MaxAffectedRadiusInvalid,
            ));
        }
        let (sample_producer, sample_consumer) = rtrb::RingBuffer::new(input_queue_capacity);
        let (feedback_producer, feedback_consumer) = rtrb::RingBuffer::new(input_queue_capacity);
        let (command_producer, command_consumer) = rtrb::RingBuffer::new(output_queue_capacity);
        let stop_requested = Arc::new(AtomicBool::new(false));
        let worker_stop_requested = Arc::clone(&stop_requested);

        let join_handle = std::thread::Builder::new()
            .name("brush_execution".to_owned())
            .spawn(move || {
                brush_execution_loop(
                    config,
                    worker_stop_requested,
                    sample_consumer,
                    feedback_consumer,
                    command_producer,
                )
            })
            .expect("spawn brush execution thread");

        Ok((
            Self {
                stop_requested,
                join_handle: Some(join_handle),
            },
            BrushExecutionSampleSender {
                producer: sample_producer,
            },
            BrushExecutionFeedbackSender {
                producer: feedback_producer,
            },
            BrushExecutionCommandReceiver {
                consumer: command_consumer,
            },
        ))
    }
}

impl Drop for BrushExecutionRuntime {
    fn drop(&mut self) {
        self.stop_requested.store(true, Ordering::Release);
        if let Some(join_handle) = self.join_handle.take() {
            join_handle.join().expect("join brush execution thread");
        }
    }
}

fn brush_execution_loop(
    config: BrushExecutionConfig,
    stop_requested: Arc<AtomicBool>,
    mut sample_consumer: rtrb::Consumer<FramedSampleChunk>,
    mut feedback_consumer: rtrb::Consumer<BrushExecutionMergeFeedback>,
    mut command_producer: rtrb::Producer<BrushRenderCommand>,
) {
    const IDLE_SLEEP_DURATION: Duration = Duration::from_millis(1);
    const MERGE_APPLIED_RELEASE_RETAIN_DURATION: Duration = Duration::from_millis(250);

    let mut buffer_tile_lifecycle = BufferTileLifecycle::<u64, BufferTileCoordinate>::new();
    let mut merge_feedback_pending_strokes = HashSet::<u64>::new();
    while !stop_requested.load(Ordering::Acquire) {
        drain_releasable_tiles(
            &mut buffer_tile_lifecycle,
            Instant::now(),
            &mut command_producer,
        );

        while let Ok(feedback) = feedback_consumer.pop() {
            let stroke_session_id = match &feedback {
                BrushExecutionMergeFeedback::MergeApplied { stroke_session_id } => {
                    *stroke_session_id
                }
                BrushExecutionMergeFeedback::MergeFailed {
                    stroke_session_id, ..
                } => *stroke_session_id,
            };
            if !merge_feedback_pending_strokes.remove(&stroke_session_id) {
                panic!(
                    "received merge feedback for stroke {} without pending feedback state",
                    stroke_session_id
                );
            }
            buffer_tile_lifecycle.move_allocated_to_pending(stroke_session_id);
            match feedback {
                BrushExecutionMergeFeedback::MergeApplied { .. } => {
                    buffer_tile_lifecycle.retain_pending_until(
                        stroke_session_id,
                        Instant::now() + MERGE_APPLIED_RELEASE_RETAIN_DURATION,
                    );
                    continue;
                }
                BrushExecutionMergeFeedback::MergeFailed { message, .. } => {
                    eprintln!(
                        "merge failed for stroke {} in brush execution: {}",
                        stroke_session_id, message
                    );
                }
            }
            drain_releasable_tiles(
                &mut buffer_tile_lifecycle,
                Instant::now(),
                &mut command_producer,
            );
        }

        let sample_chunk = match sample_consumer.pop() {
            Ok(sample_chunk) => sample_chunk,
            Err(rtrb::PopError::Empty) => {
                std::thread::sleep(IDLE_SLEEP_DURATION);
                continue;
            }
        };

        let chunk = sample_chunk.chunk;
        if chunk.starts_stroke {
            push_command(
                &mut command_producer,
                BrushRenderCommand::BeginStroke(BrushStrokeBegin {
                    stroke_session_id: chunk.stroke_session_id,
                    brush_id: config.brush_id,
                    program_revision: config.program_revision,
                    reference_set_id: config.reference_set_id,
                    target_layer_id: config.target_layer_id,
                    discontinuity_before: chunk.discontinuity_before,
                }),
                "begin stroke",
            );
        }

        let affected_tiles = collect_affected_tiles(
            chunk.canvas_x(),
            chunk.canvas_y(),
            config.max_affected_radius,
            config.buffer_tile_size,
        );
        let newly_affected_tiles =
            buffer_tile_lifecycle.record_allocated_batch(chunk.stroke_session_id, affected_tiles);
        if !newly_affected_tiles.is_empty() {
            push_command(
                &mut command_producer,
                BrushRenderCommand::AllocateBufferTiles(BrushBufferTileAllocate {
                    stroke_session_id: chunk.stroke_session_id,
                    tiles: newly_affected_tiles,
                }),
                "allocate buffer tiles",
            );
        }

        let dab_chunk = BrushDabChunkF32::from_slices(
            chunk.stroke_session_id,
            chunk.canvas_x(),
            chunk.canvas_y(),
            chunk.pressure(),
        )
        .expect("convert sample chunk to dab chunk");
        push_command(
            &mut command_producer,
            BrushRenderCommand::PushDabChunkF32(dab_chunk),
            "push dab chunk",
        );

        if chunk.ends_stroke {
            push_command(
                &mut command_producer,
                BrushRenderCommand::EndStroke(BrushStrokeEnd {
                    stroke_session_id: chunk.stroke_session_id,
                }),
                "end stroke",
            );
            emit_merge_for_stroke(
                chunk.stroke_session_id,
                config.target_layer_id,
                &mut merge_feedback_pending_strokes,
                &mut command_producer,
            );
        }
    }

    buffer_tile_lifecycle.begin_shutdown();
    for batch in buffer_tile_lifecycle.force_release_all() {
        push_release_command(&mut command_producer, batch.owner, batch.tiles);
    }
}

fn emit_merge_for_stroke(
    stroke_session_id: u64,
    target_layer_id: LayerId,
    merge_feedback_pending_strokes: &mut HashSet<u64>,
    command_producer: &mut rtrb::Producer<BrushRenderCommand>,
) {
    push_command(
        command_producer,
        BrushRenderCommand::MergeBuffer(BrushBufferMerge {
            stroke_session_id,
            target_layer_id,
        }),
        "merge buffer",
    );
    let inserted = merge_feedback_pending_strokes.insert(stroke_session_id);
    if !inserted {
        panic!("pending merge feedback duplicated for ended stroke");
    }
}

fn drain_releasable_tiles(
    buffer_tile_lifecycle: &mut BufferTileLifecycle<u64, BufferTileCoordinate>,
    now: Instant,
    command_producer: &mut rtrb::Producer<BrushRenderCommand>,
) {
    for batch in buffer_tile_lifecycle.drain_releasable(now) {
        push_release_command(command_producer, batch.owner, batch.tiles);
    }
}

fn push_release_command(
    command_producer: &mut rtrb::Producer<BrushRenderCommand>,
    stroke_session_id: u64,
    tiles: Vec<BufferTileCoordinate>,
) {
    push_command(
        command_producer,
        BrushRenderCommand::ReleaseBufferTiles(BrushBufferTileRelease {
            stroke_session_id,
            tiles,
        }),
        "release tiles",
    );
}

fn push_command(
    command_producer: &mut rtrb::Producer<BrushRenderCommand>,
    command: BrushRenderCommand,
    context: &'static str,
) {
    command_producer
        .push(command)
        .unwrap_or_else(|_| panic!("brush execution output queue full on {}", context));
}

fn collect_affected_tiles(
    canvas_x: &[f32],
    canvas_y: &[f32],
    radius: f32,
    tile_size: u32,
) -> Vec<BufferTileCoordinate> {
    let mut affected_tiles = Vec::new();
    let mut seen_tiles = HashSet::new();
    let tile_size_f32 = tile_size as f32;
    for (&dab_x, &dab_y) in canvas_x.iter().zip(canvas_y.iter()) {
        if !dab_x.is_finite() || !dab_y.is_finite() {
            panic!("dab coordinates must be finite");
        }

        let min_tile_x = tile_index_for_canvas(dab_x - radius, tile_size_f32);
        let max_tile_x = tile_index_for_canvas(dab_x + radius, tile_size_f32);
        let min_tile_y = tile_index_for_canvas(dab_y - radius, tile_size_f32);
        let max_tile_y = tile_index_for_canvas(dab_y + radius, tile_size_f32);

        for tile_x in min_tile_x..=max_tile_x {
            for tile_y in min_tile_y..=max_tile_y {
                let tile = BufferTileCoordinate { tile_x, tile_y };
                if seen_tiles.insert(tile) {
                    affected_tiles.push(tile);
                }
            }
        }
    }
    affected_tiles
}

fn tile_index_for_canvas(value: f32, tile_size: f32) -> i32 {
    let tile_index = (value / tile_size).floor();
    if tile_index < i32::MIN as f32 || tile_index > i32::MAX as f32 {
        panic!("tile index out of i32 range");
    }
    tile_index as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use driver::{SampleChunk, StrokeSample};
    use std::collections::HashSet;

    fn sample(index: usize) -> StrokeSample {
        StrokeSample {
            timestamp_micros: index as u64,
            canvas_x: index as f32,
            canvas_y: index as f32 + 0.5,
            pressure: 0.8,
            velocity_pixels_per_second: 1.0,
            tilt_x_degrees: 0.0,
            tilt_y_degrees: 0.0,
            twist_degrees: 0.0,
        }
    }

    #[test]
    fn converts_sample_chunk_to_brush_render_commands() {
        let (runtime, mut sender, mut _feedback_sender, mut receiver) =
            BrushExecutionRuntime::start(
                BrushExecutionConfig {
                    brush_id: 9,
                    program_revision: 3,
                    reference_set_id: 5,
                    target_layer_id: 77,
                    buffer_tile_size: 128,
                    max_affected_radius: 64.0,
                },
                8,
                8,
            )
            .expect("start brush execution runtime");

        let chunk = SampleChunk::from_samples(33, 7, true, true, &[sample(0), sample(1)])
            .expect("build sample chunk");
        sender
            .push_chunk(FramedSampleChunk {
                frame_sequence_id: 10,
                chunk,
            })
            .expect("push sample chunk");

        let start = std::time::Instant::now();
        let mut commands = Vec::new();
        while commands.len() < 4 && start.elapsed() < std::time::Duration::from_secs(1) {
            if let Some(command) = receiver.pop_command() {
                commands.push(command);
            }
        }

        assert_eq!(commands.len(), 4);
        match commands[0] {
            BrushRenderCommand::BeginStroke(begin) => {
                assert_eq!(begin.stroke_session_id, 33);
                assert_eq!(begin.brush_id, 9);
                assert_eq!(begin.program_revision, 3);
                assert_eq!(begin.reference_set_id, 5);
                assert_eq!(begin.target_layer_id, 77);
            }
            _ => panic!("first command must be begin stroke"),
        }
        match &commands[1] {
            BrushRenderCommand::AllocateBufferTiles(allocate) => {
                assert_eq!(allocate.stroke_session_id, 33);
                let expected_tiles = vec![
                    BufferTileCoordinate {
                        tile_x: -1,
                        tile_y: -1,
                    },
                    BufferTileCoordinate {
                        tile_x: 0,
                        tile_y: -1,
                    },
                    BufferTileCoordinate { tile_x: -1, tile_y: 0 },
                    BufferTileCoordinate { tile_x: 0, tile_y: 0 },
                ];
                assert_eq!(allocate.tiles.len(), expected_tiles.len());
                let actual_tiles: HashSet<BufferTileCoordinate> =
                    allocate.tiles.iter().copied().collect();
                for expected_tile in expected_tiles {
                    assert!(actual_tiles.contains(&expected_tile));
                }
            }
            _ => panic!("second command must be allocate buffer tiles"),
        }
        match &commands[2] {
            BrushRenderCommand::PushDabChunkF32(chunk) => {
                assert_eq!(chunk.stroke_session_id, 33);
                assert_eq!(chunk.dab_count(), 2);
            }
            _ => panic!("third command must be push dab chunk"),
        }
        match commands[3] {
            BrushRenderCommand::EndStroke(end) => {
                assert_eq!(end.stroke_session_id, 33);
            }
            _ => panic!("fourth command must be end stroke"),
        }

        drop(runtime);
    }

    #[test]
    fn emits_merge_before_next_stroke_begin() {
        let (runtime, mut sender, mut feedback_sender, mut receiver) =
            BrushExecutionRuntime::start(
                BrushExecutionConfig {
                    brush_id: 2,
                    program_revision: 4,
                    reference_set_id: 6,
                    target_layer_id: 90,
                    buffer_tile_size: 128,
                    max_affected_radius: 64.0,
                },
                8,
                16,
            )
            .expect("start brush execution runtime");

        let first_chunk = SampleChunk::from_samples(100, 7, true, true, &[sample(0)])
            .expect("build first sample chunk");
        sender
            .push_chunk(FramedSampleChunk {
                frame_sequence_id: 10,
                chunk: first_chunk,
            })
            .expect("push first sample chunk");

        let second_chunk = SampleChunk::from_samples(101, 7, true, false, &[sample(1)])
            .expect("build second sample chunk");
        sender
            .push_chunk(FramedSampleChunk {
                frame_sequence_id: 11,
                chunk: second_chunk,
            })
            .expect("push second sample chunk");

        let start = std::time::Instant::now();
        let mut commands = Vec::new();
        while commands.len() < 8 && start.elapsed() < std::time::Duration::from_secs(1) {
            if let Some(command) = receiver.pop_command() {
                commands.push(command);
            }
        }

        assert_eq!(commands.len(), 8);
        assert!(matches!(
            commands[0],
            BrushRenderCommand::BeginStroke(BrushStrokeBegin {
                stroke_session_id: 100,
                ..
            })
        ));
        assert!(matches!(
            commands[1],
            BrushRenderCommand::AllocateBufferTiles(BrushBufferTileAllocate {
                stroke_session_id: 100,
                ..
            })
        ));
        assert!(matches!(
            commands[2],
            BrushRenderCommand::PushDabChunkF32(BrushDabChunkF32 {
                stroke_session_id: 100,
                ..
            })
        ));
        assert!(matches!(
            commands[3],
            BrushRenderCommand::EndStroke(BrushStrokeEnd {
                stroke_session_id: 100
            })
        ));
        assert!(matches!(
            commands[4],
            BrushRenderCommand::MergeBuffer(BrushBufferMerge {
                stroke_session_id: 100,
                target_layer_id: 90
            })
        ));
        assert!(matches!(
            commands[5],
            BrushRenderCommand::BeginStroke(BrushStrokeBegin {
                stroke_session_id: 101,
                ..
            })
        ));
        assert!(matches!(
            commands[6],
            BrushRenderCommand::AllocateBufferTiles(BrushBufferTileAllocate {
                stroke_session_id: 101,
                ..
            })
        ));
        assert!(matches!(
            commands[7],
            BrushRenderCommand::PushDabChunkF32(BrushDabChunkF32 {
                stroke_session_id: 101,
                ..
            })
        ));

        let release_start = std::time::Instant::now();
        feedback_sender
            .push_feedback(BrushExecutionMergeFeedback::MergeApplied {
                stroke_session_id: 100,
            })
            .expect("push merge applied feedback");
        let start = std::time::Instant::now();
        let release = loop {
            if let Some(command) = receiver.pop_command() {
                break command;
            }
            if start.elapsed() >= std::time::Duration::from_secs(1) {
                panic!("timed out waiting for release command");
            }
        };
        assert!(matches!(
            release,
            BrushRenderCommand::ReleaseBufferTiles(BrushBufferTileRelease {
                stroke_session_id: 100,
                ..
            })
        ));
        assert!(
            release_start.elapsed() >= std::time::Duration::from_millis(200),
            "release must be retained briefly after merge applied"
        );

        drop(runtime);
    }

    #[test]
    fn emits_merge_immediately_when_stroke_ends_without_next_stroke() {
        let (runtime, mut sender, mut _feedback_sender, mut receiver) =
            BrushExecutionRuntime::start(
                BrushExecutionConfig {
                    brush_id: 2,
                    program_revision: 4,
                    reference_set_id: 6,
                    target_layer_id: 90,
                    buffer_tile_size: 128,
                    max_affected_radius: 64.0,
                },
                8,
                16,
            )
            .expect("start brush execution runtime");

        let chunk = SampleChunk::from_samples(200, 7, true, true, &[sample(0)])
            .expect("build sample chunk");
        sender
            .push_chunk(FramedSampleChunk {
                frame_sequence_id: 10,
                chunk,
            })
            .expect("push sample chunk");

        let start = std::time::Instant::now();
        let mut observed_merge = false;
        while start.elapsed() < std::time::Duration::from_secs(1) {
            if let Some(command) = receiver.pop_command() {
                if matches!(
                    command,
                    BrushRenderCommand::MergeBuffer(BrushBufferMerge {
                        stroke_session_id: 200,
                        target_layer_id: 90
                    })
                ) {
                    observed_merge = true;
                    break;
                }
            }
        }
        assert!(
            observed_merge,
            "stroke end must emit merge without waiting for next stroke"
        );

        drop(runtime);
    }

    #[test]
    fn runtime_drop_releases_tiles_for_unfinished_stroke() {
        let (runtime, mut sender, mut _feedback_sender, mut receiver) =
            BrushExecutionRuntime::start(
                BrushExecutionConfig {
                    brush_id: 2,
                    program_revision: 4,
                    reference_set_id: 6,
                    target_layer_id: 90,
                    buffer_tile_size: 128,
                    max_affected_radius: 64.0,
                },
                8,
                16,
            )
            .expect("start brush execution runtime");

        let chunk = SampleChunk::from_samples(300, 7, true, false, &[sample(0)])
            .expect("build sample chunk");
        sender
            .push_chunk(FramedSampleChunk {
                frame_sequence_id: 10,
                chunk,
            })
            .expect("push sample chunk");

        let start = std::time::Instant::now();
        let mut observed_allocate = false;
        while start.elapsed() < std::time::Duration::from_secs(1) {
            if let Some(command) = receiver.pop_command() {
                if matches!(
                    command,
                    BrushRenderCommand::AllocateBufferTiles(BrushBufferTileAllocate {
                        stroke_session_id: 300,
                        ..
                    })
                ) {
                    observed_allocate = true;
                    break;
                }
            }
        }
        assert!(observed_allocate, "must allocate tiles before runtime drop");

        drop(runtime);

        let release_wait_start = std::time::Instant::now();
        let mut observed_release = false;
        while release_wait_start.elapsed() < std::time::Duration::from_secs(1) {
            if let Some(command) = receiver.pop_command() {
                if matches!(
                    command,
                    BrushRenderCommand::ReleaseBufferTiles(BrushBufferTileRelease {
                        stroke_session_id: 300,
                        ..
                    })
                ) {
                    observed_release = true;
                    break;
                }
            }
        }
        assert!(
            observed_release,
            "runtime drop must force release unfinished stroke tiles"
        );
    }
}
