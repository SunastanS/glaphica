use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use replay_protocol::{
    CompareError, OutputEvent, OutputKind, OutputPayload, OutputPhase, ValidationError,
    compare_semantic_events, read_jsonl_events, validate_event_stream, write_jsonl_event_line,
};

#[derive(Debug, Clone)]
pub enum RecordedInputEventKind {
    MouseInput { pressed: bool },
    CursorMoved { x: f64, y: f64 },
    MouseWheelLine { vertical_lines: f32 },
    MouseWheelPixel { delta_y: f64 },
}

#[derive(Debug, Clone)]
struct RecordedInputEvent {
    elapsed_micros: u64,
    kind: RecordedInputEventKind,
}

pub struct InputTraceRecorder {
    started_at: Instant,
    writer: BufWriter<File>,
}

impl InputTraceRecorder {
    pub fn from_path(path: PathBuf) -> Self {
        let file = File::create(&path).unwrap_or_else(|error| {
            panic!("create input trace file '{}': {error}", path.display())
        });
        Self {
            started_at: Instant::now(),
            writer: BufWriter::new(file),
        }
    }

    pub fn record(&mut self, kind: RecordedInputEventKind) {
        let elapsed_micros = self.started_at.elapsed().as_micros();
        let elapsed_micros = u64::try_from(elapsed_micros)
            .unwrap_or_else(|_| panic!("input trace timestamp overflow"));
        let line = match kind {
            RecordedInputEventKind::MouseInput { pressed } => {
                format!(
                    "{elapsed_micros}\tmouse_input\t{}",
                    if pressed { "1" } else { "0" }
                )
            }
            RecordedInputEventKind::CursorMoved { x, y } => {
                format!("{elapsed_micros}\tcursor_moved\t{x}\t{y}")
            }
            RecordedInputEventKind::MouseWheelLine { vertical_lines } => {
                format!("{elapsed_micros}\tmouse_wheel_line\t{vertical_lines}")
            }
            RecordedInputEventKind::MouseWheelPixel { delta_y } => {
                format!("{elapsed_micros}\tmouse_wheel_pixel\t{delta_y}")
            }
        };
        writeln!(self.writer, "{line}")
            .unwrap_or_else(|error| panic!("write input trace event failed: {error}"));
        self.writer
            .flush()
            .unwrap_or_else(|error| panic!("flush input trace event failed: {error}"));
    }
}

pub struct InputTraceReplay {
    started_at: Instant,
    events: Vec<RecordedInputEvent>,
    next_event_index: usize,
    completion_logged: bool,
}

impl InputTraceReplay {
    pub fn from_path(path: PathBuf) -> Self {
        let file = File::open(&path)
            .unwrap_or_else(|error| panic!("open input trace file '{}': {error}", path.display()));
        let reader = BufReader::new(file);
        let mut events = Vec::new();
        for (line_index, line_result) in reader.lines().enumerate() {
            let line_number = line_index
                .checked_add(1)
                .unwrap_or_else(|| panic!("input trace line number overflow"));
            let line = line_result
                .unwrap_or_else(|error| panic!("read input trace line {line_number}: {error}"));
            if line.trim().is_empty() {
                continue;
            }
            events.push(parse_recorded_input_event(&line, line_number));
        }
        Self {
            started_at: Instant::now(),
            events,
            next_event_index: 0,
            completion_logged: false,
        }
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    pub fn restart_clock(&mut self) {
        self.started_at = Instant::now();
    }

    pub fn take_ready_events(&mut self) -> Vec<RecordedInputEventKind> {
        let elapsed_micros = self.started_at.elapsed().as_micros();
        let elapsed_micros = u64::try_from(elapsed_micros)
            .unwrap_or_else(|_| panic!("input replay timestamp overflow"));
        let mut ready = Vec::new();
        while self.next_event_index < self.events.len()
            && self.events[self.next_event_index].elapsed_micros <= elapsed_micros
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

fn parse_recorded_input_event(line: &str, line_number: usize) -> RecordedInputEvent {
    let fields = line.split('\t').collect::<Vec<_>>();
    assert!(
        fields.len() >= 3,
        "invalid input trace format at line {line_number}: expected at least 3 fields"
    );
    let elapsed_micros = fields[0].parse::<u64>().unwrap_or_else(|error| {
        panic!("invalid input trace timestamp at line {line_number}: {error}")
    });
    let kind = match fields[1] {
        "mouse_input" => {
            assert!(
                fields.len() == 3,
                "invalid mouse_input trace format at line {line_number}"
            );
            let pressed = match fields[2] {
                "1" => true,
                "0" => false,
                value => panic!("invalid mouse_input value '{value}' at line {line_number}"),
            };
            RecordedInputEventKind::MouseInput { pressed }
        }
        "cursor_moved" => {
            assert!(
                fields.len() == 4,
                "invalid cursor_moved trace format at line {line_number}"
            );
            let x = fields[2]
                .parse::<f64>()
                .unwrap_or_else(|error| panic!("invalid cursor x at line {line_number}: {error}"));
            let y = fields[3]
                .parse::<f64>()
                .unwrap_or_else(|error| panic!("invalid cursor y at line {line_number}: {error}"));
            RecordedInputEventKind::CursorMoved { x, y }
        }
        "mouse_wheel_line" => {
            assert!(
                fields.len() == 3,
                "invalid mouse_wheel_line trace format at line {line_number}"
            );
            let vertical_lines = fields[2].parse::<f32>().unwrap_or_else(|error| {
                panic!("invalid mouse wheel line delta at line {line_number}: {error}")
            });
            RecordedInputEventKind::MouseWheelLine { vertical_lines }
        }
        "mouse_wheel_pixel" => {
            assert!(
                fields.len() == 3,
                "invalid mouse_wheel_pixel trace format at line {line_number}"
            );
            let delta_y = fields[2].parse::<f64>().unwrap_or_else(|error| {
                panic!("invalid mouse wheel pixel delta at line {line_number}: {error}")
            });
            RecordedInputEventKind::MouseWheelPixel { delta_y }
        }
        kind => panic!("unknown input trace event kind '{kind}' at line {line_number}"),
    };
    RecordedInputEvent {
        elapsed_micros,
        kind,
    }
}

pub struct OutputTraceRecorder {
    started_at: Instant,
    scenario_id: String,
    run_id: String,
    writer: BufWriter<File>,
    next_event_id: u64,
    next_merge_request_id: u64,
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
            debug_wall_time_micros: Some(
                u64::try_from(self.started_at.elapsed().as_micros())
                    .unwrap_or_else(|_| panic!("output trace timestamp overflow")),
            ),
        };
        self.next_event_id = self
            .next_event_id
            .checked_add(1)
            .unwrap_or_else(|| panic!("output trace event id overflow"));
        write_jsonl_event_line(&mut self.writer, &event)
            .unwrap_or_else(|error| panic!("write output trace event failed: {error}"));
        self.writer
            .flush()
            .unwrap_or_else(|error| panic!("flush output trace event failed: {error}"));
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
            debug_wall_time_micros: Some(1),
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
    fn compare_output_files_accepts_matching_semantics() {
        let base =
            std::env::temp_dir().join(format!("window_replay_compare_ok_{}", std::process::id()));
        std::fs::create_dir_all(&base)
            .unwrap_or_else(|error| panic!("create temp test dir '{}': {error}", base.display()));
        let expected_path = base.join("expected.jsonl");
        let actual_path = base.join("actual.jsonl");
        let mut expected_event = test_event(1, 1);
        let mut actual_event = expected_event.clone();
        expected_event.debug_wall_time_micros = Some(10);
        actual_event.debug_wall_time_micros = Some(20);
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
