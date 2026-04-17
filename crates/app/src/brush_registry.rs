use atlas::Backend;
use brush::{
    BrushBackend, BrushId, BrushInput, BrushInputError, BrushRegistry, BrushStrokeInputProcessor,
    round::{ROUND_SHADER_REGISTRATION, RoundBrushInputProcessor},
};
use renderer::{BrushShaderProvider, BrushShaderSpec};

#[derive(Default)]
pub struct AppBrushRegistry {
    brushes: BrushRegistry,
}

impl AppBrushRegistry {
    pub fn new() -> Self {
        Self {
            brushes: BrushRegistry::new(),
        }
    }

    pub fn with_builtin_round(intermediate_backend: Backend) -> Self {
        let mut registry = Self::new();
        registry
            .register_round(intermediate_backend)
            .expect("builtin round backend should register");
        registry
    }

    pub fn register_round(
        &mut self,
        intermediate_backend: Backend,
    ) -> Result<(), brush::BrushStrokeError> {
        self.brushes.register(
            ROUND_SHADER_REGISTRATION,
            intermediate_backend,
            Box::new(RoundBrushInputProcessor::default()),
        )
    }

    pub fn shader_spec(&self, brush_id: BrushId) -> Option<BrushShaderSpec> {
        self.brushes.shader_spec(brush_id)
    }

    pub fn begin_stroke(&self, brush_id: BrushId) -> Option<brush::BrushStrokeState> {
        self.brushes.begin_stroke(brush_id)
    }

    pub fn brush_backend(&self, brush_id: BrushId) -> Option<&BrushBackend> {
        self.brushes.backend(brush_id)
    }

    pub fn brush_backend_mut(&mut self, brush_id: BrushId) -> Option<&mut BrushBackend> {
        self.brushes.backend_mut(brush_id)
    }

    pub fn begin_input_stroke(
        &self,
        brush_id: BrushId,
    ) -> Result<Box<dyn BrushStrokeInputProcessor>, BrushInputError> {
        self.brushes.begin_input_stroke(brush_id)
    }

    pub fn max_affected_radius_px(&self, brush_id: BrushId) -> Option<u32> {
        self.brushes.max_affected_radius_px(brush_id)
    }

    pub fn block_center(
        &self,
        input: &BrushInput,
        block_index: usize,
    ) -> Result<glaphica_core::CanvasVec2, BrushInputError> {
        self.brushes.block_center(input, block_index)
    }

    pub fn encode_apply_dab_payload(
        &self,
        input: &BrushInput,
        block_index: usize,
        tile_canvas_origin: glaphica_core::CanvasVec2,
    ) -> Result<Vec<u8>, BrushInputError> {
        self.brushes
            .encode_apply_dab_payload(input, block_index, tile_canvas_origin)
    }

    pub fn encode_merge_payload(&self, input: &BrushInput) -> Result<Vec<u8>, BrushInputError> {
        self.brushes.encode_merge_payload(input)
    }
}

impl BrushShaderProvider for AppBrushRegistry {
    fn shader_spec(&self, brush_id: BrushId) -> Option<BrushShaderSpec> {
        self.shader_spec(brush_id)
    }
}

#[cfg(test)]
mod tests {
    use atlas::{AtlasLayout, Backend, BackendId};
    use brush::round::{ROUND_BRUSH_ID, ROUND_SHADER_SPEC};

    use crate::brush_registry::AppBrushRegistry;

    #[test]
    fn builtin_round_registration_exposes_shader_spec_and_backend() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(19));
        let registry = AppBrushRegistry::with_builtin_round(backend);

        assert_eq!(
            registry.shader_spec(ROUND_BRUSH_ID),
            Some(ROUND_SHADER_SPEC)
        );
        assert!(registry.brush_backend(ROUND_BRUSH_ID).is_some());
    }
}
