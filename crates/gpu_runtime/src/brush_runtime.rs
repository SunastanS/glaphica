use std::error::Error;
use std::fmt::{Display, Formatter};

use brushes::{BrushDrawInputLayout, BrushLayoutRegistry, BrushPipelineError, BrushRegistryError};
use glaphica_core::BrushId;
use thread_protocol::{DrawOp, GpuCmdMsg};

pub trait BrushDrawExecutor<Context>: Send {
    fn execute_draw(
        &mut self,
        context: &mut Context,
        draw_op: &DrawOp,
        layout: BrushDrawInputLayout,
    ) -> Result<(), BrushPipelineError>;

    fn execute_draw_with_encoder(
        &mut self,
        context: &mut Context,
        draw_op: &DrawOp,
        layout: BrushDrawInputLayout,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), BrushPipelineError>;
}

#[derive(Debug)]
pub enum BrushGpuDispatchError {
    Registry(BrushRegistryError),
    InputLayoutMismatch {
        brush_id: BrushId,
        layout: BrushDrawInputLayout,
        input_len: usize,
    },
    Executor {
        brush_id: BrushId,
        source: BrushPipelineError,
    },
}

impl Display for BrushGpuDispatchError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Registry(err) => write!(f, "{err}"),
            Self::InputLayoutMismatch {
                brush_id,
                layout,
                input_len,
            } => write!(
                f,
                "draw input layout mismatch for brush {} (layout: {layout:?}, input_len: {input_len})",
                brush_id.0
            ),
            Self::Executor { brush_id, source } => {
                write!(
                    f,
                    "gpu draw executor failed for brush {}: {source}",
                    brush_id.0
                )
            }
        }
    }
}

impl Error for BrushGpuDispatchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registry(err) => Some(err),
            Self::Executor { source, .. } => Some(source.as_ref()),
            Self::InputLayoutMismatch { .. } => None,
        }
    }
}

impl From<BrushRegistryError> for BrushGpuDispatchError {
    fn from(value: BrushRegistryError) -> Self {
        Self::Registry(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushGpuApplyOutcome {
    AppliedDraw,
    IgnoredNonDraw,
}

pub struct BrushGpuRuntime<Executor> {
    executor: Executor,
}

impl<Executor> BrushGpuRuntime<Executor> {
    pub fn new(executor: Executor) -> Self {
        Self { executor }
    }

    pub fn executor_mut(&mut self) -> &mut Executor {
        &mut self.executor
    }

    pub fn apply_draw_op<Context>(
        &mut self,
        context: &mut Context,
        draw_op: &DrawOp,
        layout_registry: &BrushLayoutRegistry,
    ) -> Result<(), BrushGpuDispatchError>
    where
        Executor: BrushDrawExecutor<Context>,
    {
        let brush_id = draw_op.brush_id;
        let layout = layout_registry.layout(brush_id)?;
        if !layout.validate_input(&draw_op.input) {
            return Err(BrushGpuDispatchError::InputLayoutMismatch {
                brush_id,
                layout,
                input_len: draw_op.input.len(),
            });
        }
        self.executor
            .execute_draw(context, draw_op, layout)
            .map_err(|source| BrushGpuDispatchError::Executor { brush_id, source })
    }

    pub fn apply_draw_op_with_encoder<Context>(
        &mut self,
        context: &mut Context,
        draw_op: &DrawOp,
        layout_registry: &BrushLayoutRegistry,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), BrushGpuDispatchError>
    where
        Executor: BrushDrawExecutor<Context>,
    {
        let brush_id = draw_op.brush_id;
        let layout = layout_registry.layout(brush_id)?;
        if !layout.validate_input(&draw_op.input) {
            return Err(BrushGpuDispatchError::InputLayoutMismatch {
                brush_id,
                layout,
                input_len: draw_op.input.len(),
            });
        }
        self.executor
            .execute_draw_with_encoder(context, draw_op, layout, encoder)
            .map_err(|source| BrushGpuDispatchError::Executor { brush_id, source })
    }

    pub fn apply_gpu_cmd<Context>(
        &mut self,
        context: &mut Context,
        command: &GpuCmdMsg,
        layout_registry: &BrushLayoutRegistry,
    ) -> Result<BrushGpuApplyOutcome, BrushGpuDispatchError>
    where
        Executor: BrushDrawExecutor<Context>,
    {
        match command {
            GpuCmdMsg::DrawOp(draw_op) => {
                self.apply_draw_op(context, draw_op, layout_registry)?;
                Ok(BrushGpuApplyOutcome::AppliedDraw)
            }
            GpuCmdMsg::CopyOp(_)
            | GpuCmdMsg::WriteOp(_)
            | GpuCmdMsg::ClearOp(_)
            | GpuCmdMsg::RenderTreeUpdated(_)
            | GpuCmdMsg::TileSlotKeyUpdate(_) => Ok(BrushGpuApplyOutcome::IgnoredNonDraw),
        }
    }
}

#[cfg(test)]
mod tests {
    use brushes::builtin_brushes::pixel_rect::PixelRectBrush;
    use brushes::{
        BrushDrawInputLayout, BrushDrawInputShape, BrushDrawKind, BrushEngineRuntime,
        BrushGpuPipelineRegistry, BrushGpuPipelineSpec, BrushLayoutRegistry, BrushSpec,
    };
    use glaphica_core::{BrushId, NodeId, TileKey};
    use thread_protocol::{ClearOp, DrawBlendMode, DrawFrameMergePolicy, DrawOp, GpuCmdMsg};

    use super::{BrushDrawExecutor, BrushGpuApplyOutcome, BrushGpuDispatchError, BrushGpuRuntime};

    #[derive(Default)]
    struct TestContext {
        input: Vec<f32>,
    }

    #[derive(Default)]
    struct TestExecutor;

    impl BrushDrawExecutor<TestContext> for TestExecutor {
        fn execute_draw(
            &mut self,
            context: &mut TestContext,
            draw_op: &DrawOp,
            _layout: BrushDrawInputLayout,
        ) -> Result<(), brushes::BrushPipelineError> {
            context.input.clear();
            context.input.extend_from_slice(&draw_op.input);
            Ok(())
        }

        fn execute_draw_with_encoder(
            &mut self,
            context: &mut TestContext,
            draw_op: &DrawOp,
            _layout: BrushDrawInputLayout,
            _encoder: &mut wgpu::CommandEncoder,
        ) -> Result<(), brushes::BrushPipelineError> {
            context.input.clear();
            context.input.extend_from_slice(&draw_op.input);
            Ok(())
        }
    }

    fn test_layout() -> BrushDrawInputLayout {
        BrushDrawInputLayout::new(
            BrushDrawKind::PixelRect,
            &[BrushDrawInputShape::Vec2F32, BrushDrawInputShape::F32],
        )
    }

    fn test_pipeline_spec() -> BrushGpuPipelineSpec {
        const TEST_WGSL: &str = "@vertex fn vs_main(@builtin(vertex_index) idx:u32)->@builtin(position) vec4<f32>{ let p=array<vec2<f32>,3>(vec2<f32>(-1.0,-1.0),vec2<f32>(3.0,-1.0),vec2<f32>(-1.0,3.0)); let xy=p[idx]; return vec4<f32>(xy,0.0,1.0);} @fragment fn fs_main()->@location(0) vec4<f32>{ return vec4<f32>(1.0); }";
        BrushGpuPipelineSpec {
            label: "test-brush",
            wgsl_source: TEST_WGSL,
            vertex_entry: "vs_main",
            fragment_entry: "fs_main",
            uses_brush_cache_backend: false,
            cache_backend_format: None,
        }
    }

    #[test]
    fn apply_draw_op_uses_registered_layout_and_executor() {
        let mut runtime = BrushGpuRuntime::new(TestExecutor);
        let mut layouts = BrushLayoutRegistry::new(4);
        let mut pipeline_registry = BrushGpuPipelineRegistry::new(4);
        assert!(layouts.register_layout(BrushId(2), test_layout()).is_ok());
        assert!(
            pipeline_registry
                .register_pipeline_spec(BrushId(2), test_pipeline_spec())
                .is_ok()
        );

        let draw_op = DrawOp {
            node_id: NodeId(0),
            tile_index: 0,
            tile_key: TileKey::from_parts(0, 0, 0),
            blend_mode: DrawBlendMode::Alpha,
            frame_merge: DrawFrameMergePolicy::None,
            origin_tile: TileKey::EMPTY,
            ref_image: None,
            input: vec![1.0, 2.0, 3.0],
            brush_id: BrushId(2),
        };
        let mut context = TestContext::default();
        let apply = runtime.apply_draw_op(&mut context, &draw_op, &layouts);
        assert!(apply.is_ok());
        assert_eq!(context.input, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn apply_draw_op_rejects_input_layout_mismatch() {
        let mut runtime = BrushGpuRuntime::new(TestExecutor);
        let mut layouts = BrushLayoutRegistry::new(4);
        let mut pipeline_registry = BrushGpuPipelineRegistry::new(4);
        assert!(layouts.register_layout(BrushId(1), test_layout()).is_ok());
        assert!(
            pipeline_registry
                .register_pipeline_spec(BrushId(1), test_pipeline_spec())
                .is_ok()
        );

        let draw_op = DrawOp {
            node_id: NodeId(0),
            tile_index: 0,
            tile_key: TileKey::from_parts(0, 0, 0),
            blend_mode: DrawBlendMode::Alpha,
            frame_merge: DrawFrameMergePolicy::None,
            origin_tile: TileKey::EMPTY,
            ref_image: None,
            input: vec![1.0, 2.0],
            brush_id: BrushId(1),
        };
        let mut context = TestContext::default();
        let apply = runtime.apply_draw_op(&mut context, &draw_op, &layouts);
        assert!(matches!(
            apply,
            Err(BrushGpuDispatchError::InputLayoutMismatch {
                brush_id: BrushId(1),
                input_len: 2,
                ..
            })
        ));
    }

    #[test]
    fn apply_gpu_cmd_ignores_non_draw_commands() {
        let mut runtime = BrushGpuRuntime::new(TestExecutor);
        let mut engine_runtime = BrushEngineRuntime::new(2);
        let mut layouts = BrushLayoutRegistry::new(2);
        let mut pipeline_registry = BrushGpuPipelineRegistry::new(2);
        let register = PixelRectBrush::new(1).register(
            BrushId(1),
            &mut engine_runtime,
            &mut layouts,
            &mut pipeline_registry,
        );
        assert!(register.is_ok());
        let mut context = TestContext::default();
        let clear_cmd = GpuCmdMsg::ClearOp(ClearOp {
            tile_key: TileKey::from_parts(0, 0, 0),
        });
        let outcome = runtime.apply_gpu_cmd(&mut context, &clear_cmd, &layouts);
        assert!(matches!(outcome, Ok(BrushGpuApplyOutcome::IgnoredNonDraw)));
    }
}
