use std::error::Error;
use std::fmt::{Display, Formatter};

use glaphica_core::{BrushId, BrushInput, TileKey};
use thread_protocol::{DrawOp, GpuCmdMsg};

use crate::brush_registry::BrushRegistry;
use crate::{BrushPipelineError, BrushRegistryError};

pub trait EngineBrushPipeline: Send {
    fn encode_draw_input(
        &mut self,
        brush_input: &BrushInput,
        tile_key: TileKey,
    ) -> Result<Vec<f32>, BrushPipelineError>;
}

#[derive(Debug)]
pub enum EngineBrushDispatchError {
    Registry(BrushRegistryError),
    Pipeline {
        brush_id: BrushId,
        source: BrushPipelineError,
    },
}

impl Display for EngineBrushDispatchError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Registry(err) => write!(f, "{err}"),
            Self::Pipeline { brush_id, source } => {
                write!(f, "engine brush pipeline {} failed: {source}", brush_id.0)
            }
        }
    }
}

impl Error for EngineBrushDispatchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registry(err) => Some(err),
            Self::Pipeline { source, .. } => Some(source.as_ref()),
        }
    }
}

impl From<BrushRegistryError> for EngineBrushDispatchError {
    fn from(value: BrushRegistryError) -> Self {
        Self::Registry(value)
    }
}

pub struct BrushEngineRuntime {
    pipelines: BrushRegistry<Box<dyn EngineBrushPipeline>>,
}

impl BrushEngineRuntime {
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
        P: EngineBrushPipeline + 'static,
    {
        self.pipelines.register(brush_id, Box::new(pipeline))
    }

    pub fn build_draw_op(
        &mut self,
        brush_id: BrushId,
        brush_input: &BrushInput,
        tile_key: TileKey,
    ) -> Result<DrawOp, EngineBrushDispatchError> {
        let pipeline = self.pipelines.get_mut(brush_id)?;
        let encoded_input = pipeline
            .encode_draw_input(brush_input, tile_key)
            .map_err(|source| EngineBrushDispatchError::Pipeline { brush_id, source })?;
        Ok(DrawOp {
            tile_key,
            input: encoded_input,
            brush_id,
        })
    }

    pub fn build_draw_cmd(
        &mut self,
        brush_id: BrushId,
        brush_input: &BrushInput,
        tile_key: TileKey,
    ) -> Result<GpuCmdMsg, EngineBrushDispatchError> {
        Ok(GpuCmdMsg::DrawOp(self.build_draw_op(
            brush_id,
            brush_input,
            tile_key,
        )?))
    }
}
