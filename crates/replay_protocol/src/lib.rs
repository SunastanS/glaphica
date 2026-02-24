use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};

pub type StrokeSessionId = u64;
pub type LayerId = u64;
pub type ReferenceSetId = u64;
pub type MergeRequestId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputPhase {
    EnqueueBeforeGpu,
    FlushQuiescent,
    Finalize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputKind {
    Driver,
    BrushExecution,
    RenderCommand,
    MergeLifecycle,
    StateDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub schema_version: u16,
    pub scenario_id: String,
    pub run_id: String,
    pub event_id: u64,
    pub tick: u64,
    pub phase: OutputPhase,
    pub kind: OutputKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrushCommandKind {
    BeginStroke,
    AllocateBufferTiles,
    PushDabChunkF32,
    EndStroke,
    MergeBuffer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeAckKind {
    Submitted,
    TerminalSuccess,
    TerminalFailure,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriverOutput {
    pub stroke_session_id: StrokeSessionId,
    pub chunk_index: u32,
    pub sample_count: u32,
    pub starts_stroke: bool,
    pub ends_stroke: bool,
    pub discontinuity_before: bool,
    pub dropped_chunk_count_before: u32,
    pub bounds_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrushExecutionOutput {
    pub stroke_session_id: StrokeSessionId,
    pub command_kind: BrushCommandKind,
    pub target_layer_id: LayerId,
    pub reference_set_id: ReferenceSetId,
    pub payload_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderCommandOutput {
    pub stroke_session_id: StrokeSessionId,
    pub command_kind: BrushCommandKind,
    pub tile_count: u32,
    pub tile_keys_digest: String,
    pub blend_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeLifecycleOutput {
    pub stroke_session_id: StrokeSessionId,
    pub merge_request_id: MergeRequestId,
    pub submit_sequence: u64,
    pub ack_kind: MergeAckKind,
    pub receipt_terminal_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDigestOutput {
    pub document_revision: u64,
    pub render_tree_revision: u64,
    pub render_tree_semantic_hash: String,
    pub pending_brush_command_count: u32,
    pub active_stroke_count: u32,
    pub dirty_tile_set_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OutputPayload {
    Driver(DriverOutput),
    BrushExecution(BrushExecutionOutput),
    RenderCommand(RenderCommandOutput),
    MergeLifecycle(MergeLifecycleOutput),
    StateDigest(StateDigestOutput),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputEvent {
    pub envelope: EventEnvelope,
    pub payload: OutputPayload,
    pub debug_wall_time_micros: Option<u64>,
}

impl OutputEvent {
    pub fn new(envelope: EventEnvelope, payload: OutputPayload) -> Self {
        Self {
            envelope,
            payload,
            debug_wall_time_micros: None,
        }
    }
}

pub fn write_jsonl_event_line(
    writer: &mut dyn Write,
    event: &OutputEvent,
) -> Result<(), std::io::Error> {
    serde_json::to_writer(&mut *writer, event).map_err(|error| {
        std::io::Error::other(format!("serialize output event as JSON failed: {error}"))
    })?;
    writer.write_all(b"\n")
}

pub fn read_jsonl_events(reader: &mut dyn BufRead) -> Result<Vec<OutputEvent>, std::io::Error> {
    let mut events = Vec::new();
    let mut line_buffer = String::new();
    let mut line_number = 0usize;
    loop {
        line_buffer.clear();
        let bytes = reader.read_line(&mut line_buffer)?;
        if bytes == 0 {
            break;
        }
        line_number = line_number
            .checked_add(1)
            .unwrap_or_else(|| panic!("jsonl line number overflow"));
        if line_buffer.trim().is_empty() {
            continue;
        }
        let event = serde_json::from_str::<OutputEvent>(&line_buffer).map_err(|error| {
            std::io::Error::other(format!(
                "parse output event JSON at line {line_number} failed: {error}"
            ))
        })?;
        events.push(event);
    }
    Ok(events)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StrokeState {
    Begun,
    Ended,
    Merged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    EmptyScenario,
    EventIdNotStrictlyIncreasing {
        previous: u64,
        current: u64,
    },
    TickDecreased {
        previous: u64,
        current: u64,
    },
    DuplicateMergeRequest {
        merge_request_id: MergeRequestId,
    },
    BeginStrokeWhileActive {
        stroke_session_id: StrokeSessionId,
    },
    StrokeCommandBeforeBegin {
        stroke_session_id: StrokeSessionId,
        command_kind: BrushCommandKind,
    },
    EndStrokeWithoutBegin {
        stroke_session_id: StrokeSessionId,
    },
    MergeBeforeEnd {
        stroke_session_id: StrokeSessionId,
    },
    MergeTerminalWithoutStroke {
        stroke_session_id: StrokeSessionId,
    },
    MergeAfterTerminal {
        stroke_session_id: StrokeSessionId,
    },
    DocumentRevisionDecreased {
        previous: u64,
        current: u64,
    },
}

pub fn validate_event_stream(events: &[OutputEvent]) -> Result<(), ValidationError> {
    if events.is_empty() {
        return Err(ValidationError::EmptyScenario);
    }

    let mut previous_event_id: Option<u64> = None;
    let mut previous_tick: Option<u64> = None;
    let mut previous_document_revision: Option<u64> = None;
    let mut merge_request_ids = HashSet::new();
    let mut stroke_states: HashMap<StrokeSessionId, StrokeState> = HashMap::new();

    for event in events {
        if let Some(previous) = previous_event_id {
            if event.envelope.event_id <= previous {
                return Err(ValidationError::EventIdNotStrictlyIncreasing {
                    previous,
                    current: event.envelope.event_id,
                });
            }
        }
        previous_event_id = Some(event.envelope.event_id);

        if let Some(previous) = previous_tick {
            if event.envelope.tick < previous {
                return Err(ValidationError::TickDecreased {
                    previous,
                    current: event.envelope.tick,
                });
            }
        }
        previous_tick = Some(event.envelope.tick);

        match &event.payload {
            OutputPayload::RenderCommand(command) => {
                validate_stroke_command(&mut stroke_states, command)?;
            }
            OutputPayload::MergeLifecycle(merge) => {
                if !merge_request_ids.insert(merge.merge_request_id) {
                    return Err(ValidationError::DuplicateMergeRequest {
                        merge_request_id: merge.merge_request_id,
                    });
                }
                validate_merge_lifecycle(&mut stroke_states, merge)?;
            }
            OutputPayload::StateDigest(state_digest) => {
                if let Some(previous) = previous_document_revision {
                    if state_digest.document_revision < previous {
                        return Err(ValidationError::DocumentRevisionDecreased {
                            previous,
                            current: state_digest.document_revision,
                        });
                    }
                }
                previous_document_revision = Some(state_digest.document_revision);
            }
            OutputPayload::Driver(_) | OutputPayload::BrushExecution(_) => {}
        }
    }

    Ok(())
}

fn validate_stroke_command(
    stroke_states: &mut HashMap<StrokeSessionId, StrokeState>,
    command: &RenderCommandOutput,
) -> Result<(), ValidationError> {
    let current = stroke_states.get(&command.stroke_session_id);
    match command.command_kind {
        BrushCommandKind::BeginStroke => {
            if matches!(current, Some(StrokeState::Begun) | Some(StrokeState::Ended)) {
                return Err(ValidationError::BeginStrokeWhileActive {
                    stroke_session_id: command.stroke_session_id,
                });
            }
            stroke_states.insert(command.stroke_session_id, StrokeState::Begun);
            Ok(())
        }
        BrushCommandKind::AllocateBufferTiles | BrushCommandKind::PushDabChunkF32 => {
            if !matches!(current, Some(StrokeState::Begun)) {
                return Err(ValidationError::StrokeCommandBeforeBegin {
                    stroke_session_id: command.stroke_session_id,
                    command_kind: command.command_kind,
                });
            }
            Ok(())
        }
        BrushCommandKind::EndStroke => {
            if !matches!(current, Some(StrokeState::Begun)) {
                return Err(ValidationError::EndStrokeWithoutBegin {
                    stroke_session_id: command.stroke_session_id,
                });
            }
            stroke_states.insert(command.stroke_session_id, StrokeState::Ended);
            Ok(())
        }
        BrushCommandKind::MergeBuffer => {
            if !matches!(
                current,
                Some(StrokeState::Ended) | Some(StrokeState::Merged)
            ) {
                return Err(ValidationError::MergeBeforeEnd {
                    stroke_session_id: command.stroke_session_id,
                });
            }
            Ok(())
        }
    }
}

fn validate_merge_lifecycle(
    stroke_states: &mut HashMap<StrokeSessionId, StrokeState>,
    merge: &MergeLifecycleOutput,
) -> Result<(), ValidationError> {
    let current = stroke_states.get(&merge.stroke_session_id);
    match merge.ack_kind {
        MergeAckKind::Submitted => {
            if !matches!(
                current,
                Some(StrokeState::Ended) | Some(StrokeState::Merged)
            ) {
                return Err(ValidationError::MergeBeforeEnd {
                    stroke_session_id: merge.stroke_session_id,
                });
            }
            Ok(())
        }
        MergeAckKind::TerminalSuccess | MergeAckKind::TerminalFailure => {
            if current.is_none() {
                return Err(ValidationError::MergeTerminalWithoutStroke {
                    stroke_session_id: merge.stroke_session_id,
                });
            }
            if matches!(current, Some(StrokeState::Merged)) {
                return Err(ValidationError::MergeAfterTerminal {
                    stroke_session_id: merge.stroke_session_id,
                });
            }
            stroke_states.insert(merge.stroke_session_id, StrokeState::Merged);
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompareError {
    EventCountMismatch { expected: usize, actual: usize },
    EventMismatch { index: usize },
}

pub fn compare_semantic_events(
    expected: &[OutputEvent],
    actual: &[OutputEvent],
) -> Result<(), CompareError> {
    if expected.len() != actual.len() {
        return Err(CompareError::EventCountMismatch {
            expected: expected.len(),
            actual: actual.len(),
        });
    }

    for (index, (left, right)) in expected.iter().zip(actual.iter()).enumerate() {
        if left.envelope != right.envelope || left.payload != right.payload {
            return Err(CompareError::EventMismatch { index });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(event_id: u64, tick: u64, kind: OutputKind) -> EventEnvelope {
        EventEnvelope {
            schema_version: 1,
            scenario_id: String::from("scenario-a"),
            run_id: String::from("run-a"),
            event_id,
            tick,
            phase: OutputPhase::EnqueueBeforeGpu,
            kind,
        }
    }

    fn render(
        event_id: u64,
        tick: u64,
        stroke: u64,
        command_kind: BrushCommandKind,
    ) -> OutputEvent {
        OutputEvent::new(
            envelope(event_id, tick, OutputKind::RenderCommand),
            OutputPayload::RenderCommand(RenderCommandOutput {
                stroke_session_id: stroke,
                command_kind,
                tile_count: 1,
                tile_keys_digest: String::from("xxh3:tile"),
                blend_mode: String::from("Normal"),
            }),
        )
    }

    fn merge(
        event_id: u64,
        tick: u64,
        stroke: u64,
        merge_request_id: u64,
        ack_kind: MergeAckKind,
    ) -> OutputEvent {
        OutputEvent::new(
            envelope(event_id, tick, OutputKind::MergeLifecycle),
            OutputPayload::MergeLifecycle(MergeLifecycleOutput {
                stroke_session_id: stroke,
                merge_request_id,
                submit_sequence: 1,
                ack_kind,
                receipt_terminal_state: String::from("ok"),
            }),
        )
    }

    #[test]
    fn validate_event_stream_accepts_monotonic_valid_sequence() {
        let events = vec![
            render(1, 1, 9, BrushCommandKind::BeginStroke),
            render(2, 1, 9, BrushCommandKind::PushDabChunkF32),
            render(3, 2, 9, BrushCommandKind::EndStroke),
            render(4, 3, 9, BrushCommandKind::MergeBuffer),
            merge(5, 3, 9, 42, MergeAckKind::Submitted),
            merge(6, 4, 9, 43, MergeAckKind::TerminalSuccess),
            OutputEvent::new(
                envelope(7, 4, OutputKind::StateDigest),
                OutputPayload::StateDigest(StateDigestOutput {
                    document_revision: 100,
                    render_tree_revision: 100,
                    render_tree_semantic_hash: String::from("xxh3:tree"),
                    pending_brush_command_count: 0,
                    active_stroke_count: 0,
                    dirty_tile_set_digest: String::from("xxh3:none"),
                }),
            ),
        ];

        assert_eq!(validate_event_stream(&events), Ok(()));
    }

    #[test]
    fn validate_event_stream_rejects_non_monotonic_event_id() {
        let events = vec![
            render(2, 1, 9, BrushCommandKind::BeginStroke),
            render(2, 1, 9, BrushCommandKind::EndStroke),
        ];

        assert_eq!(
            validate_event_stream(&events),
            Err(ValidationError::EventIdNotStrictlyIncreasing {
                previous: 2,
                current: 2,
            })
        );
    }

    #[test]
    fn compare_semantic_events_ignores_debug_wall_time() {
        let mut left = render(1, 1, 9, BrushCommandKind::BeginStroke);
        let mut right = render(1, 1, 9, BrushCommandKind::BeginStroke);

        left.debug_wall_time_micros = Some(100);
        right.debug_wall_time_micros = Some(200);

        assert_eq!(compare_semantic_events(&[left], &[right]), Ok(()));
    }

    #[test]
    fn jsonl_roundtrip_preserves_semantics() {
        let event = render(1, 1, 9, BrushCommandKind::BeginStroke);
        let mut bytes = Vec::new();
        write_jsonl_event_line(&mut bytes, &event).expect("write event");
        let mut reader = std::io::BufReader::new(bytes.as_slice());
        let parsed = read_jsonl_events(&mut reader).expect("read events");
        assert_eq!(parsed.len(), 1);
        assert_eq!(compare_semantic_events(&[event], &parsed), Ok(()));
    }
}
