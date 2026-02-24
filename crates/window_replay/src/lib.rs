use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use replay_protocol::{
    CompareError, DebugOutput, OutputEvent, OutputKind, OutputPayload, OutputPhase,
    ValidationError, compare_semantic_events, read_jsonl_events, validate_event_stream,
    write_jsonl_event_line,
};
use serde::{Deserialize, Serialize};

const TRACE_RECORDER_FLUSH_EVERY_EVENTS: u32 = 128;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordedInputEventKind {
    MouseInput { pressed: bool },
    CursorMoved { x: f64, y: f64 },
    MouseWheelLine { vertical_lines: f32 },
    MouseWheelPixel { delta_y: f64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecordedInputEvent {
    schema_version: u32,
    event_id: u64,
    elapsed_micros: u64,
    #[serde(flatten)]
    kind: RecordedInputEventKind,
}

pub struct InputTraceRecorder {
    started_at: Instant,
    writer: BufWriter<File>,
    next_event_id: u64,
    pending_events_since_flush: u32,
}

impl InputTraceRecorder {
    pub fn from_path(path: PathBuf) -> Self {
        let file = File::create(&path).unwrap_or_else(|error| {
            panic!("create input trace file '{}': {error}", path.display())
        });
        Self {
            started_at: Instant::now(),
            writer: BufWriter::new(file),
            next_event_id: 1,
            pending_events_since_flush: 0,
        }
    }

    pub fn record(&mut self, kind: RecordedInputEventKind) {
        let elapsed_micros = self.started_at.elapsed().as_micros();
        let elapsed_micros = u64::try_from(elapsed_micros)
            .unwrap_or_else(|_| panic!("input trace timestamp overflow"));
        let event = RecordedInputEvent {
            schema_version: 1,
            event_id: self.next_event_id,
            elapsed_micros,
            kind,
        };
        self.next_event_id = self
            .next_event_id
            .checked_add(1)
            .unwrap_or_else(|| panic!("input trace event id overflow"));
        serde_json::to_writer(&mut self.writer, &event)
            .unwrap_or_else(|error| panic!("write input trace event failed: {error}"));
        writeln!(self.writer)
            .unwrap_or_else(|error| panic!("write input trace event failed: {error}"));
        self.pending_events_since_flush = self
            .pending_events_since_flush
            .checked_add(1)
            .unwrap_or_else(|| panic!("input trace pending event counter overflow"));
        self.flush_if_needed();
    }

    fn flush_if_needed(&mut self) {
        if self.pending_events_since_flush < TRACE_RECORDER_FLUSH_EVERY_EVENTS {
            return;
        }
        self.writer
            .flush()
            .unwrap_or_else(|error| panic!("flush input trace event failed: {error}"));
        self.pending_events_since_flush = 0;
    }

    fn flush_all(&mut self) {
        if self.pending_events_since_flush == 0 {
            return;
        }
        self.writer
            .flush()
            .unwrap_or_else(|error| panic!("flush input trace event failed: {error}"));
        self.pending_events_since_flush = 0;
    }
}

impl Drop for InputTraceRecorder {
    fn drop(&mut self) {
        self.flush_all();
    }
}

pub struct InputTraceReplay {
    replay_elapsed_micros: u64,
    events: Vec<RecordedInputEvent>,
    next_event_index: usize,
    completion_logged: bool,
}

impl InputTraceReplay {
    pub fn from_path(path: PathBuf) -> Self {
        let file = File::open(&path)
            .unwrap_or_else(|error| panic!("open input trace file '{}': {error}", path.display()));
        let reader = BufReader::new(file);
        let mut events: Vec<RecordedInputEvent> = Vec::new();
        for (line_index, line_result) in reader.lines().enumerate() {
            let line_number = line_index
                .checked_add(1)
                .unwrap_or_else(|| panic!("input trace line number overflow"));
            let line = line_result
                .unwrap_or_else(|error| panic!("read input trace line {line_number}: {error}"));
            if line.trim().is_empty() {
                continue;
            }
            let event = serde_json::from_str::<RecordedInputEvent>(&line).unwrap_or_else(|error| {
                panic!("invalid input trace json at line {line_number}: {error}")
            });
            assert!(
                event.schema_version == 1,
                "unsupported input trace schema version {} at line {line_number}",
                event.schema_version
            );
            if let Some(previous) = events.last() {
                assert!(
                    event.event_id > previous.event_id,
                    "non-monotonic input trace event_id at line {line_number}: {} <= {}",
                    event.event_id,
                    previous.event_id
                );
                assert!(
                    event.elapsed_micros >= previous.elapsed_micros,
                    "non-monotonic input trace elapsed_micros at line {line_number}: {} < {}",
                    event.elapsed_micros,
                    previous.elapsed_micros
                );
            }
            events.push(event);
        }
        Self {
            replay_elapsed_micros: 0,
            events,
            next_event_index: 0,
            completion_logged: false,
        }
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    pub fn restart_clock(&mut self) {
        self.replay_elapsed_micros = 0;
    }

    pub fn has_pending_events(&self) -> bool {
        self.next_event_index < self.events.len()
    }

    pub fn advance_and_take_ready_events(
        &mut self,
        delta_micros: u64,
    ) -> Vec<RecordedInputEventKind> {
        self.replay_elapsed_micros = self
            .replay_elapsed_micros
            .checked_add(delta_micros)
            .unwrap_or_else(|| panic!("input replay timeline overflow"));
        let mut ready = Vec::new();
        while self.next_event_index < self.events.len()
            && self.events[self.next_event_index].elapsed_micros <= self.replay_elapsed_micros
        {
            ready.push(self.events[self.next_event_index].kind.clone());
            self.next_event_index = self
                .next_event_index
                .checked_add(1)
                .unwrap_or_else(|| panic!("input replay index overflow"));
        }
        ready
    }

    pub fn take_completion_notice(&mut self) -> bool {
        if self.next_event_index >= self.events.len() && !self.completion_logged {
            self.completion_logged = true;
            true
        } else {
            false
        }
    }
}

pub struct OutputTraceRecorder {
    started_at: Instant,
    scenario_id: String,
    run_id: String,
    writer: BufWriter<File>,
    next_event_id: u64,
    next_merge_request_id: u64,
    pending_events_since_flush: u32,
}

impl OutputTraceRecorder {
    pub fn from_path(path: PathBuf, scenario_id: String) -> Self {
        let file = File::create(&path).unwrap_or_else(|error| {
            panic!("create output trace file '{}': {error}", path.display())
        });
        let run_id = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_else(|error| panic!("system clock before UNIX_EPOCH: {error}"))
            .as_micros()
            .to_string();
        Self {
            started_at: Instant::now(),
            scenario_id,
            run_id,
            writer: BufWriter::new(file),
            next_event_id: 1,
            next_merge_request_id: 1,
            pending_events_since_flush: 0,
        }
    }

    pub fn record(&mut self, tick: u64, phase: OutputPhase, payload: OutputPayload) {
        let kind = match &payload {
            OutputPayload::Driver(_) => OutputKind::Driver,
            OutputPayload::BrushExecution(_) => OutputKind::BrushExecution,
            OutputPayload::RenderCommand(_) => OutputKind::RenderCommand,
            OutputPayload::MergeLifecycle(_) => OutputKind::MergeLifecycle,
            OutputPayload::StateDigest(_) => OutputKind::StateDigest,
        };
        let event = OutputEvent {
            envelope: replay_protocol::EventEnvelope {
                schema_version: 1,
                scenario_id: self.scenario_id.clone(),
                run_id: self.run_id.clone(),
                event_id: self.next_event_id,
                tick,
                phase,
                kind,
            },
            payload,
            debug: Some(DebugOutput {
                wall_time_micros: Some(
                    u64::try_from(self.started_at.elapsed().as_micros())
                        .unwrap_or_else(|_| panic!("output trace timestamp overflow")),
                ),
            }),
            debug_wall_time_micros: None,
        };
        self.next_event_id = self
            .next_event_id
            .checked_add(1)
            .unwrap_or_else(|| panic!("output trace event id overflow"));
        write_jsonl_event_line(&mut self.writer, &event)
            .unwrap_or_else(|error| panic!("write output trace event failed: {error}"));
        self.pending_events_since_flush = self
            .pending_events_since_flush
            .checked_add(1)
            .unwrap_or_else(|| panic!("output trace pending event counter overflow"));
        self.flush_if_needed();
    }

    fn flush_if_needed(&mut self) {
        if self.pending_events_since_flush < TRACE_RECORDER_FLUSH_EVERY_EVENTS {
            return;
        }
        self.writer
            .flush()
            .unwrap_or_else(|error| panic!("flush output trace event failed: {error}"));
        self.pending_events_since_flush = 0;
    }

    fn flush_all(&mut self) {
        if self.pending_events_since_flush == 0 {
            return;
        }
        self.writer
            .flush()
            .unwrap_or_else(|error| panic!("flush output trace event failed: {error}"));
        self.pending_events_since_flush = 0;
    }

    pub fn next_merge_request_id(&mut self) -> u64 {
        let merge_request_id = self.next_merge_request_id;
        self.next_merge_request_id = self
            .next_merge_request_id
            .checked_add(1)
            .unwrap_or_else(|| panic!("output trace merge request id overflow"));
        merge_request_id
    }
}

impl Drop for OutputTraceRecorder {
    fn drop(&mut self) {
        self.flush_all();
    }
}

#[derive(Debug)]
pub enum OutputCompareError {
    ReadExpected { path: PathBuf, message: String },
    ReadActual { path: PathBuf, message: String },
    ExpectedValidation(ValidationError),
    ActualValidation(ValidationError),
    SemanticMismatch(CompareError),
}

impl Display for OutputCompareError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputCompareError::ReadExpected { path, message } => {
                write!(
                    formatter,
                    "read expected output '{}': {message}",
                    path.display()
                )
            }
            OutputCompareError::ReadActual { path, message } => {
                write!(
                    formatter,
                    "read actual output '{}': {message}",
                    path.display()
                )
            }
            OutputCompareError::ExpectedValidation(error) => {
                write!(formatter, "expected output validation failed: {error:?}")
            }
            OutputCompareError::ActualValidation(error) => {
                write!(formatter, "actual output validation failed: {error:?}")
            }
            OutputCompareError::SemanticMismatch(error) => {
                write!(formatter, "output semantic mismatch: {error:?}")
            }
        }
    }
}

pub fn compare_output_files(
    expected_path: &Path,
    actual_path: &Path,
) -> Result<(), OutputCompareError> {
    let mut expected_reader = BufReader::new(File::open(expected_path).map_err(|error| {
        OutputCompareError::ReadExpected {
            path: expected_path.to_path_buf(),
            message: error.to_string(),
        }
    })?);
    let mut actual_reader = BufReader::new(File::open(actual_path).map_err(|error| {
        OutputCompareError::ReadActual {
            path: actual_path.to_path_buf(),
            message: error.to_string(),
        }
    })?);

    let expected = read_jsonl_events(&mut expected_reader).map_err(|error| {
        OutputCompareError::ReadExpected {
            path: expected_path.to_path_buf(),
            message: error.to_string(),
        }
    })?;
    let actual =
        read_jsonl_events(&mut actual_reader).map_err(|error| OutputCompareError::ReadActual {
            path: actual_path.to_path_buf(),
            message: error.to_string(),
        })?;

    validate_event_stream(&expected).map_err(OutputCompareError::ExpectedValidation)?;
    validate_event_stream(&actual).map_err(OutputCompareError::ActualValidation)?;
    compare_semantic_events(&expected, &actual).map_err(OutputCompareError::SemanticMismatch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use replay_protocol::{OutputPayload, RenderCommandOutput};
    use serde_json::Value;

    fn test_event(event_id: u64, tick: u64) -> OutputEvent {
        OutputEvent {
            envelope: replay_protocol::EventEnvelope {
                schema_version: 1,
                scenario_id: String::from("scenario"),
                run_id: String::from("run"),
                event_id,
                tick,
                phase: OutputPhase::EnqueueBeforeGpu,
                kind: OutputKind::RenderCommand,
            },
            payload: OutputPayload::RenderCommand(RenderCommandOutput {
                stroke_session_id: 9,
                command_kind: replay_protocol::BrushCommandKind::BeginStroke,
                tile_count: 0,
                tile_keys_digest: String::from("fx:0"),
                blend_mode: String::from("N/A"),
            }),
            debug: Some(DebugOutput {
                wall_time_micros: Some(1),
            }),
            debug_wall_time_micros: None,
        }
    }

    fn write_events(path: &Path, events: &[OutputEvent]) {
        let mut writer = BufWriter::new(
            File::create(path)
                .unwrap_or_else(|error| panic!("create test trace '{}': {error}", path.display())),
        );
        for event in events {
            write_jsonl_event_line(&mut writer, event)
                .unwrap_or_else(|error| panic!("write test trace event failed: {error}"));
        }
        writer
            .flush()
            .unwrap_or_else(|error| panic!("flush test trace failed: {error}"));
    }

    #[test]
    fn input_trace_recorder_writes_jsonl() {
        let base =
            std::env::temp_dir().join(format!("window_replay_input_jsonl_{}", std::process::id()));
        std::fs::create_dir_all(&base)
            .unwrap_or_else(|error| panic!("create temp test dir '{}': {error}", base.display()));
        let path = base.join("input.jsonl");
        {
            let mut recorder = InputTraceRecorder::from_path(path.clone());
            recorder.record(RecordedInputEventKind::MouseInput { pressed: true });
        }
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read input trace '{}': {error}", path.display()));
        let first_line = content
            .lines()
            .next()
            .unwrap_or_else(|| panic!("input trace '{}' is empty", path.display()));
        let parsed: Value = serde_json::from_str(first_line)
            .unwrap_or_else(|error| panic!("parse input json line failed: {error}"));
        assert_eq!(parsed["schema_version"], Value::from(1));
        assert_eq!(parsed["event_id"], Value::from(1));
        assert_eq!(parsed["kind"], Value::from("mouse_input"));
        assert_eq!(parsed["pressed"], Value::from(true));
    }

    #[test]
    fn input_trace_replay_reads_jsonl() {
        let base =
            std::env::temp_dir().join(format!("window_replay_input_replay_{}", std::process::id()));
        std::fs::create_dir_all(&base)
            .unwrap_or_else(|error| panic!("create temp test dir '{}': {error}", base.display()));
        let path = base.join("input.jsonl");
        let lines = [
            "{\"schema_version\":1,\"event_id\":1,\"elapsed_micros\":0,\"kind\":\"mouse_input\",\"pressed\":true}",
            "{\"schema_version\":1,\"event_id\":2,\"elapsed_micros\":1,\"kind\":\"cursor_moved\",\"x\":1.0,\"y\":2.0}",
        ];
        std::fs::write(&path, lines.join("\n"))
            .unwrap_or_else(|error| panic!("write input trace '{}': {error}", path.display()));

        let replay = InputTraceReplay::from_path(path);
        assert_eq!(replay.event_count(), 2);
    }

    #[test]
    fn input_trace_replay_uses_simulated_timeline() {
        let base = std::env::temp_dir().join(format!(
            "window_replay_input_timeline_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&base)
            .unwrap_or_else(|error| panic!("create temp test dir '{}': {error}", base.display()));
        let path = base.join("input.jsonl");
        let lines = [
            "{\"schema_version\":1,\"event_id\":1,\"elapsed_micros\":1000,\"kind\":\"mouse_input\",\"pressed\":true}",
            "{\"schema_version\":1,\"event_id\":2,\"elapsed_micros\":2500,\"kind\":\"cursor_moved\",\"x\":1.0,\"y\":2.0}",
        ];
        std::fs::write(&path, lines.join("\n"))
            .unwrap_or_else(|error| panic!("write input trace '{}': {error}", path.display()));

        let mut replay = InputTraceReplay::from_path(path);
        assert!(replay.has_pending_events());
        assert!(replay.advance_and_take_ready_events(500).is_empty());
        assert_eq!(replay.advance_and_take_ready_events(500).len(), 1);
        assert_eq!(replay.advance_and_take_ready_events(1000).len(), 0);
        assert_eq!(replay.advance_and_take_ready_events(500).len(), 1);
        assert!(!replay.has_pending_events());
    }

    #[test]
    fn compare_output_files_accepts_matching_semantics() {
        let base =
            std::env::temp_dir().join(format!("window_replay_compare_ok_{}", std::process::id()));
        std::fs::create_dir_all(&base)
            .unwrap_or_else(|error| panic!("create temp test dir '{}': {error}", base.display()));
        let expected_path = base.join("expected.jsonl");
        let actual_path = base.join("actual.jsonl");
        let mut expected_event = test_event(1, 1);
        let mut actual_event = expected_event.clone();
        expected_event.debug = Some(DebugOutput {
            wall_time_micros: Some(10),
        });
        actual_event.debug = Some(DebugOutput {
            wall_time_micros: Some(20),
        });
        write_events(&expected_path, &[expected_event]);
        write_events(&actual_path, &[actual_event]);

        let result = compare_output_files(expected_path.as_path(), actual_path.as_path());
        assert!(result.is_ok());
    }

    #[test]
    fn compare_output_files_rejects_mismatched_payload() {
        let base =
            std::env::temp_dir().join(format!("window_replay_compare_fail_{}", std::process::id()));
        std::fs::create_dir_all(&base)
            .unwrap_or_else(|error| panic!("create temp test dir '{}': {error}", base.display()));
        let expected_path = base.join("expected.jsonl");
        let actual_path = base.join("actual.jsonl");
        let expected_event = test_event(1, 1);
        let mut actual_event = test_event(1, 1);
        actual_event.payload = OutputPayload::RenderCommand(RenderCommandOutput {
            stroke_session_id: 99,
            command_kind: replay_protocol::BrushCommandKind::BeginStroke,
            tile_count: 0,
            tile_keys_digest: String::from("fx:0"),
            blend_mode: String::from("N/A"),
        });
        write_events(&expected_path, &[expected_event]);
        write_events(&actual_path, &[actual_event]);

        let result = compare_output_files(expected_path.as_path(), actual_path.as_path());
        assert!(matches!(
            result,
            Err(OutputCompareError::SemanticMismatch(
                CompareError::EventMismatch { .. }
            ))
        ));
    }
}
