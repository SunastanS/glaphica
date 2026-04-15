use brush::{BrushId, BrushRegistry, round::ROUND_SHADER_REGISTRATION};
use renderer::{BrushShaderProvider, BrushShaderSpec};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AppBrushRegistry {
    brushes: BrushRegistry,
}

impl AppBrushRegistry {
    pub fn new() -> Self {
        Self {
            brushes: BrushRegistry::new(),
        }
    }

    pub fn with_builtin_round() -> Self {
        let mut registry = Self::new();
        registry.register_round();
        registry
    }

    pub fn register_round(&mut self) {
        self.brushes.register(ROUND_SHADER_REGISTRATION);
    }

    pub fn shader_spec(&self, brush_id: BrushId) -> Option<BrushShaderSpec> {
        self.brushes.shader_spec(brush_id)
    }
}

impl BrushShaderProvider for AppBrushRegistry {
    fn shader_spec(&self, brush_id: BrushId) -> Option<BrushShaderSpec> {
        self.shader_spec(brush_id)
    }
}

#[cfg(test)]
mod tests {
    use brush::round::{ROUND_BRUSH_ID, ROUND_SHADER_SPEC};

    use crate::brush_registry::AppBrushRegistry;

    #[test]
    fn builtin_round_registration_exposes_shader_spec() {
        let registry = AppBrushRegistry::with_builtin_round();

        assert_eq!(registry.shader_spec(ROUND_BRUSH_ID), Some(ROUND_SHADER_SPEC));
    }
}
