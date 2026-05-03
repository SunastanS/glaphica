use atlas::Backend;
use brush::{
    BrushBackend, BrushId, BrushInput, BrushInputError, BrushRegistry, BrushStrokeInputProcessor,
    round::{
        ROUND_BRUSH_ID, ROUND_SHADER_REGISTRATION, RoundBrushInputProcessor, RoundBrushSettings,
    },
};
use renderer::{BrushShaderProvider, BrushShaderSpec};
use std::error::Error;
use std::fmt::{Display, Formatter};

#[derive(Default)]
pub struct AppBrushRegistry {
    brushes: BrushRegistry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppBrushRegistryUpdateError {
    BrushNotRegistered(BrushId),
}

impl Display for AppBrushRegistryUpdateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BrushNotRegistered(brush_id) => {
                write!(f, "brush {} is not registered", brush_id.raw())
            }
        }
    }
}

impl Error for AppBrushRegistryUpdateError {}

impl AppBrushRegistry {
    pub fn new() -> Self {
        Self {
            brushes: BrushRegistry::new(),
        }
    }

    pub fn with_builtin_round(brush_backend: Backend) -> Self {
        Self::with_builtin_round_processor(brush_backend, RoundBrushInputProcessor::default())
    }

    pub fn with_builtin_round_settings(
        brush_backend: Backend,
        settings: RoundBrushSettings,
    ) -> Self {
        Self::with_builtin_round_processor(brush_backend, RoundBrushInputProcessor::from(settings))
    }

    pub fn with_builtin_round_processor(
        brush_backend: Backend,
        input_processor: RoundBrushInputProcessor,
    ) -> Self {
        let mut registry = Self::new();
        registry
            .register_round_with_processor(brush_backend, input_processor)
            .expect("builtin round backend should register");
        registry
    }

    pub fn register_round(
        &mut self,
        brush_backend: Backend,
    ) -> Result<(), brush::BrushStrokeError> {
        self.register_round_with_processor(brush_backend, RoundBrushInputProcessor::default())
    }

    pub fn register_round_with_processor(
        &mut self,
        brush_backend: Backend,
        input_processor: RoundBrushInputProcessor,
    ) -> Result<(), brush::BrushStrokeError> {
        self.brushes.register(
            ROUND_SHADER_REGISTRATION,
            brush_backend,
            Box::new(input_processor),
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
        slot_canvas_origin: glaphica_core::CanvasVec2,
    ) -> Result<Vec<u8>, BrushInputError> {
        self.brushes
            .encode_apply_dab_payload(input, block_index, slot_canvas_origin)
    }

    pub fn merge_payload(&self, brush_id: BrushId) -> Option<Vec<u8>> {
        self.brushes.merge_payload(brush_id)
    }

    pub fn update_round_brush_settings(
        &mut self,
        settings: RoundBrushSettings,
    ) -> Result<(), AppBrushRegistryUpdateError> {
        if self.brushes.replace_input_processor(
            ROUND_BRUSH_ID,
            Box::new(RoundBrushInputProcessor::from(settings)),
        ) {
            return Ok(());
        }
        Err(AppBrushRegistryUpdateError::BrushNotRegistered(
            ROUND_BRUSH_ID,
        ))
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
    use renderer::BrushTileFormat;

    use crate::brush_registry::AppBrushRegistry;

    #[test]
    fn builtin_round_registration_exposes_shader_spec_and_backend() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(19));
        let registry = AppBrushRegistry::with_builtin_round(backend);

        assert_eq!(
            registry.shader_spec(ROUND_BRUSH_ID),
            Some(ROUND_SHADER_SPEC)
        );
        let brush_backend = registry
            .brush_backend(ROUND_BRUSH_ID)
            .expect("round backend should be registered");
        assert_eq!(brush_backend.brush_tile_format(), BrushTileFormat::R16Float);
    }
}
