use std::collections::HashMap;
use std::error::Error;
use std::fmt::{Display, Formatter};

use glaphica_core::{BackendId, BrushId, BrushInput, CanvasVec2, NodeId, TileKey};
use images::Image;
use thread_protocol::{
    ClearOp, CompositeOp, CopyOp, DrawBlendMode, DrawFrameMergePolicy, DrawOp, GpuCmdFrameMergeTag,
    GpuCmdMsg, RefImage, WriteBlendMode, WriteOp,
};

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
    pub clear_op: Option<ClearOp>,
    pub draw_op: Option<DrawOp>,
    pub copy_op: Option<CopyOp>,
    pub write_op: Option<WriteOp>,
    pub composite_op: Option<CompositeOp>,
    pub tile_key_update: Option<(NodeId, usize, TileKey)>,
}

pub trait EngineBrushPipeline: Send {
    fn encode_draw_input(
        &mut self,
        brush_input: &BrushInput,
        tile_key: TileKey,
        tile_canvas_origin: CanvasVec2,
    ) -> Result<Vec<f32>, BrushPipelineError>;

    fn uses_stroke_buffer(&self) -> bool {
        false
    }

    fn encode_stroke_buffer_dab_input(
        &mut self,
        brush_input: &BrushInput,
        tile_key: TileKey,
        tile_canvas_origin: CanvasVec2,
    ) -> Result<Vec<f32>, BrushPipelineError> {
        self.encode_draw_input(brush_input, tile_key, tile_canvas_origin)
    }

    fn encode_stroke_buffer_composite_input(
        &mut self,
        brush_input: &BrushInput,
        tile_key: TileKey,
        tile_canvas_origin: CanvasVec2,
    ) -> Result<Vec<f32>, BrushPipelineError> {
        self.encode_draw_input(brush_input, tile_key, tile_canvas_origin)
    }

    fn stroke_buffer_write_opacity(
        &mut self,
        _brush_input: &BrushInput,
    ) -> Result<f32, BrushPipelineError> {
        Ok(1.0)
    }

    fn stroke_buffer_copy_frame_merge_tag(&self) -> GpuCmdFrameMergeTag {
        GpuCmdFrameMergeTag::None
    }

    fn stroke_buffer_write_frame_merge_tag(&self) -> GpuCmdFrameMergeTag {
        GpuCmdFrameMergeTag::None
    }
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
    stroke_buffer_backend: Option<BackendId>,
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
    stroke_tiles: HashMap<StrokeTileKey, TileKey>,
    stroke_restore_tiles: HashMap<StrokeTileKey, TileKey>,
    stroke_buffer_tiles: HashMap<StrokeTileKey, TileKey>,
}

impl BrushEngineRuntime {
    pub fn new(max_brushes: usize) -> Self {
        Self {
            pipelines: BrushRegistry::with_max_brushes(max_brushes),
            scratch_affected_tiles: Vec::new(),
            stroke_tiles: HashMap::new(),
            stroke_restore_tiles: HashMap::new(),
            stroke_buffer_tiles: HashMap::new(),
        }
    }

    pub fn begin_stroke(&mut self) {
        self.stroke_tiles.clear();
        self.stroke_restore_tiles.clear();
        self.stroke_buffer_tiles.clear();
    }

    pub fn end_stroke(&mut self) {
        self.stroke_tiles.clear();
        self.stroke_restore_tiles.clear();
        self.stroke_buffer_tiles.clear();
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
        self.register_pipeline_with_stroke_buffer_backend(
            brush_id,
            max_affected_radius_px,
            None,
            pipeline,
        )
    }

    pub fn register_pipeline_with_stroke_buffer_backend<P>(
        &mut self,
        brush_id: BrushId,
        max_affected_radius_px: u32,
        stroke_buffer_backend: Option<BackendId>,
        pipeline: P,
    ) -> Result<(), BrushRegistryError>
    where
        P: EngineBrushPipeline + 'static,
    {
        let stroke_buffer_backend = if pipeline.uses_stroke_buffer() {
            stroke_buffer_backend
        } else {
            None
        };
        self.pipelines.register(
            brush_id,
            EngineBrushRegistration {
                max_affected_radius_px,
                stroke_buffer_backend,
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

    pub fn update_pipeline<P>(
        &mut self,
        brush_id: BrushId,
        max_affected_radius_px: u32,
        pipeline: P,
    ) -> Result<(), BrushRegistryError>
    where
        P: EngineBrushPipeline + 'static,
    {
        let registration = self.pipelines.get_mut(brush_id)?;
        registration.max_affected_radius_px = max_affected_radius_px;
        registration.pipeline = Box::new(pipeline);
        Ok(())
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
            blend_mode: DrawBlendMode::Alpha,
            frame_merge: DrawFrameMergePolicy::None,
            origin_tile: TileKey::EMPTY,
            ref_image: ref_tile_key.map(|tile_key| RefImage { tile_key }),
            input: encoded_input,
            brush_id,
            stroke_id: brush_input.stroke,
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
                blend_mode: DrawBlendMode::Alpha,
                frame_merge: DrawFrameMergePolicy::None,
                origin_tile: TileKey::EMPTY,
                ref_image: affected_tile
                    .ref_tile_key
                    .map(|tile_key| RefImage { tile_key }),
                input: encoded_input,
                brush_id,
                stroke_id: brush_input.stroke,
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
            TileKey,
            Option<TileKey>,
            Option<CopyOp>,
            Option<ClearOp>,
            Option<ClearOp>,
            Option<TileKey>,
            Option<(NodeId, usize, TileKey)>,
        )> = Vec::new();
        let stroke_buffer_backend = self.pipelines.get(brush_id)?.stroke_buffer_backend;
        let uses_stroke_buffer = stroke_buffer_backend.is_some();
        let (copy_frame_merge_tag, write_frame_merge_tag) = if uses_stroke_buffer {
            let registration = self.pipelines.get_mut(brush_id)?;
            (
                registration.pipeline.stroke_buffer_copy_frame_merge_tag(),
                registration.pipeline.stroke_buffer_write_frame_merge_tag(),
            )
        } else {
            (GpuCmdFrameMergeTag::None, GpuCmdFrameMergeTag::None)
        };

        for affected_tile in affected_tiles {
            let stroke_key = StrokeTileKey {
                node_id,
                tile_index: affected_tile.tile_index,
            };

            let (final_tile_key, origin_tile, copy_op, origin_init_clear_op, tile_key_update) =
                self.prepare_tile_for_stroke(
                    stroke_key,
                    affected_tile.tile_key,
                    affected_tile.tile_index,
                    node_id,
                    image,
                    allocator,
                );

            let (buffer_tile_key, clear_op) = if let Some(buffer_backend) = stroke_buffer_backend {
                let (buffer_tile_key, clear_op) =
                    self.prepare_stroke_buffer_tile(stroke_key, buffer_backend, allocator);
                (Some(buffer_tile_key), clear_op)
            } else {
                (None, None)
            };

            prepared_tiles.push((
                affected_tile.tile_index,
                final_tile_key,
                origin_tile,
                affected_tile.ref_tile_key,
                copy_op,
                origin_init_clear_op,
                clear_op,
                buffer_tile_key,
                tile_key_update,
            ));
        }

        for (
            tile_index,
            final_tile_key,
            origin_tile,
            ref_tile_key,
            copy_op,
            origin_init_clear_op,
            clear_op,
            buffer_tile_key,
            tile_key_update,
        ) in prepared_tiles
        {
            let tile_canvas_origin = image
                .tile_canvas_origin(tile_index)
                .unwrap_or(CanvasVec2::new(0.0, 0.0));
            if uses_stroke_buffer {
                let Some(buffer_tile_key) = buffer_tile_key else {
                    continue;
                };
                if let Some(clear_op) = origin_init_clear_op {
                    output.push(StrokeDrawOutput {
                        clear_op: Some(clear_op),
                        draw_op: None,
                        copy_op: None,
                        write_op: None,
                        composite_op: None,
                        tile_key_update: None,
                    });
                }
                if buffer_tile_key == TileKey::EMPTY {
                    let encoded_input = self
                        .pipelines
                        .get_mut(brush_id)?
                        .pipeline
                        .encode_draw_input(brush_input, final_tile_key, tile_canvas_origin)
                        .map_err(|source| EngineBrushDispatchError::Pipeline {
                            brush_id,
                            source,
                        })?;
                    output.push(StrokeDrawOutput {
                        clear_op: None,
                        draw_op: Some(DrawOp {
                            node_id,
                            tile_index,
                            tile_key: final_tile_key,
                            blend_mode: DrawBlendMode::Alpha,
                            frame_merge: DrawFrameMergePolicy::None,
                            origin_tile,
                            ref_image: ref_tile_key.map(|tile_key| RefImage { tile_key }),
                            input: encoded_input,
                            brush_id,
                            stroke_id: brush_input.stroke,
                        }),
                        copy_op,
                        write_op: None,
                        composite_op: None,
                        tile_key_update,
                    });
                    continue;
                }
                let encoded_dab_input = self
                    .pipelines
                    .get_mut(brush_id)?
                    .pipeline
                    .encode_stroke_buffer_dab_input(
                        brush_input,
                        buffer_tile_key,
                        tile_canvas_origin,
                    )
                    .map_err(|source| EngineBrushDispatchError::Pipeline { brush_id, source })?;

                let copy_op = copy_op.map(|copy_op| CopyOp {
                    src_tile_key: copy_op.src_tile_key,
                    dst_tile_key: copy_op.dst_tile_key,
                    frame_merge: copy_frame_merge_tag,
                });

                output.push(StrokeDrawOutput {
                    clear_op,
                    draw_op: Some(DrawOp {
                        node_id,
                        tile_index,
                        tile_key: buffer_tile_key,
                        blend_mode: DrawBlendMode::Alpha,
                        frame_merge: DrawFrameMergePolicy::None,
                        origin_tile: TileKey::EMPTY,
                        ref_image: None,
                        input: encoded_dab_input,
                        brush_id,
                        stroke_id: brush_input.stroke,
                    }),
                    copy_op,
                    write_op: None,
                    composite_op: None,
                    tile_key_update,
                });

                let write_opacity = self
                    .pipelines
                    .get_mut(brush_id)?
                    .pipeline
                    .stroke_buffer_write_opacity(brush_input)
                    .map_err(|source| EngineBrushDispatchError::Pipeline { brush_id, source })?;
                let write_dst_tile_key = copy_op
                    .map(|copy_op| copy_op.dst_tile_key)
                    .unwrap_or(final_tile_key);
                output.push(StrokeDrawOutput {
                    clear_op: None,
                    draw_op: None,
                    copy_op: None,
                    write_op: Some(WriteOp {
                        src_tile_key: buffer_tile_key,
                        dst_tile_key: write_dst_tile_key,
                        blend_mode: WriteBlendMode::Normal,
                        opacity: write_opacity,
                        frame_merge: write_frame_merge_tag,
                    }),
                    composite_op: None,
                    tile_key_update: None,
                });
            } else {
                let encoded_input = self
                    .pipelines
                    .get_mut(brush_id)?
                    .pipeline
                    .encode_draw_input(brush_input, final_tile_key, tile_canvas_origin)
                    .map_err(|source| EngineBrushDispatchError::Pipeline { brush_id, source })?;

                if let Some(clear_op) = origin_init_clear_op {
                    output.push(StrokeDrawOutput {
                        clear_op: Some(clear_op),
                        draw_op: None,
                        copy_op: None,
                        write_op: None,
                        composite_op: None,
                        tile_key_update: None,
                    });
                }
                output.push(StrokeDrawOutput {
                    clear_op: None,
                    draw_op: Some(DrawOp {
                        node_id,
                        tile_index,
                        tile_key: final_tile_key,
                        blend_mode: DrawBlendMode::Alpha,
                        frame_merge: DrawFrameMergePolicy::None,
                        origin_tile,
                        ref_image: ref_tile_key.map(|tile_key| RefImage { tile_key }),
                        input: encoded_input,
                        brush_id,
                        stroke_id: brush_input.stroke,
                    }),
                    copy_op,
                    write_op: None,
                    composite_op: None,
                    tile_key_update,
                });
            }
        }
        Ok(())
    }

    fn prepare_stroke_buffer_tile<A>(
        &mut self,
        stroke_key: StrokeTileKey,
        buffer_backend: BackendId,
        allocator: &mut A,
    ) -> (TileKey, Option<ClearOp>)
    where
        A: TileSlotAllocator,
    {
        if let Some(tile_key) = self.stroke_buffer_tiles.get(&stroke_key).copied() {
            return (tile_key, None);
        }
        let Some(tile_key) = allocator.alloc(buffer_backend) else {
            return (TileKey::EMPTY, None);
        };
        self.stroke_buffer_tiles.insert(stroke_key, tile_key);
        (tile_key, Some(ClearOp { tile_key }))
    }

    fn prepare_tile_for_stroke<A>(
        &mut self,
        stroke_key: StrokeTileKey,
        current_tile_key: TileKey,
        tile_index: usize,
        node_id: NodeId,
        image: &mut Image,
        allocator: &mut A,
    ) -> (
        TileKey,
        TileKey,
        Option<CopyOp>,
        Option<ClearOp>,
        Option<(NodeId, usize, TileKey)>,
    )
    where
        A: TileSlotAllocator,
    {
        if let Some(origin_tile) = self.stroke_tiles.get(&stroke_key).copied() {
            let restore_tile = self
                .stroke_restore_tiles
                .get(&stroke_key)
                .copied()
                .unwrap_or(origin_tile);
            // Restore must stay in Copy(A->B) semantics. Clearing B would not preserve
            // the "replace from original snapshot" contract for this stroke.
            return (
                current_tile_key,
                origin_tile,
                if restore_tile == TileKey::EMPTY || current_tile_key == TileKey::EMPTY {
                    None
                } else {
                    Some(CopyOp {
                        src_tile_key: restore_tile,
                        dst_tile_key: current_tile_key,
                        frame_merge: GpuCmdFrameMergeTag::None,
                    })
                },
                None,
                None,
            );
        }
        let origin_tile = current_tile_key;

        let backend = image.backend();
        let next_tile = if current_tile_key == TileKey::EMPTY {
            allocator.alloc(backend)
        } else {
            allocator.alloc_with_parity(backend, !current_tile_key.slot_parity())
        };
        let Some(new_tile_key) = next_tile else {
            return (current_tile_key, origin_tile, None, None, None);
        };

        let mut origin_init_clear_op = None;
        let restore_tile = if current_tile_key == TileKey::EMPTY {
            let snapshot = allocator
                .alloc_with_parity(backend, !new_tile_key.slot_parity())
                .or_else(|| allocator.alloc(backend));
            if let Some(snapshot_tile) = snapshot {
                origin_init_clear_op = Some(ClearOp {
                    tile_key: snapshot_tile,
                });
                snapshot_tile
            } else {
                TileKey::EMPTY
            }
        } else {
            current_tile_key
        };

        let copy_op = if current_tile_key == TileKey::EMPTY {
            None
        } else {
            Some(CopyOp {
                src_tile_key: current_tile_key,
                dst_tile_key: new_tile_key,
                frame_merge: GpuCmdFrameMergeTag::None,
            })
        };

        if image.set_tile_key(tile_index, new_tile_key).is_err() {
            return (current_tile_key, origin_tile, None, None, None);
        }
        self.stroke_tiles.insert(stroke_key, origin_tile);
        self.stroke_restore_tiles.insert(stroke_key, restore_tile);

        (
            new_tile_key,
            origin_tile,
            copy_op,
            origin_init_clear_op,
            Some((node_id, tile_index, new_tile_key)),
        )
    }
}

#[cfg(test)]
mod tests {
    use glaphica_core::{
        BrushId, BrushInput, BrushInputFlags, CanvasVec2, IMAGE_TILE_SIZE, MappedCursor, NodeId,
        RadianVec2, StrokeId, TileKey,
    };
    use images::{Image, layout::ImageLayout};

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
        assert_eq!(draw_ops[0].origin_tile, TileKey::EMPTY);
        assert_eq!(draw_ops[0].ref_image, None);
        assert_eq!(draw_ops[1].tile_key, TileKey::from_parts(1, 1, 101));
        assert_eq!(draw_ops[1].origin_tile, TileKey::EMPTY);
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
        assert_eq!(draw_ops[0].origin_tile, TileKey::EMPTY);
        assert_eq!(
            draw_ops[1].ref_image.map(|ref_image| ref_image.tile_key),
            Some(TileKey::from_parts(2, 3, 201))
        );
        assert_eq!(draw_ops[1].origin_tile, TileKey::EMPTY);
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
        assert!(
            runtime
                .register_pipeline(BrushId(2), 0, TestEnginePipeline)
                .is_ok()
        );
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
        let draw_op = output[0].draw_op.as_ref().expect("draw op must exist");
        assert_eq!(draw_op.tile_key, TileKey::from_parts(1, 0, 0x8000_0001));
        assert_eq!(draw_op.origin_tile, existing_key);
        assert_eq!(
            output[0].copy_op,
            Some(thread_protocol::CopyOp {
                src_tile_key: existing_key,
                dst_tile_key: TileKey::from_parts(1, 0, 0x8000_0001),
                frame_merge: thread_protocol::GpuCmdFrameMergeTag::None,
            })
        );
        assert_eq!(output[0].write_op, None);
    }
}
