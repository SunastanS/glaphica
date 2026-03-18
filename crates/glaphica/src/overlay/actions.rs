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

#[derive(Default)]
pub struct PendingActions {
    pub brush_update: Option<(BrushKind, Vec<BrushConfigValue>)>,
    pub layer_select: Option<NodeId>,
    pub layer_create: Option<NewLayerKind>,
    pub group_create: bool,
    pub layer_move: Option<(NodeId, LayerMoveTarget)>,
    pub layer_visibility: Option<(NodeId, bool)>,
    pub layer_opacity: Option<(NodeId, f32)>,
    pub layer_blend_mode: Option<(NodeId, UiBlendMode)>,
    pub document_save: Option<PathBuf>,
    pub document_load: Option<PathBuf>,
    pub document_export: Option<PathBuf>,
    pub exit_confirm_action: Option<ExitConfirmAction>,
    pub path_dialog_cancelled: bool,
}
