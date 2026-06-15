mod display;
mod document;
mod frame;
mod runtime;
mod tool;
mod view;

pub use display::{ScreenBlitter, ScreenPresentCache, SurfaceError, SurfaceFrame, SurfaceRuntime};
pub use document::{DEFAULT_CANVAS_HEIGHT_PX, DEFAULT_CANVAS_WIDTH_PX};
pub use document::{
    DocumentPresentError, DocumentWorkspace, DocumentWorkspaceBuildError, DocumentWorkspaceError,
    DocumentWorkspaceInitError, ReplaceCircleStrokeSample,
};
pub use gla_session::{DrawCommit, DrawHistory, DrawRecordId};
pub use runtime::{AppRunError, AppRuntimeConfig, run_app_window, run_app_window_with_config};
pub use tool::{ActiveTool, BrushId, BrushSettings, Tool, ToolSet};
pub use view::{AppView, AppViewMatrixError};
