use glaphica_core::BrushId;

use crate::{BrushGpuPipelineSpec, BrushRegistry, BrushRegistryError};

pub struct BrushGpuPipelineRegistry {
    specs: BrushRegistry<BrushGpuPipelineSpec>,
}

impl BrushGpuPipelineRegistry {
    pub fn new(max_brushes: usize) -> Self {
        Self {
            specs: BrushRegistry::with_max_brushes(max_brushes),
        }
    }

    pub fn register_pipeline_spec(
        &mut self,
        brush_id: BrushId,
        spec: BrushGpuPipelineSpec,
    ) -> Result<(), BrushRegistryError> {
        self.specs.register(brush_id, spec)
    }

    pub fn ensure_can_register_pipeline_spec(
        &self,
        brush_id: BrushId,
    ) -> Result<(), BrushRegistryError> {
        self.specs.ensure_can_register(brush_id)
    }

    pub fn pipeline_spec(
        &self,
        brush_id: BrushId,
    ) -> Result<BrushGpuPipelineSpec, BrushRegistryError> {
        self.specs.get(brush_id).copied()
    }
}

#[cfg(test)]
mod tests {
    use glaphica_core::BrushId;

    use crate::BrushGpuPipelineSpec;

    use super::BrushGpuPipelineRegistry;

    #[test]
    fn register_and_get_pipeline_spec_by_brush_id() {
        let mut registry = BrushGpuPipelineRegistry::new(4);
        let spec = BrushGpuPipelineSpec {
            label: "test",
            wgsl_source: "@vertex fn vs_main(@builtin(vertex_index) idx:u32)->@builtin(position) vec4<f32>{ let p=array<vec2<f32>,3>(vec2<f32>(-1.0,-1.0),vec2<f32>(3.0,-1.0),vec2<f32>(-1.0,3.0)); let xy=p[idx]; return vec4<f32>(xy,0.0,1.0);} @fragment fn fs_main()->@location(0) vec4<f32>{ return vec4<f32>(1.0); }",
            vertex_entry: "vs_main",
            fragment_entry: "fs_main",
            uses_brush_cache_backend: false,
            cache_backend_format: None,
        };
        assert!(registry.register_pipeline_spec(BrushId(2), spec).is_ok());
        assert_eq!(registry.pipeline_spec(BrushId(2)), Ok(spec));
    }
}
