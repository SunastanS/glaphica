use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;

use gla_core::CanvasInput;
use gla_draw_on::DrawOnInput;
use gla_ir::ImageId;
use gla_ir::{DocumentVersionId, DrawSessionIR, RegistryPatch};
use serde::{Deserialize, Serialize};

use crate::{ActiveTool, DocumentBlendMode, DocumentNodeId, RoundBrushSettings};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ScriptModuleId(u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptModuleSource {
    pub name: String,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ScriptValue {
    Nil,
    Bool(bool),
    Number(f64),
    String(String),
    Bytes(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ScriptCommand {
    ApplyRegistryPatch(RegistryPatch),
    OpenWorkspaceDirectory(PathBuf),
    AppendLayer {
        parent: DocumentNodeId,
    },
    AppendGroup {
        parent: DocumentNodeId,
    },
    DeleteNode(DocumentNodeId),
    MoveNode {
        node_id: DocumentNodeId,
        new_parent: DocumentNodeId,
        new_index: usize,
    },
    SetActiveNode(DocumentNodeId),
    SetNodeOpacity {
        node_id: DocumentNodeId,
        opacity: f32,
    },
    SetNodeBlendMode {
        node_id: DocumentNodeId,
        blend_mode: DocumentBlendMode,
    },
    RunDrawSession(ScriptDrawSession),
    SetActiveTool(ActiveTool),
    SetRoundBrushSettings(RoundBrushSettings),
    BeginStroke(CanvasInput),
    PushStrokeInput(CanvasInput),
    FinishStroke,
    CancelStroke,
    Undo,
    Redo,
    RequestRedraw,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScriptDrawSession {
    pub ir: DrawSessionIR,
    pub frames: Vec<ScriptDrawFrame>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScriptDrawFrame {
    pub commands: Vec<ScriptDrawCommand>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ScriptDrawCommand {
    DrawOn {
        target: ImageId,
        input: DrawOnInput,
    },
    DrawDab {
        shown_image: ImageId,
        input: CanvasInput,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ScriptCommandOutcome {
    None,
    DocumentVersion(DocumentVersionId),
    DocumentNode(DocumentNodeId),
    DirtyRootTiles(Vec<u32>),
    RedrawRequested,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptHostError {
    UnsupportedCommand { command: &'static str },
    InvalidCommand { reason: String },
    Runtime { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptRuntimeError {
    UnknownModule { id: ScriptModuleId },
    EntryUnavailable { id: ScriptModuleId, entry: String },
    Host(ScriptHostError),
    Runtime { reason: String },
}

#[derive(Default)]
pub struct NullScriptRuntime {
    modules: Vec<ScriptModuleSource>,
}

pub trait ScriptHost {
    fn execute_script_command(
        &mut self,
        command: ScriptCommand,
    ) -> Result<ScriptCommandOutcome, ScriptHostError>;
}

pub trait ScriptRuntime {
    type Error: Error + 'static;

    fn load_module(&mut self, source: ScriptModuleSource) -> Result<ScriptModuleId, Self::Error>;

    fn call_entry(
        &mut self,
        host: &mut dyn ScriptHost,
        module: ScriptModuleId,
        entry: &str,
        args: &[ScriptValue],
    ) -> Result<ScriptValue, Self::Error>;
}

impl ScriptModuleId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

impl ScriptModuleSource {
    pub fn new(name: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            source: source.into(),
        }
    }
}

impl ScriptDrawSession {
    pub fn new(ir: DrawSessionIR) -> Self {
        Self {
            ir,
            frames: Vec::new(),
        }
    }

    pub fn with_frames(ir: DrawSessionIR, frames: Vec<ScriptDrawFrame>) -> Self {
        Self { ir, frames }
    }
}

impl ScriptDrawFrame {
    pub fn new(commands: Vec<ScriptDrawCommand>) -> Self {
        Self { commands }
    }
}

pub fn script_draw_session_from_json_str(
    source: &str,
) -> Result<ScriptDrawSession, serde_json::Error> {
    serde_json::from_str(source)
}

pub fn script_draw_session_to_json_string_pretty(
    request: &ScriptDrawSession,
) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(request)
}

impl NullScriptRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn module_source(&self, id: ScriptModuleId) -> Option<&ScriptModuleSource> {
        self.modules.get(id.value() as usize)
    }
}

impl ScriptRuntime for NullScriptRuntime {
    type Error = ScriptRuntimeError;

    fn load_module(&mut self, source: ScriptModuleSource) -> Result<ScriptModuleId, Self::Error> {
        let id = ScriptModuleId::new(self.modules.len() as u64);
        self.modules.push(source);
        Ok(id)
    }

    fn call_entry(
        &mut self,
        _host: &mut dyn ScriptHost,
        module: ScriptModuleId,
        entry: &str,
        _args: &[ScriptValue],
    ) -> Result<ScriptValue, Self::Error> {
        if self.module_source(module).is_none() {
            return Err(ScriptRuntimeError::UnknownModule { id: module });
        }
        Err(ScriptRuntimeError::EntryUnavailable {
            id: module,
            entry: entry.to_owned(),
        })
    }
}

impl Display for ScriptHostError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedCommand { command } => {
                write!(f, "script command {command} is unsupported")
            }
            Self::InvalidCommand { reason } => write!(f, "invalid script command: {reason}"),
            Self::Runtime { reason } => write!(f, "script host runtime failed: {reason}"),
        }
    }
}

impl Error for ScriptHostError {}

impl Display for ScriptRuntimeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownModule { id } => write!(f, "script module {} is not loaded", id.value()),
            Self::EntryUnavailable { id, entry } => {
                write!(
                    f,
                    "script module {} has no callable entry {entry}",
                    id.value()
                )
            }
            Self::Host(error) => Display::fmt(error, f),
            Self::Runtime { reason } => write!(f, "script runtime failed: {reason}"),
        }
    }
}

impl Error for ScriptRuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Host(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ScriptHostError> for ScriptRuntimeError {
    fn from(error: ScriptHostError) -> Self {
        Self::Host(error)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        NullScriptRuntime, ScriptCommand, ScriptCommandOutcome, ScriptDrawSession, ScriptHost,
        ScriptHostError, ScriptModuleId, ScriptModuleSource, ScriptRuntime, ScriptRuntimeError,
        ScriptValue,
    };
    use crate::{ActiveTool, BrushId, DocumentBlendMode, DocumentNodeId, RoundBrushSettings};
    use gla_core::{CanvasCoordF, CanvasInput};
    use gla_ir::{DocImageUse, DocumentVersionId, DrawSessionIR, ImageId};

    #[derive(Default)]
    struct RecordingHost {
        commands: Vec<ScriptCommand>,
    }

    impl ScriptHost for RecordingHost {
        fn execute_script_command(
            &mut self,
            command: ScriptCommand,
        ) -> Result<ScriptCommandOutcome, ScriptHostError> {
            self.commands.push(command);
            Ok(ScriptCommandOutcome::None)
        }
    }

    #[test]
    fn null_runtime_loads_source_but_has_no_callable_entries() {
        let mut runtime = NullScriptRuntime::new();
        let mut host = RecordingHost::default();

        let id = runtime
            .load_module(ScriptModuleSource::new("startup.janet", "(print :ready)"))
            .unwrap();
        let error = runtime
            .call_entry(&mut host, id, "main", &[ScriptValue::Nil])
            .unwrap_err();

        assert_eq!(id, ScriptModuleId::new(0));
        assert_eq!(
            runtime.module_source(id).unwrap(),
            &ScriptModuleSource::new("startup.janet", "(print :ready)")
        );
        assert_eq!(
            error,
            ScriptRuntimeError::EntryUnavailable {
                id,
                entry: "main".to_owned()
            }
        );
    }

    #[test]
    fn null_runtime_reports_unknown_module() {
        let mut runtime = NullScriptRuntime::new();
        let mut host = RecordingHost::default();

        let error = runtime
            .call_entry(&mut host, ScriptModuleId::new(99), "main", &[])
            .unwrap_err();

        assert_eq!(
            error,
            ScriptRuntimeError::UnknownModule {
                id: ScriptModuleId::new(99)
            }
        );
    }

    #[test]
    fn script_commands_reserve_document_and_app_control_surface() {
        let ir = DrawSessionIR {
            expected_document_version: DocumentVersionId::new(7),
            doc_images: vec![DocImageUse::read(ImageId::new(1))],
            session_images: Vec::new(),
            draw_on: Vec::new(),
            derive: Vec::new(),
        };
        let input = CanvasInput {
            time_ns: 42,
            position: CanvasCoordF::new(3.0, 4.0),
            pressure: 0.5,
            tilt: (0.0, 0.0),
            twist: 0.0,
        };

        let commands = vec![
            ScriptCommand::RunDrawSession(ScriptDrawSession::new(ir.clone())),
            ScriptCommand::OpenWorkspaceDirectory("fixtures/workspace".into()),
            ScriptCommand::AppendLayer {
                parent: DocumentNodeId::new(1),
            },
            ScriptCommand::AppendGroup {
                parent: DocumentNodeId::new(1),
            },
            ScriptCommand::SetActiveNode(DocumentNodeId::new(2)),
            ScriptCommand::SetNodeOpacity {
                node_id: DocumentNodeId::new(2),
                opacity: 0.5,
            },
            ScriptCommand::SetNodeBlendMode {
                node_id: DocumentNodeId::new(2),
                blend_mode: DocumentBlendMode::Multiply,
            },
            ScriptCommand::MoveNode {
                node_id: DocumentNodeId::new(2),
                new_parent: DocumentNodeId::new(1),
                new_index: 0,
            },
            ScriptCommand::DeleteNode(DocumentNodeId::new(2)),
            ScriptCommand::SetActiveTool(ActiveTool::Brush(BrushId::DEFAULT)),
            ScriptCommand::SetRoundBrushSettings(RoundBrushSettings::default()),
            ScriptCommand::BeginStroke(input),
            ScriptCommand::PushStrokeInput(input),
            ScriptCommand::FinishStroke,
            ScriptCommand::Undo,
            ScriptCommand::Redo,
            ScriptCommand::RequestRedraw,
        ];

        assert!(matches!(&commands[0], ScriptCommand::RunDrawSession(found) if found.ir == ir));
        assert!(matches!(
            &commands[1],
            ScriptCommand::OpenWorkspaceDirectory(path) if path.ends_with("fixtures/workspace")
        ));
        assert!(matches!(
            commands[3],
            ScriptCommand::AppendGroup { parent } if parent == DocumentNodeId::new(1)
        ));
        assert!(matches!(
            commands[6],
            ScriptCommand::SetNodeBlendMode {
                node_id,
                blend_mode: DocumentBlendMode::Multiply,
            } if node_id == DocumentNodeId::new(2)
        ));
        assert!(matches!(
            commands[10],
            ScriptCommand::SetRoundBrushSettings(_)
        ));
        assert!(matches!(commands[11], ScriptCommand::BeginStroke(found) if found == input));
    }

    #[test]
    fn script_host_trait_accepts_reserved_commands() {
        let mut host = RecordingHost::default();

        let outcome = host
            .execute_script_command(ScriptCommand::CancelStroke)
            .unwrap();

        assert_eq!(outcome, ScriptCommandOutcome::None);
        assert_eq!(host.commands, vec![ScriptCommand::CancelStroke]);
    }
}
