use atlas::{BackendManager, EditSession, TileKeySwap};
use brushes::{BrushEngineRuntime, BrushResamplerDistance, StrokeDrawOutput, TileSlotAllocator};
use document::{Document, FlatRenderTree, SharedRenderTree};
use glaphica_core::{
    BackendId, BrushId, BrushInput, NodeId, RenderTreeGeneration, StrokeId, TileKey,
};
use images::Image;
use std::{collections::HashMap, sync::Arc};
use stroke_input::{InputProcessingConfig, StrokeInputProcessor};

pub struct EngineBackendManager {
    manager: BackendManager,
    stroke_edits: HashMap<u8, EditSession>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StrokeTileUndoRecord {
    node_id: NodeId,
    tile_index: usize,
    old_tile_key: TileKey,
    new_tile_key: TileKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StrokeUndoRecord {
    tiles: Vec<StrokeTileUndoRecord>,
}

impl EngineBackendManager {
    pub fn new() -> Self {
        Self {
            manager: BackendManager::new(),
            stroke_edits: HashMap::new(),
        }
    }

    pub fn inner(&self) -> &BackendManager {
        &self.manager
    }

    pub fn inner_mut(&mut self) -> &mut BackendManager {
        &mut self.manager
    }

    fn retire_tiles<I>(&mut self, keys: I)
    where
        I: IntoIterator<Item = TileKey>,
    {
        self.cache_tiles(keys, false);
    }

    fn drop_tiles<I>(&mut self, keys: I)
    where
        I: IntoIterator<Item = TileKey>,
    {
        self.cache_tiles(keys, true);
    }

    fn cache_tiles<I>(&mut self, keys: I, drop_first: bool)
    where
        I: IntoIterator<Item = TileKey>,
    {
        let mut sessions: HashMap<u8, EditSession> = HashMap::new();
        for key in keys {
            if key == TileKey::EMPTY {
                continue;
            }
            let backend = key.backend();
            let Some(backend_ref) = self.manager.backend_mut(backend) else {
                continue;
            };
            let session = sessions
                .entry(backend.raw())
                .or_insert_with(|| backend_ref.begin_edit());
            let result = if drop_first {
                backend_ref.drop_key(session, key)
            } else {
                backend_ref.release_key(session, key)
            };
            if let Err(error) = result {
                eprintln!("failed to cache tile key: {error:?}");
            }
        }

        for (backend, session) in sessions {
            if let Some(backend_ref) = self.manager.backend_mut(BackendId::new(backend)) {
                let result = if drop_first {
                    backend_ref.finish_drop(session)
                } else {
                    backend_ref.finish_edit(session)
                };
                if let Err(error) = result {
                    eprintln!("failed to finish tile cache session: {error:?}");
                }
            }
        }
    }

    fn ensure_stroke_edit_session(&mut self, backend: BackendId) -> Option<&mut EditSession> {
        if !self.stroke_edits.contains_key(&backend.raw()) {
            let session = self.manager.backend(backend)?.begin_edit();
            self.stroke_edits.insert(backend.raw(), session);
        }
        self.stroke_edits.get_mut(&backend.raw())
    }
}

impl TileSlotAllocator for EngineBackendManager {
    fn alloc_active(&mut self, backend: BackendId) -> Option<TileKey> {
        self.manager
            .backend_mut(backend)
            .and_then(|b| b.alloc_active().ok().map(|tile| tile.key()))
    }

    fn alloc_active_with_parity(&mut self, backend: BackendId, parity: bool) -> Option<TileKey> {
        self.manager.backend_mut(backend).and_then(|b| {
            b.alloc_active_with_parity(parity)
                .ok()
                .map(|tile| tile.key())
        })
    }

    fn begin_stroke(&mut self) {
        self.stroke_edits.clear();
    }

    fn end_stroke(&mut self) {
        let stroke_edits = std::mem::take(&mut self.stroke_edits);
        for (backend, session) in stroke_edits {
            if let Some(backend) = self.manager.backend_mut(BackendId::new(backend)) {
                if let Err(error) = backend.finish_edit(session) {
                    eprintln!("failed to finish backend edit session: {error:?}");
                }
            }
        }
    }

    fn replace(&mut self, old: TileKey, new: TileKey) {
        let backend = old.backend();
        let backend_key = backend.raw();
        if self.ensure_stroke_edit_session(backend).is_none() {
            return;
        }
        let Some(mut session) = self.stroke_edits.remove(&backend_key) else {
            return;
        };
        let Some(backend) = self.manager.backend_mut(backend) else {
            self.stroke_edits.insert(backend_key, session);
            return;
        };
        if let Err(error) = backend.replace_key(&mut session, old, new) {
            eprintln!("failed to register tile replacement: {error:?}");
        }
        self.stroke_edits.insert(backend_key, session);
    }

    fn release(&mut self, tile: TileKey) {
        let backend = tile.backend();
        let backend_key = backend.raw();
        if self.ensure_stroke_edit_session(backend).is_none() {
            return;
        }
        let Some(mut session) = self.stroke_edits.remove(&backend_key) else {
            return;
        };
        let Some(backend) = self.manager.backend_mut(backend) else {
            self.stroke_edits.insert(backend_key, session);
            return;
        };
        if let Err(error) = backend.release_key(&mut session, tile) {
            eprintln!("failed to register tile release: {error:?}");
        }
        self.stroke_edits.insert(backend_key, session);
    }
}

pub struct EngineThreadState {
    document: Document,
    shared_tree: Arc<SharedRenderTree>,
    backend_manager: EngineBackendManager,
    brush_runtime: BrushEngineRuntime,
    stroke_outputs: Vec<StrokeDrawOutput>,
    input_processor: StrokeInputProcessor,
    active_stroke_id: Option<StrokeId>,
    pending_stroke_undo_tiles: Vec<StrokeTileUndoRecord>,
    undo_strokes: Vec<StrokeUndoRecord>,
    redo_strokes: Vec<StrokeUndoRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineStats {
    pub backend_tiles: Vec<atlas::BackendTileStats>,
    pub undo_stroke_count: usize,
}

const RESAMPLER_MIN_TIME_S: f32 = 0.008;
const RESAMPLER_MAX_TIME_S: f32 = 0.05;

impl EngineThreadState {
    pub fn new(document: Document, shared_tree: Arc<SharedRenderTree>, max_brushes: usize) -> Self {
        let input_processor = StrokeInputProcessor::new(InputProcessingConfig {
            smoothing: stroke_input::ExponentialMovingAverageConfig {
                position_alpha: 0.3,
                pressure_alpha: 0.3,
                tilt_alpha: 0.3,
                twist_alpha: 0.3,
            },
            resampling: stroke_input::ResamplerConfig {
                min_distance: 2.0,
                max_distance: 10.0,
                min_time_s: RESAMPLER_MIN_TIME_S,
                max_time_s: RESAMPLER_MAX_TIME_S,
            },
            velocity_window_size: 4,
            curvature_window_size: 4,
        });

        Self {
            document,
            shared_tree,
            backend_manager: EngineBackendManager::new(),
            brush_runtime: BrushEngineRuntime::new(max_brushes),
            stroke_outputs: Vec::new(),
            input_processor,
            active_stroke_id: None,
            pending_stroke_undo_tiles: Vec::new(),
            undo_strokes: Vec::new(),
            redo_strokes: Vec::new(),
        }
    }

    pub fn backend_manager(&self) -> &BackendManager {
        self.backend_manager.inner()
    }

    pub fn backend_manager_mut(&mut self) -> &mut BackendManager {
        self.backend_manager.inner_mut()
    }

    pub fn brush_runtime(&self) -> &BrushEngineRuntime {
        &self.brush_runtime
    }

    pub fn brush_runtime_mut(&mut self) -> &mut BrushEngineRuntime {
        &mut self.brush_runtime
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn document_mut(&mut self) -> &mut Document {
        &mut self.document
    }

    pub fn allocate_leaf_tile(&mut self, backend: BackendId) -> Option<TileKey> {
        self.backend_manager.alloc_active(backend)
    }

    pub fn replace_document(&mut self, document: Document) {
        let old_keys = self.document.collect_raster_tile_keys();
        self.backend_manager.drop_tiles(old_keys);
        self.document = document;
        self.pending_stroke_undo_tiles.clear();
        self.undo_strokes.clear();
        self.redo_strokes.clear();
        self.active_stroke_id = None;
        self.input_processor.end_stroke();
    }

    pub fn stats(&self) -> EngineStats {
        EngineStats {
            backend_tiles: self.backend_manager.inner().backend_tile_stats(),
            undo_stroke_count: self.undo_strokes.len(),
        }
    }

    pub fn shared_tree(&self) -> &SharedRenderTree {
        &self.shared_tree
    }

    pub fn begin_stroke(&mut self, stroke_id: StrokeId) {
        self.active_stroke_id = Some(stroke_id);
        self.pending_stroke_undo_tiles.clear();
        self.input_processor.begin_stroke(stroke_id);
        self.brush_runtime.begin_stroke(&mut self.backend_manager);
    }

    pub fn end_stroke(&mut self) {
        self.input_processor.end_stroke();
        self.brush_runtime.end_stroke(&mut self.backend_manager);
        if !self.pending_stroke_undo_tiles.is_empty() {
            self.undo_strokes.push(StrokeUndoRecord {
                tiles: std::mem::take(&mut self.pending_stroke_undo_tiles),
            });
        }
        self.active_stroke_id = None;
    }

    pub fn undo_stroke(&mut self) -> Option<thread_protocol::TileSlotKeyUpdateMsg> {
        let record = self.undo_strokes.pop()?;
        self.apply_stroke_undo_record(&record)?;
        self.redo_strokes.push(record.clone());
        Some(Self::tile_update_msg_from_record(&record, true))
    }

    pub fn redo_stroke(&mut self) -> Option<thread_protocol::TileSlotKeyUpdateMsg> {
        let record = self.redo_strokes.pop()?;
        self.apply_stroke_redo_record(&record)?;
        self.undo_strokes.push(record.clone());
        Some(Self::tile_update_msg_from_record(&record, false))
    }

    pub fn invalidate_redo_strokes(&mut self) {
        let mut keys = self
            .redo_strokes
            .iter()
            .flat_map(|record| record.tiles.iter().map(|tile| tile.new_tile_key))
            .filter(|key| *key != TileKey::EMPTY)
            .collect::<Vec<_>>();
        keys.sort_unstable_by_key(|key| {
            (
                key.backend_index(),
                key.generation_index(),
                key.slot_index(),
            )
        });
        keys.dedup();
        self.backend_manager.drop_tiles(keys);
        self.redo_strokes.clear();
    }

    pub fn process_raw_input(
        &mut self,
        cursor: glaphica_core::MappedCursor,
        timestamp_ns: u64,
    ) -> Vec<BrushInput> {
        match self.active_stroke_id {
            Some(stroke_id) => self
                .input_processor
                .process_input(stroke_id, cursor, timestamp_ns),
            None => Vec::new(),
        }
    }

    pub fn input_processor(&self) -> &StrokeInputProcessor {
        &self.input_processor
    }

    pub fn input_processor_mut(&mut self) -> &mut StrokeInputProcessor {
        &mut self.input_processor
    }

    pub fn set_resampler_distance(&mut self, distance: BrushResamplerDistance) {
        self.input_processor
            .set_resampling_config(stroke_input::ResamplerConfig {
                min_distance: distance.min_distance,
                max_distance: distance.max_distance,
                min_time_s: RESAMPLER_MIN_TIME_S,
                max_time_s: RESAMPLER_MAX_TIME_S,
            });
    }

    pub fn process_stroke_input(
        &mut self,
        brush_id: BrushId,
        brush_input: &BrushInput,
        rgb: [f32; 3],
        erase: bool,
        node_id: NodeId,
        ref_image: Option<&Image>,
    ) -> Result<Vec<thread_protocol::GpuCmdMsg>, brushes::EngineBrushDispatchError> {
        self.stroke_outputs.clear();

        let image = self.document.get_leaf_image_mut(node_id);
        let image = match image {
            Some(img) => img,
            None => {
                return Ok(Vec::new());
            }
        };

        self.brush_runtime.build_stroke_draw_outputs_for_image(
            brush_id,
            brush_input,
            rgb,
            erase,
            node_id,
            image,
            ref_image,
            &mut self.backend_manager,
            &mut self.stroke_outputs,
        )?;

        let mut clear_ops = Vec::new();
        let mut copy_ops = Vec::new();
        let mut draw_ops = Vec::new();
        let mut composite_ops = Vec::new();
        let mut write_ops = Vec::new();
        let mut tile_updates: Vec<(NodeId, usize)> = Vec::new();

        for output in &self.stroke_outputs {
            if let Some(clear_op) = output.clear_op {
                clear_ops.push(clear_op);
            }

            if let Some(copy_op) = output.copy_op {
                copy_ops.push(copy_op);
            }

            if let Some(write_op) = output.write_op {
                write_ops.push(write_op);
            }

            if let Some(composite_op) = output.composite_op {
                composite_ops.push(composite_op);
            }

            if let Some(draw_op) = &output.draw_op {
                draw_ops.push(draw_op.clone());
            }

            if let Some(tile_update) = output.tile_key_update {
                tile_updates.push((tile_update.node_id, tile_update.tile_index));
                self.pending_stroke_undo_tiles.push(StrokeTileUndoRecord {
                    node_id: tile_update.node_id,
                    tile_index: tile_update.tile_index,
                    old_tile_key: tile_update.old_tile_key,
                    new_tile_key: tile_update.new_tile_key,
                });
            }
        }

        let mut gpu_cmds = Vec::with_capacity(
            clear_ops.len()
                + copy_ops.len()
                + draw_ops.len()
                + composite_ops.len()
                + write_ops.len()
                + 1,
        );
        for clear_op in clear_ops {
            gpu_cmds.push(thread_protocol::GpuCmdMsg::ClearOp(clear_op));
        }
        for copy_op in copy_ops {
            gpu_cmds.push(thread_protocol::GpuCmdMsg::CopyOp(copy_op));
        }
        for draw_op in draw_ops {
            gpu_cmds.push(thread_protocol::GpuCmdMsg::DrawOp(draw_op));
        }
        for composite_op in composite_ops {
            gpu_cmds.push(thread_protocol::GpuCmdMsg::CompositeOp(composite_op));
        }
        for write_op in write_ops {
            gpu_cmds.push(thread_protocol::GpuCmdMsg::WriteOp(write_op));
        }

        if !tile_updates.is_empty() {
            let tile_keys: Vec<_> = tile_updates
                .iter()
                .filter_map(|(node_id, tile_index)| {
                    let image = self.document.get_leaf_image(*node_id)?;
                    let tile_key = image.tile_key(*tile_index)?;
                    Some((*node_id, *tile_index, tile_key))
                })
                .collect();

            gpu_cmds.push(thread_protocol::GpuCmdMsg::TileSlotKeyUpdate(
                thread_protocol::TileSlotKeyUpdateMsg { updates: tile_keys },
            ));
        }
        Ok(gpu_cmds)
    }

    fn apply_stroke_undo_record(&mut self, record: &StrokeUndoRecord) -> Option<()> {
        let mut swaps_by_backend: HashMap<u8, Vec<TileKeySwap>> = HashMap::new();
        for tile in &record.tiles {
            let backend = if tile.old_tile_key != TileKey::EMPTY {
                tile.old_tile_key.backend()
            } else if tile.new_tile_key != TileKey::EMPTY {
                tile.new_tile_key.backend()
            } else {
                continue;
            };
            swaps_by_backend
                .entry(backend.raw())
                .or_default()
                .push(TileKeySwap {
                    restore_key: tile.old_tile_key,
                    retire_key: tile.new_tile_key,
                });
        }

        for (backend, swaps) in &swaps_by_backend {
            let backend = self
                .backend_manager
                .inner_mut()
                .backend_mut(BackendId::new(*backend))?;
            if backend.restore_cached_keys(swaps).is_err() {
                return None;
            }
        }

        for tile in &record.tiles {
            let image = self.document.get_leaf_image_mut(tile.node_id)?;
            if image
                .set_tile_key(tile.tile_index, tile.old_tile_key)
                .is_err()
            {
                return None;
            }
        }
        Some(())
    }

    fn apply_stroke_redo_record(&mut self, record: &StrokeUndoRecord) -> Option<()> {
        let keys = record
            .tiles
            .iter()
            .map(|tile| tile.old_tile_key)
            .filter(|key| *key != TileKey::EMPTY)
            .collect::<Vec<_>>();
        self.backend_manager.retire_tiles(keys);

        for tile in &record.tiles {
            let image = self.document.get_leaf_image_mut(tile.node_id)?;
            if image
                .set_tile_key(tile.tile_index, tile.new_tile_key)
                .is_err()
            {
                return None;
            }
        }
        Some(())
    }

    fn tile_update_msg_from_record(
        record: &StrokeUndoRecord,
        use_old_tile_key: bool,
    ) -> thread_protocol::TileSlotKeyUpdateMsg {
        thread_protocol::TileSlotKeyUpdateMsg {
            updates: record
                .tiles
                .iter()
                .map(|tile| {
                    (
                        tile.node_id,
                        tile.tile_index,
                        if use_old_tile_key {
                            tile.old_tile_key
                        } else {
                            tile.new_tile_key
                        },
                    )
                })
                .collect(),
        }
    }

    pub fn rebuild_render_tree(
        &mut self,
    ) -> Result<thread_protocol::RenderTreeUpdatedMsg, document::ImageCreateError> {
        let generation = self.shared_tree.generation();
        let new_generation = RenderTreeGeneration(generation.0 + 1);

        let old_tree = self.shared_tree.read();
        let mut new_tree = self.document.build_flat_render_tree(new_generation)?;
        new_tree.carry_forward_render_caches(&old_tree);
        allocate_missing_render_cache_tiles(&mut new_tree, &mut self.backend_manager);
        retire_stale_render_cache_tiles(&old_tree, &new_tree, &mut self.backend_manager);

        let dirty_render_caches = new_tree.diff_render_cache_dirty(&old_tree);

        self.shared_tree.update(new_tree);

        Ok(thread_protocol::RenderTreeUpdatedMsg {
            generation: new_generation,
            dirty_render_caches,
        })
    }
}

fn retire_stale_render_cache_tiles(
    old_tree: &FlatRenderTree,
    new_tree: &FlatRenderTree,
    backend_manager: &mut EngineBackendManager,
) {
    let old_keys = collect_render_cache_tile_keys(old_tree);
    let new_keys = collect_render_cache_tile_keys(new_tree);
    backend_manager.retire_tiles(old_keys.difference(&new_keys).copied());
}

fn collect_render_cache_tile_keys(tree: &FlatRenderTree) -> std::collections::HashSet<TileKey> {
    let mut keys = std::collections::HashSet::new();
    for node in tree.nodes.values() {
        let Some(render_cache) = node.kind.render_cache() else {
            continue;
        };
        for tile_index in 0..render_cache.tile_count() {
            let Some(tile_key) = render_cache.tile_key(tile_index) else {
                continue;
            };
            if tile_key != TileKey::EMPTY {
                keys.insert(tile_key);
            }
        }
    }
    keys
}

fn allocate_missing_render_cache_tiles(
    tree: &mut FlatRenderTree,
    backend_manager: &mut EngineBackendManager,
) {
    let mut parities = std::collections::HashMap::new();
    for node_id in tree.nodes.keys().copied() {
        cache_node_parity(tree, node_id, &mut parities);
    }
    let nodes = Arc::make_mut(&mut tree.nodes);
    for (node_id, node) in nodes.iter_mut() {
        let Some(render_cache) = node.kind.render_cache_mut() else {
            continue;
        };
        let parity = *parities.get(node_id).unwrap_or(&false);
        for tile_index in 0..render_cache.tile_count() {
            let Some(tile_key) = render_cache.tile_key(tile_index) else {
                continue;
            };
            if tile_key != TileKey::EMPTY {
                continue;
            }
            let Some(new_tile_key) =
                backend_manager.alloc_active_with_parity(render_cache.backend(), parity)
            else {
                eprintln!(
                    "failed to allocate render cache tile for node={} tile_index={tile_index} parity={parity}",
                    node_id.0,
                );
                continue;
            };
            if let Err(error) = render_cache.set_tile_key(tile_index, new_tile_key) {
                eprintln!(
                    "failed to assign render cache tile for node={} tile_index={tile_index}: {error}",
                    node_id.0
                );
            }
        }
    }
}

fn cache_node_parity(
    tree: &FlatRenderTree,
    node_id: NodeId,
    memo: &mut std::collections::HashMap<NodeId, bool>,
) -> bool {
    if let Some(&parity) = memo.get(&node_id) {
        return parity;
    }
    let parity = match tree.nodes.get(&node_id).and_then(|node| node.parent_id) {
        Some(parent_id) => !cache_node_parity(tree, parent_id, memo),
        None => false,
    };
    memo.insert(node_id, parity);
    parity
}

#[cfg(test)]
mod tests {
    use super::{
        EngineBackendManager, collect_render_cache_tile_keys, retire_stale_render_cache_tiles,
    };
    use document::{FlatNodeKind, FlatRenderNode, FlatRenderTree, LeafBlendMode, NodeConfig};
    use glaphica_core::{BackendId, NodeId, RenderTreeGeneration, TileKey};
    use images::{Image, layout::ImageLayout};
    use std::{collections::HashMap, sync::Arc};

    fn build_branch_tree(layout: ImageLayout, tile_keys: &[TileKey]) -> FlatRenderTree {
        let mut render_cache = Image::new(layout, BackendId::new(1)).unwrap();
        for (tile_index, tile_key) in tile_keys.iter().copied().enumerate() {
            render_cache.set_tile_key(tile_index, tile_key).unwrap();
        }

        let mut nodes = HashMap::new();
        nodes.insert(
            NodeId(1),
            FlatRenderNode {
                parent_id: None,
                config: NodeConfig {
                    opacity: 1.0,
                    blend_mode: LeafBlendMode::Normal,
                },
                kind: FlatNodeKind::Branch {
                    children: Vec::new(),
                    render_cache,
                },
            },
        );

        FlatRenderTree {
            generation: RenderTreeGeneration(0),
            nodes: Arc::new(nodes),
            root_id: Some(NodeId(1)),
        }
    }

    #[test]
    fn retire_stale_render_cache_tiles_only_retires_removed_keys() {
        let layout = ImageLayout::new(512, 256);
        let mut backend_manager = EngineBackendManager::new();
        backend_manager
            .inner_mut()
            .add_backend(glaphica_core::AtlasLayout::Small11)
            .unwrap();
        backend_manager
            .inner_mut()
            .add_backend(glaphica_core::AtlasLayout::Small11)
            .unwrap();

        let backend = backend_manager
            .inner_mut()
            .backend_mut(BackendId::new(1))
            .unwrap();
        let kept = backend.alloc_active().unwrap().key();
        let removed = backend.alloc_active().unwrap().key();

        let old_tree = build_branch_tree(layout, &[kept, removed]);
        let new_tree = build_branch_tree(layout, &[kept, TileKey::EMPTY]);

        assert_eq!(collect_render_cache_tile_keys(&old_tree).len(), 2);
        assert_eq!(collect_render_cache_tile_keys(&new_tree).len(), 1);

        retire_stale_render_cache_tiles(&old_tree, &new_tree, &mut backend_manager);

        let backend = backend_manager.inner().backend(BackendId::new(1)).unwrap();
        assert_eq!(backend.tile_state(kept).unwrap(), atlas::TileState::Active);
        assert_eq!(
            backend.tile_state(removed).unwrap(),
            atlas::TileState::Cached
        );
    }
}
