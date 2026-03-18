use brushes::{BrushConfigItem, BrushConfigValue};
use glaphica_core::BrushId;

pub const ROUND_BRUSH_ID: BrushId = BrushId(0);
pub const PIXEL_RECT_BRUSH_ID: BrushId = BrushId(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushKind {
    Round,
    PixelRect,
}

impl BrushKind {
    pub const ALL: [Self; 2] = [Self::Round, Self::PixelRect];

    pub const fn brush_id(self) -> BrushId {
        match self {
            Self::Round => ROUND_BRUSH_ID,
            Self::PixelRect => PIXEL_RECT_BRUSH_ID,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Round => "Round",
            Self::PixelRect => "PixelRect",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BrushUiState {
    pub kind: BrushKind,
    pub color_rgb: [f32; 3],
    pub eraser: bool,
    pub items: Vec<BrushConfigItem>,
    pub values: Vec<BrushConfigValue>,
    pub visible: Vec<bool>,
    pub dirty: bool,
}

impl BrushUiState {
    pub fn new(kind: BrushKind, items: Vec<BrushConfigItem>) -> Self {
        let values = items
            .iter()
            .map(|item| item.default_value.clone())
            .collect::<Vec<_>>();
        let visible = items.iter().map(|item| !item.default_hidden).collect();
        Self {
            kind,
            color_rgb: [1.0, 0.0, 0.0],
            eraser: false,
            items,
            values,
            visible,
            dirty: false,
        }
    }

    pub fn reset(&mut self) {
        for (item, value) in self.items.iter().zip(self.values.iter_mut()) {
            *value = item.default_value.clone();
        }
        for (item, visible) in self.items.iter().zip(self.visible.iter_mut()) {
            *visible = !item.default_hidden;
        }
        self.dirty = true;
    }
}
