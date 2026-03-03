use glaphica_core::BrushId;

use crate::{BrushDrawInputLayout, BrushRegistry, BrushRegistryError};

pub struct BrushLayoutRegistry {
    layouts: BrushRegistry<BrushDrawInputLayout>,
}

impl BrushLayoutRegistry {
    pub fn new(max_brushes: usize) -> Self {
        Self {
            layouts: BrushRegistry::with_max_brushes(max_brushes),
        }
    }

    pub fn register_layout(
        &mut self,
        brush_id: BrushId,
        layout: BrushDrawInputLayout,
    ) -> Result<(), BrushRegistryError> {
        self.layouts.register(brush_id, layout)
    }

    pub fn ensure_can_register_layout(&self, brush_id: BrushId) -> Result<(), BrushRegistryError> {
        self.layouts.ensure_can_register(brush_id)
    }

    pub fn layout(&self, brush_id: BrushId) -> Result<BrushDrawInputLayout, BrushRegistryError> {
        self.layouts.get(brush_id).copied()
    }
}

#[cfg(test)]
mod tests {
    use glaphica_core::BrushId;

    use crate::{BrushDrawInputLayout, BrushDrawInputShape, BrushDrawKind};

    use super::BrushLayoutRegistry;

    #[test]
    fn register_and_get_layout_by_brush_id() {
        let mut registry = BrushLayoutRegistry::new(4);
        let layout = BrushDrawInputLayout::new(
            BrushDrawKind::PixelRect,
            &[BrushDrawInputShape::Vec2F32, BrushDrawInputShape::F32],
        );
        assert!(registry.register_layout(BrushId(2), layout).is_ok());
        assert_eq!(registry.layout(BrushId(2)), Ok(layout));
    }
}
