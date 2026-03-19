use std::path::PathBuf;

use brushes::BrushConfigValue;
use document::{LayerMoveTarget, NewLayerKind, UiBlendMode};
use glaphica_core::NodeId;

use crate::brush_ui::state::BrushKind;

#[derive(Clone, Copy)]
pub enum ExitConfirmAction {
    SaveAndExit,
    DiscardAndExit,
    Cancel,
}

#[derive(Clone, Copy)]
pub enum PathDialogAction {
    Save,
    Load,
    Export,
}

pub enum UiCommand {
    BrushUpdated(BrushKind, Vec<BrushConfigValue>),
    LayerSelected(NodeId),
    LayerCreated(NewLayerKind),
    GroupCreated,
    LayerMoved(NodeId, LayerMoveTarget),
    LayerVisibilityChanged(NodeId, bool),
    LayerOpacityChanged(NodeId, f32),
    LayerBlendModeChanged(NodeId, UiBlendMode),
    DocumentSaveRequested(PathBuf),
    DocumentLoadRequested(PathBuf),
    DocumentExportRequested(PathBuf),
    ExitConfirmed(ExitConfirmAction),
    PathDialogCancelled,
}
