use std::error::Error;
use std::fmt::{Display, Formatter};

use glaphica_core::{BrushId, BrushInput, TileKey};
use images::Image;
use thread_protocol::{DrawOp, GpuCmdMsg, RefImage};

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

struct EngineBrushRegistration {
    max_affected_radius_px: u32,
    pipeline: Box<dyn EngineBrushPipeline>,
}

#[derive(Debug, Clone, Copy)]
struct AffectedTile {
    tile_key: TileKey,
    ref_tile_key: Option<TileKey>,
}

pub struct BrushEngineRuntime {
    pipelines: BrushRegistry<EngineBrushRegistration>,
    scratch_affected_tiles: Vec<AffectedTile>,
}

impl BrushEngineRuntime {
    pub fn new(max_brushes: usize) -> Self {
        Self {
            pipelines: BrushRegistry::with_max_brushes(max_brushes),
            scratch_affected_tiles: Vec::new(),
        }
    }

    pub fn register_pipeline<P>(
        &mut self,
        brush_id: BrushId,
        max_affected_radius_px: u32,
        pipeline: P,
    ) -> Result<(), BrushRegistryError>
    where
        P: EngineBrushPipeline + 'static,
    {
        self.pipelines.register(
            brush_id,
            EngineBrushRegistration {
                max_affected_radius_px,
                pipeline: Box::new(pipeline),
            },
        )
    }

    pub fn ensure_can_register_pipeline(
        &self,
        brush_id: BrushId,
    ) -> Result<(), BrushRegistryError> {
        self.pipelines.ensure_can_register(brush_id)
    }

    pub fn build_draw_op(
        &mut self,
        brush_id: BrushId,
        brush_input: &BrushInput,
        tile_key: TileKey,
    ) -> Result<DrawOp, EngineBrushDispatchError> {
        self.build_draw_op_with_ref_tile(brush_id, brush_input, tile_key, None)
    }

    pub fn build_draw_op_with_ref_tile(
        &mut self,
        brush_id: BrushId,
        brush_input: &BrushInput,
        tile_key: TileKey,
        ref_tile_key: Option<TileKey>,
    ) -> Result<DrawOp, EngineBrushDispatchError> {
        let registration = self.pipelines.get_mut(brush_id)?;
        let encoded_input = registration
            .pipeline
            .encode_draw_input(brush_input, tile_key)
            .map_err(|source| EngineBrushDispatchError::Pipeline { brush_id, source })?;
        Ok(DrawOp {
            tile_key,
            ref_image: ref_tile_key.map(|tile_key| RefImage { tile_key }),
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

    pub fn build_draw_ops_for_image(
        &mut self,
        brush_id: BrushId,
        brush_input: &BrushInput,
        image: &Image,
        output: &mut Vec<DrawOp>,
    ) -> Result<(), EngineBrushDispatchError> {
        self.dispatch_draw_ops_for_image(brush_id, brush_input, image, None, |draw_op| {
            output.push(draw_op)
        })
    }

    pub fn build_draw_ops_for_image_with_ref_image(
        &mut self,
        brush_id: BrushId,
        brush_input: &BrushInput,
        image: &Image,
        ref_image: Option<&Image>,
        output: &mut Vec<DrawOp>,
    ) -> Result<(), EngineBrushDispatchError> {
        self.dispatch_draw_ops_for_image(brush_id, brush_input, image, ref_image, |draw_op| {
            output.push(draw_op)
        })
    }

    pub fn build_draw_cmds_for_image(
        &mut self,
        brush_id: BrushId,
        brush_input: &BrushInput,
        image: &Image,
        output: &mut Vec<GpuCmdMsg>,
    ) -> Result<(), EngineBrushDispatchError> {
        self.dispatch_draw_ops_for_image(brush_id, brush_input, image, None, |draw_op| {
            output.push(GpuCmdMsg::DrawOp(draw_op))
        })
    }

    pub fn build_draw_cmds_for_image_with_ref_image(
        &mut self,
        brush_id: BrushId,
        brush_input: &BrushInput,
        image: &Image,
        ref_image: Option<&Image>,
        output: &mut Vec<GpuCmdMsg>,
    ) -> Result<(), EngineBrushDispatchError> {
        self.dispatch_draw_ops_for_image(brush_id, brush_input, image, ref_image, |draw_op| {
            output.push(GpuCmdMsg::DrawOp(draw_op))
        })
    }

    fn dispatch_draw_ops_for_image<F>(
        &mut self,
        brush_id: BrushId,
        brush_input: &BrushInput,
        image: &Image,
        ref_image: Option<&Image>,
        mut emit: F,
    ) -> Result<(), EngineBrushDispatchError>
    where
        F: FnMut(DrawOp),
    {
        let max_affected_radius_px = self.pipelines.get_mut(brush_id)?.max_affected_radius_px;
        self.scratch_affected_tiles.clear();
        let scratch_affected_tiles = &mut self.scratch_affected_tiles;
        image.for_each_affected_tile_key(
            brush_input.cursor.cursor,
            max_affected_radius_px,
            |tile_index, tile_key| {
                let ref_tile_key = ref_image.and_then(|image| image.tile_key(tile_index));
                scratch_affected_tiles.push(AffectedTile {
                    tile_key,
                    ref_tile_key,
                });
            },
        );

        let registration = self.pipelines.get_mut(brush_id)?;
        for affected_tile in self.scratch_affected_tiles.iter().copied() {
            let encoded_input = registration
                .pipeline
                .encode_draw_input(brush_input, affected_tile.tile_key)
                .map_err(|source| EngineBrushDispatchError::Pipeline { brush_id, source })?;
            emit(DrawOp {
                tile_key: affected_tile.tile_key,
                ref_image: affected_tile
                    .ref_tile_key
                    .map(|tile_key| RefImage { tile_key }),
                input: encoded_input,
                brush_id,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use glaphica_core::{
        BrushId, BrushInput, BrushInputFlags, CanvasVec2, IMAGE_TILE_SIZE, MappedCursor,
        RadianVec2, StrokeId, TileKey,
    };
    use images::{Image, layout::ImageLayout};

    use super::{BrushEngineRuntime, EngineBrushPipeline};

    struct TestEnginePipeline;

    impl EngineBrushPipeline for TestEnginePipeline {
        fn encode_draw_input(
            &mut self,
            _brush_input: &BrushInput,
            tile_key: TileKey,
        ) -> Result<Vec<f32>, crate::BrushPipelineError> {
            Ok(vec![tile_key.slot_index() as f32])
        }
    }

    fn build_test_brush_input(center: CanvasVec2) -> BrushInput {
        BrushInput {
            stroke: StrokeId(1),
            cursor: MappedCursor {
                cursor: center,
                tilt: RadianVec2::new(0.0, 0.0),
                pressure: 1.0,
                twist: 0.0,
            },
            flags: BrushInputFlags::empty(),
            path_s: 0.0,
            delta_s: 0.0,
            dt_s: 0.0,
            vel: CanvasVec2::new(0.0, 0.0),
            speed: 0.0,
            tangent: CanvasVec2::new(0.0, 0.0),
            acc: CanvasVec2::new(0.0, 0.0),
            accel: 0.0,
            curvature: 0.0,
            confidence: 1.0,
        }
    }

    #[test]
    fn build_draw_ops_for_image_uses_layout_addressing_with_gutter() {
        let layout = ImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE);
        let image_result = Image::new(layout, glaphica_core::BackendId::new(1));
        assert!(image_result.is_ok());
        let mut image = match image_result {
            Ok(image) => image,
            Err(_) => return,
        };
        assert!(
            image
                .set_tile_key(0, TileKey::from_parts(1, 1, 100))
                .is_ok()
        );
        assert!(
            image
                .set_tile_key(1, TileKey::from_parts(1, 1, 101))
                .is_ok()
        );

        let mut runtime = BrushEngineRuntime::new(4);
        assert!(
            runtime
                .register_pipeline(BrushId(2), 0, TestEnginePipeline)
                .is_ok()
        );

        let brush_input = build_test_brush_input(CanvasVec2::new(IMAGE_TILE_SIZE as f32, 10.0));
        let mut draw_ops = Vec::new();
        let build_result =
            runtime.build_draw_ops_for_image(BrushId(2), &brush_input, &image, &mut draw_ops);
        assert!(build_result.is_ok());
        assert_eq!(draw_ops.len(), 2);
        assert_eq!(draw_ops[0].tile_key, TileKey::from_parts(1, 1, 100));
        assert_eq!(draw_ops[0].ref_image, None);
        assert_eq!(draw_ops[1].tile_key, TileKey::from_parts(1, 1, 101));
        assert_eq!(draw_ops[1].ref_image, None);
    }

    #[test]
    fn build_draw_ops_for_image_resolves_ref_image_tile_key_by_same_tile_index() {
        let layout = ImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE);
        let mut write_image = match Image::new(layout, glaphica_core::BackendId::new(1)) {
            Ok(image) => image,
            Err(_) => return,
        };
        assert!(
            write_image
                .set_tile_key(0, TileKey::from_parts(1, 1, 100))
                .is_ok()
        );
        assert!(
            write_image
                .set_tile_key(1, TileKey::from_parts(1, 1, 101))
                .is_ok()
        );

        let mut read_image = match Image::new(layout, glaphica_core::BackendId::new(2)) {
            Ok(image) => image,
            Err(_) => return,
        };
        assert!(
            read_image
                .set_tile_key(0, TileKey::from_parts(2, 3, 200))
                .is_ok()
        );
        assert!(
            read_image
                .set_tile_key(1, TileKey::from_parts(2, 3, 201))
                .is_ok()
        );

        let mut runtime = BrushEngineRuntime::new(4);
        assert!(
            runtime
                .register_pipeline(BrushId(2), 0, TestEnginePipeline)
                .is_ok()
        );

        let brush_input = build_test_brush_input(CanvasVec2::new(IMAGE_TILE_SIZE as f32, 10.0));
        let mut draw_ops = Vec::new();
        let build_result = runtime.build_draw_ops_for_image_with_ref_image(
            BrushId(2),
            &brush_input,
            &write_image,
            Some(&read_image),
            &mut draw_ops,
        );
        assert!(build_result.is_ok());
        assert_eq!(draw_ops.len(), 2);
        assert_eq!(
            draw_ops[0].ref_image.map(|ref_image| ref_image.tile_key),
            Some(TileKey::from_parts(2, 3, 200))
        );
        assert_eq!(
            draw_ops[1].ref_image.map(|ref_image| ref_image.tile_key),
            Some(TileKey::from_parts(2, 3, 201))
        );
    }
}
