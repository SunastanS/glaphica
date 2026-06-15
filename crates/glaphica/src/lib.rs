mod display;
mod document;
mod egui_overlay;
mod export;
mod frame;
mod input_ring;
mod layer_tree;
mod runtime;
mod script;
mod stroke;
mod tool;
mod trace;
mod ui;
mod ui_components;
mod ui_overlay;
mod view;

pub use display::{ScreenBlitter, ScreenPresentCache, SurfaceError, SurfaceFrame, SurfaceRuntime};
pub use document::{DEFAULT_CANVAS_HEIGHT_PX, DEFAULT_CANVAS_WIDTH_PX};
pub use document::{
    DocumentBrushInputError, DocumentLayerRenderError, DocumentPresentError, DocumentRootTileRead,
    DocumentStrokePreviewError, DocumentWorkspace, DocumentWorkspaceBuildError,
    DocumentWorkspaceError, DocumentWorkspaceInitError, DocumentWorkspaceLayerError,
    ReplaceCircleStrokeSample,
};
pub use export::{
    WorkspaceExportError, WorkspaceExportManifest, WorkspaceExportSnapshot, WorkspaceExportTile,
    WorkspaceExportTileAsset, export_workspace_directory, import_workspace_directory,
    read_workspace_directory, read_workspace_manifest, root_tile_asset_relative_path,
    workspace_from_export_snapshot, write_workspace_manifest,
};
pub use gla_session::{DrawCommit, DrawHistory, DrawRecordId};
pub use input_ring::{OverwriteRingConsumer, OverwriteRingProducer, create_overwrite_ring};
pub use layer_tree::{
    DocumentBlendMode, DocumentLayerNode, DocumentLayerTree, DocumentLayerTreeError,
    DocumentNodeId, DocumentNodeKind,
};
pub use runtime::{
    AppPerfTraceConfig, AppRunError, AppRuntimeConfig, run_app_window, run_app_window_with_config,
};
pub use script::{
    NullScriptRuntime, ScriptCommand, ScriptCommandOutcome, ScriptCommandPlan, ScriptDrawCommand,
    ScriptDrawFrame, ScriptDrawSession, ScriptHost, ScriptHostError, ScriptModuleId,
    ScriptModuleSource, ScriptRuntime, ScriptRuntimeError, ScriptValue,
    script_command_plan_from_json_str, script_command_plan_to_json_string_pretty,
    script_draw_session_from_json_str, script_draw_session_to_json_string_pretty,
};
pub use stroke::{
    BrushInput, BrushInputBlock, BrushInputBlockList, BrushInputError, BrushInputProcessor,
    BrushStrokeInputProcessor, BrushThreadRuntimeError, BrushWorkerError, FrozenCanvasSample,
    ROUND_BRUSH_INPUT_BLOCK_VALUE_COUNT, RoundBrushInputProcessor, RoundMergeSettings,
    encode_round_apply_payload, encode_round_merge_payload,
};
pub use tool::{
    ActiveTool, BrushId, BrushSettings, CurvePoint, CurveValidationError, ModulationCurve,
    RoundBrushDabVariable, RoundBrushInputFeature, RoundBrushModulationSet, RoundBrushSettings,
    RoundBrushVariableModulation, Tool, ToolSet,
};
pub use trace::{
    AppTraceBlendMode, AppTraceCanvasInput, AppTraceConfig, AppTraceError, AppTraceEvent,
    AppTraceMode, AppTraceRoundBrushSettings, AppTraceStatus, AppTraceUiAction, load_trace_file,
    save_trace_file,
};
pub use ui::{
    UiAction, UiLayerItem, UiTraceMode, UiTraceStatus, collect_ui_layers, visible_layer_index,
};
pub use view::{AppView, AppViewMatrixError};
