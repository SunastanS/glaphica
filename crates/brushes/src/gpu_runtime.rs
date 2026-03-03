use std::error::Error;
use std::fmt::{Display, Formatter};

use glaphica_core::BrushId;
use thread_protocol::{DrawOp, GpuCmdMsg};

use crate::brush_registry::BrushRegistry;
use crate::{BrushPipelineError, BrushRegistryError};

pub trait BrushGpuPipeline<Context>: Send {
    fn apply_draw_op(
        &mut self,
        context: &mut Context,
        draw_op: &DrawOp,
    ) -> Result<(), BrushPipelineError>;
}

#[derive(Debug)]
pub enum BrushGpuDispatchError {
    Registry(BrushRegistryError),
    Pipeline {
        brush_id: BrushId,
        source: BrushPipelineError,
    },
}

impl Display for BrushGpuDispatchError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Registry(err) => write!(f, "{err}"),
            Self::Pipeline { brush_id, source } => {
                write!(f, "gpu brush pipeline {} failed: {source}", brush_id.0)
            }
        }
    }
}

impl Error for BrushGpuDispatchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registry(err) => Some(err),
            Self::Pipeline { source, .. } => Some(source.as_ref()),
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

pub struct BrushGpuRuntime<Context> {
    pipelines: BrushRegistry<Box<dyn BrushGpuPipeline<Context>>>,
}

impl<Context> BrushGpuRuntime<Context> {
    pub fn new(max_brushes: usize) -> Self {
        Self {
            pipelines: BrushRegistry::with_max_brushes(max_brushes),
        }
    }

    pub fn register_pipeline<P>(
        &mut self,
        brush_id: BrushId,
        pipeline: P,
    ) -> Result<(), BrushRegistryError>
    where
        P: BrushGpuPipeline<Context> + 'static,
    {
        self.pipelines.register(brush_id, Box::new(pipeline))
    }

    pub fn apply_draw_op(
        &mut self,
        context: &mut Context,
        draw_op: &DrawOp,
    ) -> Result<(), BrushGpuDispatchError> {
        let brush_id = draw_op.brush_id;
        let pipeline = self.pipelines.get_mut(brush_id)?;
        pipeline
            .apply_draw_op(context, draw_op)
            .map_err(|source| BrushGpuDispatchError::Pipeline { brush_id, source })
    }

    pub fn apply_gpu_cmd(
        &mut self,
        context: &mut Context,
        command: &GpuCmdMsg,
    ) -> Result<BrushGpuApplyOutcome, BrushGpuDispatchError> {
        match command {
            GpuCmdMsg::DrawOp(draw_op) => {
                self.apply_draw_op(context, draw_op)?;
                Ok(BrushGpuApplyOutcome::AppliedDraw)
            }
            GpuCmdMsg::CopyOp(_) | GpuCmdMsg::ClearOp(_) => {
                Ok(BrushGpuApplyOutcome::IgnoredNonDraw)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use glaphica_core::{BrushId, TileKey};
    use thread_protocol::{ClearOp, DrawOp, GpuCmdMsg};

    use super::{BrushGpuApplyOutcome, BrushGpuPipeline, BrushGpuRuntime};

    struct TestGpuPipeline;

    impl BrushGpuPipeline<Vec<f32>> for TestGpuPipeline {
        fn apply_draw_op(
            &mut self,
            context: &mut Vec<f32>,
            draw_op: &DrawOp,
        ) -> Result<(), crate::BrushPipelineError> {
            context.extend_from_slice(&draw_op.input);
            Ok(())
        }
    }

    #[test]
    fn apply_draw_op_dispatches_by_brush_id() {
        let mut runtime = BrushGpuRuntime::<Vec<f32>>::new(4);
        assert!(
            runtime
                .register_pipeline(BrushId(2), TestGpuPipeline)
                .is_ok()
        );

        let mut context = Vec::new();
        let draw_op = DrawOp {
            tile_key: TileKey::from_parts(0, 0, 0),
            input: vec![1.0, 2.0, 3.0],
            brush_id: BrushId(2),
        };
        let apply_result = runtime.apply_draw_op(&mut context, &draw_op);
        assert!(apply_result.is_ok());
        assert_eq!(context, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn apply_gpu_cmd_ignores_non_draw_commands() {
        let mut runtime = BrushGpuRuntime::<Vec<f32>>::new(2);
        let mut context = Vec::new();
        let clear_cmd = GpuCmdMsg::ClearOp(ClearOp {
            tile_key: TileKey::from_parts(0, 0, 0),
        });
        let outcome = runtime.apply_gpu_cmd(&mut context, &clear_cmd);
        assert!(matches!(outcome, Ok(BrushGpuApplyOutcome::IgnoredNonDraw)));
        assert!(context.is_empty());
    }
}
