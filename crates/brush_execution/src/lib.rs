use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use driver::FramedSampleChunk;
use render_protocol::{
    BrushBufferMerge, BrushDabChunkF32, BrushId, BrushRenderCommand, BrushStrokeBegin,
    BrushStrokeEnd, LayerId, ProgramRevision, ReferenceSetId,
};

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrushExecutionConfig {
    pub brush_id: BrushId,
    pub program_revision: ProgramRevision,
    pub reference_set_id: ReferenceSetId,
    pub target_layer_id: LayerId,
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
        let (sample_producer, sample_consumer) = rtrb::RingBuffer::new(input_queue_capacity);
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
    mut command_producer: rtrb::Producer<BrushRenderCommand>,
) {
    let mut pending_merge: Option<(u64, LayerId)> = None;
    while !stop_requested.load(Ordering::Acquire) {
        let sample_chunk = match sample_consumer.pop() {
            Ok(sample_chunk) => sample_chunk,
            Err(rtrb::PopError::Empty) => {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
        };

        let chunk = sample_chunk.chunk;
        if chunk.starts_stroke {
            if let Some((ended_stroke_session_id, target_layer_id)) = pending_merge {
                if ended_stroke_session_id != chunk.stroke_session_id {
                    command_producer
                        .push(BrushRenderCommand::MergeBuffer(BrushBufferMerge {
                            stroke_session_id: ended_stroke_session_id,
                            target_layer_id,
                        }))
                        .unwrap_or_else(|_| {
                            panic!("brush execution output queue full on merge buffer")
                        });
                    pending_merge = None;
                }
            }
            command_producer
                .push(BrushRenderCommand::BeginStroke(BrushStrokeBegin {
                    stroke_session_id: chunk.stroke_session_id,
                    brush_id: config.brush_id,
                    program_revision: config.program_revision,
                    reference_set_id: config.reference_set_id,
                    target_layer_id: config.target_layer_id,
                    discontinuity_before: chunk.discontinuity_before,
                }))
                .unwrap_or_else(|_| panic!("brush execution output queue full on begin stroke"));
        }

        let dab_chunk = BrushDabChunkF32::from_slices(
            chunk.stroke_session_id,
            chunk.canvas_x(),
            chunk.canvas_y(),
            chunk.pressure(),
        )
        .expect("convert sample chunk to dab chunk");
        command_producer
            .push(BrushRenderCommand::PushDabChunkF32(dab_chunk))
            .unwrap_or_else(|_| panic!("brush execution output queue full on push dab chunk"));

        if chunk.ends_stroke {
            command_producer
                .push(BrushRenderCommand::EndStroke(BrushStrokeEnd {
                    stroke_session_id: chunk.stroke_session_id,
                }))
                .unwrap_or_else(|_| panic!("brush execution output queue full on end stroke"));
            pending_merge = Some((chunk.stroke_session_id, config.target_layer_id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use driver::{SampleChunk, StrokeSample};

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
        let (runtime, mut sender, mut receiver) = BrushExecutionRuntime::start(
            BrushExecutionConfig {
                brush_id: 9,
                program_revision: 3,
                reference_set_id: 5,
                target_layer_id: 77,
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
        while commands.len() < 3 && start.elapsed() < std::time::Duration::from_secs(1) {
            if let Some(command) = receiver.pop_command() {
                commands.push(command);
            }
        }

        assert_eq!(commands.len(), 3);
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
            BrushRenderCommand::PushDabChunkF32(chunk) => {
                assert_eq!(chunk.stroke_session_id, 33);
                assert_eq!(chunk.dab_count(), 2);
            }
            _ => panic!("second command must be push dab chunk"),
        }
        match commands[2] {
            BrushRenderCommand::EndStroke(end) => {
                assert_eq!(end.stroke_session_id, 33);
            }
            _ => panic!("third command must be end stroke"),
        }

        drop(runtime);
    }

    #[test]
    fn emits_merge_before_next_stroke_begin() {
        let (runtime, mut sender, mut receiver) = BrushExecutionRuntime::start(
            BrushExecutionConfig {
                brush_id: 2,
                program_revision: 4,
                reference_set_id: 6,
                target_layer_id: 90,
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
        while commands.len() < 5 && start.elapsed() < std::time::Duration::from_secs(1) {
            if let Some(command) = receiver.pop_command() {
                commands.push(command);
            }
        }

        assert_eq!(commands.len(), 5);
        assert!(matches!(
            commands[0],
            BrushRenderCommand::BeginStroke(BrushStrokeBegin {
                stroke_session_id: 100,
                ..
            })
        ));
        assert!(matches!(
            commands[1],
            BrushRenderCommand::PushDabChunkF32(BrushDabChunkF32 {
                stroke_session_id: 100,
                ..
            })
        ));
        assert!(matches!(
            commands[2],
            BrushRenderCommand::EndStroke(BrushStrokeEnd {
                stroke_session_id: 100
            })
        ));
        assert!(matches!(
            commands[3],
            BrushRenderCommand::MergeBuffer(BrushBufferMerge {
                stroke_session_id: 100,
                target_layer_id: 90
            })
        ));
        assert!(matches!(
            commands[4],
            BrushRenderCommand::BeginStroke(BrushStrokeBegin {
                stroke_session_id: 101,
                ..
            })
        ));

        drop(runtime);
    }
}
