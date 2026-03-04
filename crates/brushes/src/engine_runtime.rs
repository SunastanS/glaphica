use std::collections::HashSet;
use std::error::Error;
use std::fmt::{Display, Formatter};

use glaphica_core::{BackendId, BrushId, BrushInput, CanvasVec2, NodeId, TileKey};
use images::Image;
use thread_protocol::{CopyOp, DrawOp, GpuCmdMsg, RefImage};

use crate::brush_registry::BrushRegistry;
use crate::{BrushPipelineError, BrushRegistryError};

pub trait TileSlotAllocator {
    fn alloc(&mut self, backend: BackendId) -> Option<TileKey>;

    fn alloc_with_parity(&mut self, backend: BackendId, _parity: bool) -> Option<TileKey> {
        self.alloc(backend)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StrokeTileKey {
    pub node_id: NodeId,
    pub tile_index: usize,
}

pub struct StrokeDrawOutput {
    pub draw_op: DrawOp,
    pub copy_op: Option<CopyOp>,
    pub tile_key_update: Option<(NodeId, usize, TileKey)>,
}

pub trait EngineBrushPipeline: Send {
    fn encode_draw_input(
        &mut self,
        brush_input: &BrushInput,
        tile_key: TileKey,
        tile_canvas_origin: CanvasVec2,
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
    tile_index: usize,
    tile_key: TileKey,
    ref_tile_key: Option<TileKey>,
}

pub struct BrushEngineRuntime {
    pipelines: BrushRegistry<EngineBrushRegistration>,
    scratch_affected_tiles: Vec<AffectedTile>,
    stroke_tiles: HashSet<StrokeTileKey>,
}

impl BrushEngineRuntime {
    pub fn new(max_brushes: usize) -> Self {
        Self {
            pipelines: BrushRegistry::with_max_brushes(max_brushes),
            scratch_affected_tiles: Vec::new(),
            stroke_tiles: HashSet::new(),
        }
    }

    pub fn begin_stroke(&mut self) {
        self.stroke_tiles.clear();
    }

    pub fn end_stroke(&mut self) {
        self.stroke_tiles.clear();
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
        node_id: NodeId,
        tile_index: usize,
        tile_key: TileKey,
    ) -> Result<DrawOp, EngineBrushDispatchError> {
        self.build_draw_op_with_ref_tile(brush_id, brush_input, node_id, tile_index, tile_key, None)
    }

    pub fn build_draw_op_with_ref_tile(
        &mut self,
        brush_id: BrushId,
        brush_input: &BrushInput,
        node_id: NodeId,
        tile_index: usize,
        tile_key: TileKey,
        ref_tile_key: Option<TileKey>,
    ) -> Result<DrawOp, EngineBrushDispatchError> {
        let registration = self.pipelines.get_mut(brush_id)?;
        let encoded_input = registration
            .pipeline
            .encode_draw_input(brush_input, tile_key, CanvasVec2::new(0.0, 0.0))
            .map_err(|source| EngineBrushDispatchError::Pipeline { brush_id, source })?;
        Ok(DrawOp {
            node_id,
            tile_index,
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
        node_id: NodeId,
        tile_index: usize,
        tile_key: TileKey,
    ) -> Result<GpuCmdMsg, EngineBrushDispatchError> {
        Ok(GpuCmdMsg::DrawOp(self.build_draw_op(
            brush_id,
            brush_input,
            node_id,
            tile_index,
            tile_key,
        )?))
    }

    pub fn build_draw_ops_for_image(
        &mut self,
        brush_id: BrushId,
        brush_input: &BrushInput,
        node_id: NodeId,
        image: &Image,
        output: &mut Vec<DrawOp>,
    ) -> Result<(), EngineBrushDispatchError> {
        self.dispatch_draw_ops_for_image(brush_id, brush_input, node_id, image, None, |draw_op| {
            output.push(draw_op)
        })
    }

    pub fn build_draw_ops_for_image_with_ref_image(
        &mut self,
        brush_id: BrushId,
        brush_input: &BrushInput,
        node_id: NodeId,
        image: &Image,
        ref_image: Option<&Image>,
        output: &mut Vec<DrawOp>,
    ) -> Result<(), EngineBrushDispatchError> {
        self.dispatch_draw_ops_for_image(
            brush_id,
            brush_input,
            node_id,
            image,
            ref_image,
            |draw_op| output.push(draw_op),
        )
    }

    pub fn build_draw_cmds_for_image(
        &mut self,
        brush_id: BrushId,
        brush_input: &BrushInput,
        node_id: NodeId,
        image: &Image,
        output: &mut Vec<GpuCmdMsg>,
    ) -> Result<(), EngineBrushDispatchError> {
        self.dispatch_draw_ops_for_image(brush_id, brush_input, node_id, image, None, |draw_op| {
            output.push(GpuCmdMsg::DrawOp(draw_op))
        })
    }

    pub fn build_draw_cmds_for_image_with_ref_image(
        &mut self,
        brush_id: BrushId,
        brush_input: &BrushInput,
        node_id: NodeId,
        image: &Image,
        ref_image: Option<&Image>,
        output: &mut Vec<GpuCmdMsg>,
    ) -> Result<(), EngineBrushDispatchError> {
        self.dispatch_draw_ops_for_image(
            brush_id,
            brush_input,
            node_id,
            image,
            ref_image,
            |draw_op| output.push(GpuCmdMsg::DrawOp(draw_op)),
        )
    }

    fn dispatch_draw_ops_for_image<F>(
        &mut self,
        brush_id: BrushId,
        brush_input: &BrushInput,
        node_id: NodeId,
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
                    tile_index,
                    tile_key,
                    ref_tile_key,
                });
            },
        );

        let registration = self.pipelines.get_mut(brush_id)?;
        for affected_tile in self.scratch_affected_tiles.iter().copied() {
            let tile_canvas_origin = image
                .tile_canvas_origin(affected_tile.tile_index)
                .unwrap_or(CanvasVec2::new(0.0, 0.0));
            let encoded_input = registration
                .pipeline
                .encode_draw_input(brush_input, affected_tile.tile_key, tile_canvas_origin)
                .map_err(|source| EngineBrushDispatchError::Pipeline { brush_id, source })?;
            emit(DrawOp {
                node_id,
                tile_index: affected_tile.tile_index,
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

    pub fn build_stroke_draw_outputs_for_image<A>(
        &mut self,
        brush_id: BrushId,
        brush_input: &BrushInput,
        node_id: NodeId,
        image: &mut Image,
        ref_image: Option<&Image>,
        allocator: &mut A,
        output: &mut Vec<StrokeDrawOutput>,
    ) -> Result<(), EngineBrushDispatchError>
    where
        A: TileSlotAllocator,
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
                    tile_index,
                    tile_key,
                    ref_tile_key,
                });
            },
        );

        let affected_tiles: Vec<AffectedTile> = self.scratch_affected_tiles.clone();

        let mut prepared_tiles: Vec<(
            usize,
            TileKey,
            Option<TileKey>,
            Option<CopyOp>,
            Option<(NodeId, usize, TileKey)>,
        )> = Vec::new();
        for affected_tile in affected_tiles {
            let stroke_key = StrokeTileKey {
                node_id,
                tile_index: affected_tile.tile_index,
            };

            let (final_tile_key, copy_op, tile_key_update) = self.prepare_tile_for_stroke(
                stroke_key,
                affected_tile.tile_key,
                affected_tile.tile_index,
                node_id,
                image,
                allocator,
            );

            prepared_tiles.push((
                affected_tile.tile_index,
                final_tile_key,
                affected_tile.ref_tile_key,
                copy_op,
                tile_key_update,
            ));
        }

        let registration = self.pipelines.get_mut(brush_id)?;
        for (tile_index, final_tile_key, ref_tile_key, copy_op, tile_key_update) in prepared_tiles {
            let tile_canvas_origin = image
                .tile_canvas_origin(tile_index)
                .unwrap_or(CanvasVec2::new(0.0, 0.0));
            let encoded_input = registration
                .pipeline
                .encode_draw_input(brush_input, final_tile_key, tile_canvas_origin)
                .map_err(|source| EngineBrushDispatchError::Pipeline { brush_id, source })?;

            output.push(StrokeDrawOutput {
                draw_op: DrawOp {
                    node_id,
                    tile_index,
                    tile_key: final_tile_key,
                    ref_image: ref_tile_key.map(|tile_key| RefImage { tile_key }),
                    input: encoded_input,
                    brush_id,
                },
                copy_op,
                tile_key_update,
            });
        }
        Ok(())
    }

    fn prepare_tile_for_stroke<A>(
        &mut self,
        stroke_key: StrokeTileKey,
        current_tile_key: TileKey,
        tile_index: usize,
        node_id: NodeId,
        image: &mut Image,
        allocator: &mut A,
    ) -> (TileKey, Option<CopyOp>, Option<(NodeId, usize, TileKey)>)
    where
        A: TileSlotAllocator,
    {
        if self.stroke_tiles.contains(&stroke_key) {
            return (current_tile_key, None, None);
        }

        let backend = image.backend();
        let next_tile = if current_tile_key == TileKey::EMPTY {
            allocator.alloc(backend)
        } else {
            allocator.alloc_with_parity(backend, !current_tile_key.slot_parity())
        };
        let Some(new_tile_key) = next_tile else {
            return (current_tile_key, None, None);
        };

        let copy_op = if current_tile_key == TileKey::EMPTY {
            None
        } else {
            Some(CopyOp {
                src_tile_key: current_tile_key,
                dst_tile_key: new_tile_key,
            })
        };

        if image.set_tile_key(tile_index, new_tile_key).is_err() {
            return (current_tile_key, None, None);
        }
        self.stroke_tiles.insert(stroke_key);

        (
            new_tile_key,
            copy_op,
            Some((node_id, tile_index, new_tile_key)),
        )
    }
}

#[cfg(test)]
mod tests {
    use glaphica_core::{
        BrushId, BrushInput, BrushInputFlags, CanvasVec2, MappedCursor, NodeId, RadianVec2,
        StrokeId, TileKey, IMAGE_TILE_SIZE,
    };
    use images::{layout::ImageLayout, Image};

    use super::{BrushEngineRuntime, EngineBrushPipeline, TileSlotAllocator};

    struct TestEnginePipeline;

    impl EngineBrushPipeline for TestEnginePipeline {
        fn encode_draw_input(
            &mut self,
            _brush_input: &BrushInput,
            tile_key: TileKey,
            _tile_canvas_origin: CanvasVec2,
        ) -> Result<Vec<f32>, crate::BrushPipelineError> {
            Ok(vec![tile_key.slot_index() as f32])
        }
    }

    #[derive(Default)]
    struct TestAllocator {
        regular_alloc: Option<TileKey>,
        odd_alloc: Option<TileKey>,
        even_alloc: Option<TileKey>,
        parity_requests: Vec<bool>,
    }

    impl TileSlotAllocator for TestAllocator {
        fn alloc(&mut self, _backend: glaphica_core::BackendId) -> Option<TileKey> {
            self.regular_alloc.take()
        }

        fn alloc_with_parity(
            &mut self,
            _backend: glaphica_core::BackendId,
            parity: bool,
        ) -> Option<TileKey> {
            self.parity_requests.push(parity);
            if parity {
                self.odd_alloc.take()
            } else {
                self.even_alloc.take()
            }
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
        assert!(image
            .set_tile_key(0, TileKey::from_parts(1, 1, 100))
            .is_ok());
        assert!(image
            .set_tile_key(1, TileKey::from_parts(1, 1, 101))
            .is_ok());

        let mut runtime = BrushEngineRuntime::new(4);
        assert!(runtime
            .register_pipeline(BrushId(2), 0, TestEnginePipeline)
            .is_ok());

        let brush_input = build_test_brush_input(CanvasVec2::new(IMAGE_TILE_SIZE as f32, 10.0));
        let mut draw_ops = Vec::new();
        let build_result = runtime.build_draw_ops_for_image(
            BrushId(2),
            &brush_input,
            NodeId(1),
            &image,
            &mut draw_ops,
        );
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
        assert!(write_image
            .set_tile_key(0, TileKey::from_parts(1, 1, 100))
            .is_ok());
        assert!(write_image
            .set_tile_key(1, TileKey::from_parts(1, 1, 101))
            .is_ok());

        let mut read_image = match Image::new(layout, glaphica_core::BackendId::new(2)) {
            Ok(image) => image,
            Err(_) => return,
        };
        assert!(read_image
            .set_tile_key(0, TileKey::from_parts(2, 3, 200))
            .is_ok());
        assert!(read_image
            .set_tile_key(1, TileKey::from_parts(2, 3, 201))
            .is_ok());

        let mut runtime = BrushEngineRuntime::new(4);
        assert!(runtime
            .register_pipeline(BrushId(2), 0, TestEnginePipeline)
            .is_ok());

        let brush_input = build_test_brush_input(CanvasVec2::new(IMAGE_TILE_SIZE as f32, 10.0));
        let mut draw_ops = Vec::new();
        let build_result = runtime.build_draw_ops_for_image_with_ref_image(
            BrushId(2),
            &brush_input,
            NodeId(1),
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

    #[test]
    fn stroke_prepare_allocates_opposite_parity_for_copy_op() {
        let layout = ImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE);
        let mut image = match Image::new(layout, glaphica_core::BackendId::new(1)) {
            Ok(image) => image,
            Err(_) => return,
        };
        let existing_key = TileKey::from_parts(1, 0, 0x0000_0007);
        assert!(image.set_tile_key(0, existing_key).is_ok());

        let mut runtime = BrushEngineRuntime::new(4);
        assert!(runtime
            .register_pipeline(BrushId(2), 0, TestEnginePipeline)
            .is_ok());
        runtime.begin_stroke();

        let mut allocator = TestAllocator {
            regular_alloc: None,
            odd_alloc: Some(TileKey::from_parts(1, 0, 0x8000_0001)),
            even_alloc: None,
            parity_requests: Vec::new(),
        };
        let brush_input = build_test_brush_input(CanvasVec2::new(10.0, 10.0));
        let mut output = Vec::new();
        let build_result = runtime.build_stroke_draw_outputs_for_image(
            BrushId(2),
            &brush_input,
            NodeId(9),
            &mut image,
            None,
            &mut allocator,
            &mut output,
        );
        assert!(build_result.is_ok());
        assert_eq!(allocator.parity_requests, vec![true]);
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].draw_op.tile_key, TileKey::from_parts(1, 0, 0x8000_0001));
        assert_eq!(
            output[0].copy_op,
            Some(thread_protocol::CopyOp {
                src_tile_key: existing_key,
                dst_tile_key: TileKey::from_parts(1, 0, 0x8000_0001),
            })
        );
    }
}
