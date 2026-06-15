mod display;
mod document;
mod export;
mod frame;
mod runtime;
mod script;
mod stroke;
mod tool;
mod trace;
mod view;

pub use display::{ScreenBlitter, ScreenPresentCache, SurfaceError, SurfaceFrame, SurfaceRuntime};
pub use document::{DEFAULT_CANVAS_HEIGHT_PX, DEFAULT_CANVAS_WIDTH_PX};
pub use document::{
    DocumentPresentError, DocumentRootTileRead, DocumentWorkspace, DocumentWorkspaceBuildError,
    DocumentWorkspaceError, DocumentWorkspaceInitError, ReplaceCircleStrokeSample,
};
pub use export::{
    WorkspaceExportError, WorkspaceExportManifest, WorkspaceExportSnapshot, WorkspaceExportTile,
    WorkspaceExportTileAsset, export_workspace_directory, read_workspace_directory,
    read_workspace_manifest, root_tile_asset_relative_path, write_workspace_manifest,
};
pub use gla_session::{DrawCommit, DrawHistory, DrawRecordId};
pub use runtime::{
    AppPerfTraceConfig, AppRunError, AppRuntimeConfig, run_app_window, run_app_window_with_config,
};
pub use script::{
    NullScriptRuntime, ScriptCommand, ScriptCommandOutcome, ScriptDrawCommand, ScriptDrawFrame,
    ScriptDrawSession, ScriptHost, ScriptHostError, ScriptModuleId, ScriptModuleSource,
    ScriptRuntime, ScriptRuntimeError, ScriptValue, script_draw_session_from_json_str,
    script_draw_session_to_json_string_pretty,
};
pub use tool::{ActiveTool, BrushId, BrushSettings, RoundBrushSettings, Tool, ToolSet};
pub use trace::{
    AppTraceCanvasInput, AppTraceConfig, AppTraceError, AppTraceEvent, AppTraceMode,
    AppTraceStatus, load_trace_file, save_trace_file,
};
pub use view::{AppView, AppViewMatrixError};
