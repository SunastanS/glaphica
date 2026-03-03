use std::error::Error;
use std::fmt::{Display, Formatter};

use glaphica_core::BrushId;

use crate::BrushRegistryError;
use crate::draw_layout::BrushDrawInputLayout;
use crate::engine_runtime::{BrushEngineRuntime, EngineBrushPipeline};
use crate::gpu_pipeline_registry::BrushGpuPipelineRegistry;
use crate::gpu_pipeline_spec::BrushGpuPipelineSpec;
use crate::layout_registry::BrushLayoutRegistry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushSpecRegisterError {
    Engine(BrushRegistryError),
    Layout(BrushRegistryError),
    GpuPipeline(BrushRegistryError),
}

impl Display for BrushSpecRegisterError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Engine(err) => write!(f, "failed to register engine pipeline: {err}"),
            Self::Layout(err) => write!(f, "failed to register draw layout: {err}"),
            Self::GpuPipeline(err) => write!(f, "failed to register gpu pipeline spec: {err}"),
        }
    }
}

impl Error for BrushSpecRegisterError {}

pub trait BrushSpec: EngineBrushPipeline + Sized + 'static {
    fn max_affected_radius_px(&self) -> u32;
    fn draw_input_layout(&self) -> BrushDrawInputLayout;
    fn gpu_pipeline_spec(&self) -> BrushGpuPipelineSpec;

    fn register(
        self,
        brush_id: BrushId,
        engine_runtime: &mut BrushEngineRuntime,
        layout_registry: &mut BrushLayoutRegistry,
        gpu_pipeline_registry: &mut BrushGpuPipelineRegistry,
    ) -> Result<(), BrushSpecRegisterError> {
        engine_runtime
            .ensure_can_register_pipeline(brush_id)
            .map_err(BrushSpecRegisterError::Engine)?;
        layout_registry
            .ensure_can_register_layout(brush_id)
            .map_err(BrushSpecRegisterError::Layout)?;
        gpu_pipeline_registry
            .ensure_can_register_pipeline_spec(brush_id)
            .map_err(BrushSpecRegisterError::GpuPipeline)?;

        let max_affected_radius_px = self.max_affected_radius_px();
        let draw_layout = self.draw_input_layout();
        let gpu_pipeline_spec = self.gpu_pipeline_spec();

        engine_runtime
            .register_pipeline(brush_id, max_affected_radius_px, self)
            .map_err(BrushSpecRegisterError::Engine)?;
        layout_registry
            .register_layout(brush_id, draw_layout)
            .map_err(BrushSpecRegisterError::Layout)?;
        gpu_pipeline_registry
            .register_pipeline_spec(brush_id, gpu_pipeline_spec)
            .map_err(BrushSpecRegisterError::GpuPipeline)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use glaphica_core::{BrushId, BrushInput, TileKey};

    use crate::{
        BrushDrawInputLayout, BrushDrawKind, BrushGpuPipelineRegistry, BrushGpuPipelineSpec,
    };
    use crate::{
        BrushDrawInputShape, BrushEngineRuntime, BrushLayoutRegistry, BrushSpec,
        BrushSpecRegisterError,
    };

    struct TestBrush;

    impl crate::EngineBrushPipeline for TestBrush {
        fn encode_draw_input(
            &mut self,
            _brush_input: &BrushInput,
            _tile_key: TileKey,
        ) -> Result<Vec<f32>, crate::BrushPipelineError> {
            Ok(vec![0.0, 0.0, 1.0])
        }
    }

    impl BrushSpec for TestBrush {
        fn max_affected_radius_px(&self) -> u32 {
            1
        }

        fn draw_input_layout(&self) -> BrushDrawInputLayout {
            BrushDrawInputLayout::new(
                BrushDrawKind::PixelRect,
                &[BrushDrawInputShape::Vec2F32, BrushDrawInputShape::F32],
            )
        }

        fn gpu_pipeline_spec(&self) -> BrushGpuPipelineSpec {
            BrushGpuPipelineSpec {
                label: "test-brush",
                wgsl_source: "@vertex fn vs_main(@builtin(vertex_index) idx:u32)->@builtin(position) vec4<f32>{ let p=array<vec2<f32>,3>(vec2<f32>(-1.0,-1.0),vec2<f32>(3.0,-1.0),vec2<f32>(-1.0,3.0)); let xy=p[idx]; return vec4<f32>(xy,0.0,1.0);} @fragment fn fs_main()->@location(0) vec4<f32>{ return vec4<f32>(1.0); }",
                vertex_entry: "vs_main",
                fragment_entry: "fs_main",
                uses_brush_cache_backend: false,
            }
        }
    }

    #[test]
    fn brush_spec_registers_engine_and_layout_together() {
        let mut engine_runtime = BrushEngineRuntime::new(4);
        let mut layout_registry = BrushLayoutRegistry::new(4);
        let mut gpu_pipeline_registry = BrushGpuPipelineRegistry::new(4);
        let register = TestBrush.register(
            BrushId(1),
            &mut engine_runtime,
            &mut layout_registry,
            &mut gpu_pipeline_registry,
        );
        assert!(register.is_ok());
        assert!(layout_registry.layout(BrushId(1)).is_ok());
        assert!(gpu_pipeline_registry.pipeline_spec(BrushId(1)).is_ok());
    }

    #[test]
    fn brush_spec_register_rejects_duplicate_brush_id() {
        let mut engine_runtime = BrushEngineRuntime::new(4);
        let mut layout_registry = BrushLayoutRegistry::new(4);
        let mut gpu_pipeline_registry = BrushGpuPipelineRegistry::new(4);
        let first = TestBrush.register(
            BrushId(2),
            &mut engine_runtime,
            &mut layout_registry,
            &mut gpu_pipeline_registry,
        );
        assert!(first.is_ok());

        let second = TestBrush.register(
            BrushId(2),
            &mut engine_runtime,
            &mut layout_registry,
            &mut gpu_pipeline_registry,
        );
        assert!(matches!(second, Err(BrushSpecRegisterError::Engine(_))));
    }
}
