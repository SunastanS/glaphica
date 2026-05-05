use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use brush::round::RoundBrushSettings;
use glaphica_core::{BlendMode, CanvasInput, CanvasVec2, RadianVec2};
use serde::{Deserialize, Serialize};

const TRACE_VERSION: u32 = 1;

#[derive(Debug)]
pub(super) enum PreviewTraceError {
    Io(std::io::Error),
    Json(serde_json::Error),
    UnsupportedVersion(u32),
}

impl Display for PreviewTraceError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "trace io error: {error}"),
            Self::Json(error) => write!(f, "trace json error: {error}"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported preview trace version {version}")
            }
        }
    }
}

impl Error for PreviewTraceError {}

impl From<std::io::Error> for PreviewTraceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for PreviewTraceError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug, Default)]
pub(super) struct PreviewTraceState {
    recorder: Option<PreviewTraceRecorder>,
    replay: Option<PreviewTraceReplay>,
    last_saved_path: Option<PathBuf>,
}

#[derive(Debug)]
struct PreviewTraceRecorder {
    events: Vec<PreviewTraceEvent>,
    path: PathBuf,
}

#[derive(Debug)]
struct PreviewTraceReplay {
    events: Vec<PreviewTraceEvent>,
    index: usize,
    path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreviewTraceMode {
    Idle,
    Recording,
    Replaying,
    ReplayDone,
}

#[derive(Debug, Clone)]
pub(super) struct PreviewTraceUiState {
    pub mode: PreviewTraceMode,
    pub event_count: usize,
    pub replay_index: usize,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PreviewTraceFile {
    version: u32,
    events: Vec<PreviewTraceEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) enum PreviewTraceEvent {
    Ui(PreviewTraceUiAction),
    BeginStroke,
    StrokeSample(PreviewTraceCanvasInput),
    EndStroke,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) enum PreviewTraceUiAction {
    Undo,
    CreateLayer,
    CreateGroup,
    DeleteActiveLayer,
    SelectLayer {
        visible_index: usize,
    },
    SetLayerOpacity {
        visible_index: usize,
        opacity: f32,
    },
    SetLayerBlendMode {
        visible_index: usize,
        blend_mode: PreviewTraceBlendMode,
    },
    SetRoundBrushSettings(PreviewTraceRoundBrushSettings),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(super) enum PreviewTraceBlendMode {
    Normal,
    Multiply,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PreviewTraceRoundBrushSettings {
    base_radius_px: f32,
    spacing_ratio: f32,
    base_hardness: f32,
    base_flow: f32,
    base_opacity: f32,
    tint: [f32; 3],
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(super) struct PreviewTraceCanvasInput {
    time_ns: u64,
    position: [f32; 2],
    pressure: f32,
    tilt: [f32; 2],
    twist: f32,
}

impl PreviewTraceState {
    pub(super) fn start_recording(&mut self, path: &Path) {
        self.replay = None;
        self.recorder = Some(PreviewTraceRecorder {
            events: Vec::new(),
            path: path.to_path_buf(),
        });
    }

    pub(super) fn stop_recording(&mut self) -> Result<Option<PathBuf>, PreviewTraceError> {
        let Some(recorder) = self.recorder.take() else {
            return Ok(None);
        };
        let path = recorder.path;
        save_trace_file(&path, recorder.events)?;
        self.last_saved_path = Some(path.clone());
        Ok(Some(path))
    }

    pub(super) fn load_replay(&mut self, path: &Path) -> Result<(), PreviewTraceError> {
        let events = load_trace_file(path)?;
        self.recorder = None;
        self.replay = Some(PreviewTraceReplay {
            events,
            index: 0,
            path: path.to_path_buf(),
        });
        Ok(())
    }

    pub(super) fn record(&mut self, event: PreviewTraceEvent) {
        if let Some(recorder) = self.recorder.as_mut() {
            recorder.events.push(event);
        }
    }

    pub(super) fn next_replay_event(&mut self) -> Option<PreviewTraceEvent> {
        let replay = self.replay.as_mut()?;
        let event = replay.events.get(replay.index)?.clone();
        replay.index += 1;
        Some(event)
    }

    pub(super) fn is_replaying(&self) -> bool {
        self.replay
            .as_ref()
            .is_some_and(|replay| replay.index < replay.events.len())
    }

    pub(super) fn is_recording(&self) -> bool {
        self.recorder.is_some()
    }

    pub(super) fn ui_state(&self) -> PreviewTraceUiState {
        if let Some(recorder) = self.recorder.as_ref() {
            return PreviewTraceUiState {
                mode: PreviewTraceMode::Recording,
                event_count: recorder.events.len(),
                replay_index: 0,
                path: Some(recorder.path.display().to_string()),
            };
        }
        if let Some(replay) = self.replay.as_ref() {
            return PreviewTraceUiState {
                mode: if replay.index >= replay.events.len() {
                    PreviewTraceMode::ReplayDone
                } else {
                    PreviewTraceMode::Replaying
                },
                event_count: replay.events.len(),
                replay_index: replay.index,
                path: Some(replay.path.display().to_string()),
            };
        }
        PreviewTraceUiState {
            mode: PreviewTraceMode::Idle,
            event_count: 0,
            replay_index: 0,
            path: self
                .last_saved_path
                .as_ref()
                .map(|path| path.display().to_string()),
        }
    }
}

impl From<BlendMode> for PreviewTraceBlendMode {
    fn from(value: BlendMode) -> Self {
        match value {
            BlendMode::Normal => Self::Normal,
            BlendMode::Multiply => Self::Multiply,
        }
    }
}

impl From<PreviewTraceBlendMode> for BlendMode {
    fn from(value: PreviewTraceBlendMode) -> Self {
        match value {
            PreviewTraceBlendMode::Normal => Self::Normal,
            PreviewTraceBlendMode::Multiply => Self::Multiply,
        }
    }
}

impl From<RoundBrushSettings> for PreviewTraceRoundBrushSettings {
    fn from(value: RoundBrushSettings) -> Self {
        Self {
            base_radius_px: value.base_radius_px,
            spacing_ratio: value.spacing_ratio,
            base_hardness: value.base_hardness,
            base_flow: value.base_flow,
            base_opacity: value.base_opacity,
            tint: value.tint,
        }
    }
}

impl PreviewTraceRoundBrushSettings {
    pub(super) fn apply_to(self, settings: &mut RoundBrushSettings) {
        settings.base_radius_px = self.base_radius_px;
        settings.spacing_ratio = self.spacing_ratio;
        settings.base_hardness = self.base_hardness;
        settings.base_flow = self.base_flow;
        settings.base_opacity = self.base_opacity;
        settings.tint = self.tint;
    }
}

impl From<CanvasInput> for PreviewTraceCanvasInput {
    fn from(value: CanvasInput) -> Self {
        Self {
            time_ns: value.time_ns,
            position: [value.position.x, value.position.y],
            pressure: value.pressure,
            tilt: [value.tilt.x, value.tilt.y],
            twist: value.twist,
        }
    }
}

impl From<PreviewTraceCanvasInput> for CanvasInput {
    fn from(value: PreviewTraceCanvasInput) -> Self {
        Self {
            time_ns: value.time_ns,
            position: CanvasVec2::new(value.position[0], value.position[1]),
            pressure: value.pressure,
            tilt: RadianVec2::new(value.tilt[0], value.tilt[1]),
            twist: value.twist,
        }
    }
}

fn save_trace_file(path: &Path, events: Vec<PreviewTraceEvent>) -> Result<(), PreviewTraceError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let writer = BufWriter::new(File::create(path)?);
    serde_json::to_writer_pretty(
        writer,
        &PreviewTraceFile {
            version: TRACE_VERSION,
            events,
        },
    )?;
    Ok(())
}

fn load_trace_file(path: &Path) -> Result<Vec<PreviewTraceEvent>, PreviewTraceError> {
    let reader = BufReader::new(File::open(path)?);
    let file: PreviewTraceFile = serde_json::from_reader(reader)?;
    if file.version != TRACE_VERSION {
        return Err(PreviewTraceError::UnsupportedVersion(file.version));
    }
    Ok(file.events)
}

#[cfg(test)]
mod tests {
    use super::{PreviewTraceEvent, PreviewTraceState, PreviewTraceUiAction, load_trace_file};

    #[test]
    fn recording_saves_replayable_trace_file() {
        let unique = format!("glaphica-preview-trace-test-{}.json", std::process::id());
        let path = std::env::temp_dir().join(unique);

        let mut trace = PreviewTraceState::default();
        trace.start_recording(&path);
        trace.record(PreviewTraceEvent::Ui(PreviewTraceUiAction::CreateLayer));
        trace
            .stop_recording()
            .expect("trace should save")
            .expect("path should be returned");

        let events = load_trace_file(&path).expect("trace should load");
        assert!(matches!(
            events.as_slice(),
            [PreviewTraceEvent::Ui(PreviewTraceUiAction::CreateLayer)]
        ));
        let _ = std::fs::remove_file(path);
    }
}
