use crate::brush_ui::state::{BrushKind, PIXEL_RECT_BRUSH_ID, ROUND_BRUSH_ID};
use glaphica_core::BrushId;

pub fn brush_id_to_kind(brush_id: BrushId) -> Option<BrushKind> {
    match brush_id {
        ROUND_BRUSH_ID => Some(BrushKind::Round),
        PIXEL_RECT_BRUSH_ID => Some(BrushKind::PixelRect),
        _ => None,
    }
}

pub fn kind_to_brush_id(kind: BrushKind) -> BrushId {
    kind.brush_id()
}
