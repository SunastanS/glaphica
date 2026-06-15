use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use gla_core::{CanvasCoordF, CanvasInput};
use serde::{Deserialize, Serialize};

use crate::{DocumentBlendMode, RoundBrushSettings};

const TRACE_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppTraceConfig {
    Disabled,
    Record { path: PathBuf },
    Replay { path: PathBuf },
}

#[derive(Debug, Default)]
pub(crate) struct AppTraceState {
    recorder: Option<AppTraceRecorder>,
    replay: Option<AppTraceReplay>,
    last_saved_path: Option<PathBuf>,
}

#[derive(Debug)]
struct AppTraceRecorder {
    events: Vec<AppTraceEvent>,
    path: PathBuf,
}

#[derive(Debug)]
struct AppTraceReplay {
    events: Vec<AppTraceEvent>,
    index: usize,
    path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppTraceMode {
    Idle,
    Recording,
    Replaying,
    ReplayDone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppTraceStatus {
    pub mode: AppTraceMode,
    pub event_count: usize,
    pub replay_index: usize,
    pub path: Option<String>,
}

#[derive(Debug)]
pub enum AppTraceError {
    Io(std::io::Error),
    Json(serde_json::Error),
    UnsupportedVersion(u32),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppTraceFile {
    version: u32,
    events: Vec<AppTraceEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AppTraceEvent {
    Ui(AppTraceUiAction),
    BeginStroke(AppTraceCanvasInput),
    StrokeSample(AppTraceCanvasInput),
    FinishStroke,
    CancelStroke,
    Undo,
    Redo,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AppTraceUiAction {
    Undo,
    CreateLayer,
    CreateGroup,
    DeleteActiveNode,
    SelectLayer {
        visible_index: usize,
    },
    SetLayerOpacity {
        visible_index: usize,
        opacity: f32,
    },
    SetLayerBlendMode {
        visible_index: usize,
        blend_mode: AppTraceBlendMode,
    },
    SetRoundBrushSettings(AppTraceRoundBrushSettings),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppTraceBlendMode {
    Normal,
    Overlay,
    Multiply,
    MaskAlpha,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppTraceRoundBrushSettings {
    base_radius_px: f32,
    spacing_ratio: f32,
    base_hardness: f32,
    base_flow: f32,
    base_opacity: f32,
    tint: [f32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AppTraceCanvasInput {
    time_ns: u64,
    position: [f32; 2],
    pressure: f32,
    tilt: [f32; 2],
    twist: f32,
}

impl Default for AppTraceConfig {
    fn default() -> Self {
        Self::Disabled
    }
}

impl AppTraceConfig {
    pub fn record(path: impl Into<PathBuf>) -> Self {
        Self::Record { path: path.into() }
    }

    pub fn replay(path: impl Into<PathBuf>) -> Self {
        Self::Replay { path: path.into() }
    }

    pub fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }
}

impl AppTraceState {
    pub(crate) fn from_config(config: &AppTraceConfig) -> Result<Self, AppTraceError> {
        let mut state = Self::default();
        match config {
            AppTraceConfig::Disabled => {}
            AppTraceConfig::Record { path } => state.start_recording(path),
            AppTraceConfig::Replay { path } => state.load_replay(path)?,
        }
        Ok(state)
    }

    pub(crate) fn start_recording(&mut self, path: &Path) {
        self.replay = None;
        self.recorder = Some(AppTraceRecorder {
            events: Vec::new(),
            path: path.to_path_buf(),
        });
    }

    pub(crate) fn stop_recording(&mut self) -> Result<Option<PathBuf>, AppTraceError> {
        let Some(recorder) = self.recorder.take() else {
            return Ok(None);
        };
        let path = recorder.path;
        save_trace_file(&path, recorder.events)?;
        self.last_saved_path = Some(path.clone());
        Ok(Some(path))
    }

    pub(crate) fn load_replay(&mut self, path: &Path) -> Result<(), AppTraceError> {
        let events = load_trace_file(path)?;
        self.recorder = None;
        self.replay = Some(AppTraceReplay {
            events,
            index: 0,
            path: path.to_path_buf(),
        });
        Ok(())
    }

    pub(crate) fn record(&mut self, event: AppTraceEvent) {
        if let Some(recorder) = self.recorder.as_mut() {
            recorder.events.push(event);
        }
    }

    pub(crate) fn next_replay_event(&mut self) -> Option<AppTraceEvent> {
        let replay = self.replay.as_mut()?;
        let event = replay.events.get(replay.index)?.clone();
        replay.index += 1;
        Some(event)
    }

    pub(crate) fn is_replaying(&self) -> bool {
        self.replay
            .as_ref()
            .is_some_and(|replay| replay.index < replay.events.len())
    }

    pub(crate) fn is_recording(&self) -> bool {
        self.recorder.is_some()
    }

    pub fn status(&self) -> AppTraceStatus {
        if let Some(recorder) = self.recorder.as_ref() {
            return AppTraceStatus {
                mode: AppTraceMode::Recording,
                event_count: recorder.events.len(),
                replay_index: 0,
                path: Some(recorder.path.display().to_string()),
            };
        }
        if let Some(replay) = self.replay.as_ref() {
            return AppTraceStatus {
                mode: if replay.index >= replay.events.len() {
                    AppTraceMode::ReplayDone
                } else {
                    AppTraceMode::Replaying
                },
                event_count: replay.events.len(),
                replay_index: replay.index,
                path: Some(replay.path.display().to_string()),
            };
        }
        AppTraceStatus {
            mode: AppTraceMode::Idle,
            event_count: 0,
            replay_index: 0,
            path: self
                .last_saved_path
                .as_ref()
                .map(|path| path.display().to_string()),
        }
    }
}

impl From<CanvasInput> for AppTraceCanvasInput {
    fn from(value: CanvasInput) -> Self {
        Self {
            time_ns: value.time_ns,
            position: [value.position.x, value.position.y],
            pressure: value.pressure,
            tilt: [value.tilt.0, value.tilt.1],
            twist: value.twist,
        }
    }
}

impl From<DocumentBlendMode> for AppTraceBlendMode {
    fn from(value: DocumentBlendMode) -> Self {
        match value {
            DocumentBlendMode::Normal => Self::Normal,
            DocumentBlendMode::Overlay => Self::Overlay,
            DocumentBlendMode::Multiply => Self::Multiply,
            DocumentBlendMode::MaskAlpha => Self::MaskAlpha,
        }
    }
}

impl From<AppTraceBlendMode> for DocumentBlendMode {
    fn from(value: AppTraceBlendMode) -> Self {
        match value {
            AppTraceBlendMode::Normal => Self::Normal,
            AppTraceBlendMode::Overlay => Self::Overlay,
            AppTraceBlendMode::Multiply => Self::Multiply,
            AppTraceBlendMode::MaskAlpha => Self::MaskAlpha,
        }
    }
}

impl From<RoundBrushSettings> for AppTraceRoundBrushSettings {
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

impl From<AppTraceRoundBrushSettings> for RoundBrushSettings {
    fn from(value: AppTraceRoundBrushSettings) -> Self {
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

impl From<AppTraceCanvasInput> for CanvasInput {
    fn from(value: AppTraceCanvasInput) -> Self {
        Self {
            time_ns: value.time_ns,
            position: CanvasCoordF::new(value.position[0], value.position[1]),
            pressure: value.pressure,
            tilt: (value.tilt[0], value.tilt[1]),
            twist: value.twist,
        }
    }
}

impl Display for AppTraceError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "trace io error: {error}"),
            Self::Json(error) => write!(f, "trace json error: {error}"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported app trace version {version}")
            }
        }
    }
}

impl Error for AppTraceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::UnsupportedVersion(_) => None,
        }
    }
}

impl From<std::io::Error> for AppTraceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for AppTraceError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub fn save_trace_file(path: &Path, events: Vec<AppTraceEvent>) -> Result<(), AppTraceError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let writer = BufWriter::new(File::create(path)?);
    serde_json::to_writer_pretty(
        writer,
        &AppTraceFile {
            version: TRACE_VERSION,
            events,
        },
    )?;
    Ok(())
}

pub fn load_trace_file(path: &Path) -> Result<Vec<AppTraceEvent>, AppTraceError> {
    let reader = BufReader::new(File::open(path)?);
    let file: AppTraceFile = serde_json::from_reader(reader)?;
    if file.version != TRACE_VERSION {
        return Err(AppTraceError::UnsupportedVersion(file.version));
    }
    Ok(file.events)
}

#[cfg(test)]
mod tests {
    use super::{
        AppTraceBlendMode, AppTraceCanvasInput, AppTraceConfig, AppTraceEvent, AppTraceMode,
        AppTraceState, AppTraceUiAction, load_trace_file,
    };
    use gla_core::{CanvasCoordF, CanvasInput};

    fn trace_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "glaphica-app-trace-{name}-{}.json",
            std::process::id()
        ))
    }

    #[test]
    fn recording_saves_replayable_trace_file() {
        let path = trace_path("recording");
        let mut trace = AppTraceState::default();

        trace.start_recording(&path);
        trace.record(AppTraceEvent::Undo);
        trace.stop_recording().unwrap().unwrap();

        let events = load_trace_file(&path).unwrap();
        assert_eq!(events, vec![AppTraceEvent::Undo]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn trace_file_preserves_ui_actions_as_readable_json() {
        let path = trace_path("ui-action");
        let event = AppTraceEvent::Ui(AppTraceUiAction::SetLayerBlendMode {
            visible_index: 2,
            blend_mode: AppTraceBlendMode::Multiply,
        });

        super::save_trace_file(&path, vec![event.clone()]).unwrap();
        let json = std::fs::read_to_string(&path).unwrap();
        let events = load_trace_file(&path).unwrap();

        assert_eq!(events, vec![event]);
        assert!(json.contains("\"Ui\""));
        assert!(json.contains("\"SetLayerBlendMode\""));
        assert!(json.contains("\"visible_index\": 2"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn replay_loads_events_from_config() {
        let path = trace_path("replay");
        super::save_trace_file(
            &path,
            vec![AppTraceEvent::BeginStroke(AppTraceCanvasInput::from(
                CanvasInput {
                    time_ns: 7,
                    position: CanvasCoordF::new(1.0, 2.0),
                    pressure: 0.5,
                    tilt: (0.1, 0.2),
                    twist: 0.3,
                },
            ))],
        )
        .unwrap();

        let mut trace = AppTraceState::from_config(&AppTraceConfig::replay(path.clone())).unwrap();
        let status = trace.status();
        let event = trace.next_replay_event().unwrap();

        assert_eq!(status.mode, AppTraceMode::Replaying);
        assert_eq!(status.event_count, 1);
        assert!(matches!(event, AppTraceEvent::BeginStroke(_)));
        assert!(!trace.is_replaying());
        let _ = std::fs::remove_file(path);
    }
}
