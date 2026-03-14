use atlas::BackendManager;
use brushes::{BrushEngineRuntime, BrushResamplerDistance, StrokeDrawOutput, TileSlotAllocator};
use document::{Document, FlatRenderTree, SharedRenderTree};
use glaphica_core::{
    BackendId, BrushId, BrushInput, NodeId, RenderTreeGeneration, StrokeId, TileKey,
};
use images::Image;
use std::sync::Arc;
use stroke_input::{InputProcessingConfig, StrokeInputProcessor};

pub struct EngineBackendManager(BackendManager);

impl EngineBackendManager {
    pub fn new() -> Self {
        Self(BackendManager::new())
    }

    pub fn inner(&self) -> &BackendManager {
        &self.0
    }

    pub fn inner_mut(&mut self) -> &mut BackendManager {
        &mut self.0
    }
}

impl TileSlotAllocator for EngineBackendManager {
    fn alloc(&mut self, backend: BackendId) -> Option<TileKey> {
        self.0.backend_mut(backend).and_then(|b| b.alloc().ok())
    }

    fn alloc_with_parity(&mut self, backend: BackendId, parity: bool) -> Option<TileKey> {
        self.0
            .backend_mut(backend)
            .and_then(|b| b.alloc_with_parity(parity).ok())
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

    pub fn shared_tree(&self) -> &SharedRenderTree {
        &self.shared_tree
    }

    pub fn begin_stroke(&mut self, stroke_id: StrokeId) {
        self.active_stroke_id = Some(stroke_id);
        self.input_processor.begin_stroke(stroke_id);
        self.brush_runtime.begin_stroke();
    }

    pub fn end_stroke(&mut self) {
        self.input_processor.end_stroke();
        self.brush_runtime.end_stroke();
        self.active_stroke_id = None;
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

            if let Some((node_id, tile_index, _tile_key)) = output.tile_key_update {
                tile_updates.push((node_id, tile_index));
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

    pub fn rebuild_render_tree(
        &mut self,
    ) -> Result<thread_protocol::RenderTreeUpdatedMsg, document::ImageCreateError> {
        let generation = self.shared_tree.generation();
        let new_generation = RenderTreeGeneration(generation.0 + 1);

        let old_tree = self.shared_tree.read();
        let mut new_tree = self.document.build_flat_render_tree(new_generation)?;
        allocate_missing_render_cache_tiles(&mut new_tree, &mut self.backend_manager);

        let dirty_render_caches = new_tree.diff_render_cache_dirty(&old_tree);

        self.shared_tree.update(new_tree);

        Ok(thread_protocol::RenderTreeUpdatedMsg {
            generation: new_generation,
            dirty_render_caches,
        })
    }
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
            let Some(new_tile_key) = backend_manager.alloc_with_parity(render_cache.backend(), parity)
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
