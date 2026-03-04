use atlas::BackendManager;
use brushes::{BrushEngineRuntime, StrokeDrawOutput, TileSlotAllocator};
use document::{Document, SharedRenderTree};
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
                min_time_s: 0.008,
                max_time_s: 0.05,
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

    pub fn process_stroke_input(
        &mut self,
        brush_id: BrushId,
        brush_input: &BrushInput,
        node_id: NodeId,
        ref_image: Option<&Image>,
    ) -> Result<Vec<thread_protocol::GpuCmdMsg>, brushes::EngineBrushDispatchError> {
        self.stroke_outputs.clear();

        let generation = self.shared_tree.generation();
        let new_generation = RenderTreeGeneration(generation.0 + 1);

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
            node_id,
            image,
            ref_image,
            &mut self.backend_manager,
            &mut self.stroke_outputs,
        )?;

        let mut gpu_cmds = Vec::with_capacity(self.stroke_outputs.len() * 2);
        let mut tile_updates: Vec<(NodeId, usize)> = Vec::new();

        for output in &self.stroke_outputs {
            if let Some(copy_op) = output.copy_op {
                gpu_cmds.push(thread_protocol::GpuCmdMsg::CopyOp(copy_op));
            }

            gpu_cmds.push(thread_protocol::GpuCmdMsg::DrawOp(output.draw_op.clone()));

            if let Some((node_id, tile_index, _tile_key)) = output.tile_key_update {
                tile_updates.push((node_id, tile_index));
            }
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

            let tree = self.shared_tree.read();
            let new_tree =
                self.document
                    .sync_tile_keys_to_flat_tree(&tree, &tile_updates, new_generation);
            self.shared_tree.update(new_tree);
        }
        Ok(gpu_cmds)
    }

    pub fn rebuild_render_tree(
        &mut self,
    ) -> Result<thread_protocol::RenderTreeUpdatedMsg, document::ImageCreateError> {
        let generation = self.shared_tree.generation();
        let new_generation = RenderTreeGeneration(generation.0 + 1);

        let old_tree = self.shared_tree.read();
        let new_tree = self.document.build_flat_render_tree(new_generation)?;

        let dirty_branch_caches = new_tree.diff_branch_cache_dirty(&old_tree);

        self.shared_tree.update(new_tree);

        Ok(thread_protocol::RenderTreeUpdatedMsg {
            generation: new_generation,
            dirty_branch_caches,
        })
    }
}
