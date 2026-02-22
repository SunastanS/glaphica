//! Frame-level orchestration.
//!
//! This module builds `FramePlan`, executes composite/view passes, and commits
//! frame results after synchronization checks.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;

use render_protocol::{
    BRUSH_DAB_CHUNK_CAPACITY, BrushControlAck, BrushControlCommand, BrushProgramActivation,
    BrushProgramKey, BrushProgramUpsert, BrushRenderCommand, BrushStrokeBegin,
    BufferTileCoordinate, ReferenceSetUpsert,
};
use tiles::{DirtySinceResult, TILE_SIZE, TILE_STRIDE, TileAddress, TileAtlasLayout, TileKey};

use crate::{
    BrushControlError, BrushRenderEnqueueError, CompositeEmission, CompositeNodePlan,
    CompositePassContext, DirtyExecutionPlan, DirtyPropagationEngine, DirtyRectMask, DirtyTileMask,
    FrameExecutionResult, FrameGpuTimingReport, FramePlan, FrameState, GpuFrameTimingSlotState,
    PreparedBrushProgram, PresentError, ReferenceSetState, RenderDataResolver, RenderNodeKey,
    RenderTreeNode, Renderer, ViewportMode,
};

fn refresh_cached_render_tree_if_dirty(frame_state: &mut FrameState) {
    if !frame_state.render_tree_dirty {
        return;
    }
    frame_state.cached_render_tree = frame_state
        .bound_tree
        .as_ref()
        .map(|snapshot| snapshot.root.as_ref().clone());
    frame_state.render_tree_dirty = false;
}

fn collect_full_leaf_dirty_tiles(
    layer_dirty_rect_masks: &HashMap<u64, DirtyRectMask>,
) -> HashMap<u64, DirtyTileMask> {
    let mut dirty_leaf_tiles = HashMap::new();
    for (layer_id, dirty_rect_mask) in layer_dirty_rect_masks {
        if matches!(dirty_rect_mask, DirtyRectMask::Full) {
            dirty_leaf_tiles.insert(*layer_id, DirtyTileMask::Full);
        }
    }
    dirty_leaf_tiles
}

fn collect_leaf_layers(node: &RenderTreeNode, output: &mut HashSet<u64>) {
    match node {
        RenderTreeNode::Leaf { layer_id, .. } => {
            output.insert(*layer_id);
        }
        RenderTreeNode::Group { children, .. } => {
            for child in children.iter() {
                collect_leaf_layers(child, output);
            }
        }
    }
}

fn mark_dirty_from_tile_history(
    frame_state: &mut FrameState,
    resolver: &dyn RenderDataResolver,
    render_tree: Option<&RenderTreeNode>,
) {
    let Some(render_tree) = render_tree else {
        return;
    };
    let mut live_layers = HashSet::new();
    collect_leaf_layers(render_tree, &mut live_layers);

    for layer_id in live_layers.iter() {
        let entry = frame_state
            .layer_dirty_versions
            .entry(*layer_id)
            .or_insert(crate::LayerDirtyVersion { last_version: 0 });
        if crate::renderer_perf_log_enabled() {
            eprintln!(
                "[renderer_perf] layer_dirty_poll layer_id={} since_version={}",
                layer_id, entry.last_version
            );
        }
        let Some(result) = resolver.layer_dirty_since(*layer_id, entry.last_version) else {
            if crate::renderer_perf_log_enabled() {
                eprintln!(
                    "[renderer_perf] layer_dirty_poll layer_id={} resolver_result=none",
                    layer_id
                );
            }
            continue;
        };
        match result {
            DirtySinceResult::UpToDate => {
                if crate::renderer_perf_log_enabled() {
                    eprintln!(
                        "[renderer_perf] layer_dirty_poll layer_id={} result=up_to_date",
                        layer_id
                    );
                }
                continue;
            }
            DirtySinceResult::HistoryTruncated => {
                frame_state.dirty_state_store.mark_layer_full(*layer_id);
                if let Some(layer_version) = resolver.layer_version(*layer_id) {
                    entry.last_version = layer_version;
                }
                if crate::renderer_perf_log_enabled() {
                    eprintln!(
                        "[renderer_perf] layer_dirty_poll layer_id={} result=history_truncated action=mark_layer_full new_since_version={}",
                        layer_id, entry.last_version
                    );
                }
                continue;
            }
            DirtySinceResult::HasChanges(query) => {
                entry.last_version = query.latest_version;
                if query.dirty_tiles.is_empty() {
                    if crate::renderer_perf_log_enabled() {
                        eprintln!(
                            "[renderer_perf] layer_dirty_poll layer_id={} result=has_changes_empty latest_version={}",
                            layer_id, query.latest_version
                        );
                    }
                    continue;
                }
                if query.dirty_tiles.is_full() {
                    frame_state.dirty_state_store.mark_layer_full(*layer_id);
                    if crate::renderer_perf_log_enabled() {
                        eprintln!(
                            "[renderer_perf] layer_dirty_poll layer_id={} result=has_changes_full latest_version={} action=mark_layer_full",
                            layer_id, query.latest_version
                        );
                    }
                    continue;
                }
                let dirty_tile_count = query.dirty_tiles.iter_dirty_tiles().count();
                if crate::renderer_perf_log_enabled() {
                    eprintln!(
                        "[renderer_perf] layer_dirty_poll layer_id={} result=has_changes_partial latest_version={} dirty_tile_count={}",
                        layer_id, query.latest_version, dirty_tile_count
                    );
                }
                for (tile_x, tile_y) in query.dirty_tiles.iter_dirty_tiles() {
                    let min_x = tile_x.saturating_mul(TILE_SIZE) as i32;
                    let min_y = tile_y.saturating_mul(TILE_SIZE) as i32;
                    let max_x = tile_x.saturating_add(1).saturating_mul(TILE_SIZE) as i32;
                    let max_y = tile_y.saturating_add(1).saturating_mul(TILE_SIZE) as i32;
                    frame_state.dirty_state_store.mark_layer_rect(
                        *layer_id,
                        crate::DirtyRect {
                            min_x,
                            min_y,
                            max_x,
                            max_y,
                        },
                    );
                }
            }
        }
    }

    frame_state
        .layer_dirty_versions
        .retain(|layer_id, _| live_layers.contains(layer_id));
}

fn split_node_dirty_tiles(
    node_dirty_tiles: HashMap<RenderNodeKey, DirtyTileMask>,
    dirty_leaf_tiles: &mut HashMap<u64, DirtyTileMask>,
    dirty_group_tiles: &mut HashMap<u64, DirtyTileMask>,
) {
    for (node_key, dirty_tile_mask) in node_dirty_tiles {
        match node_key {
            RenderNodeKey::Leaf(layer_id) => {
                dirty_leaf_tiles.insert(layer_id, dirty_tile_mask);
            }
            RenderNodeKey::Group(group_id) => {
                dirty_group_tiles.insert(group_id, dirty_tile_mask);
            }
        }
    }
}

fn group_tile_count(tiles_per_row: u32, tiles_per_column: u32) -> usize {
    usize::try_from(
        tiles_per_row
            .checked_mul(tiles_per_column)
            .expect("group tile count overflow"),
    )
    .expect("group tile count exceeds usize")
}

fn build_dirty_execution_plan(
    render_tree: Option<&RenderTreeNode>,
    layer_dirty_rect_masks: &HashMap<u64, DirtyRectMask>,
    force_group_rerender: bool,
    group_tile_count: usize,
) -> DirtyExecutionPlan {
    let mut dirty_leaf_tiles = collect_full_leaf_dirty_tiles(layer_dirty_rect_masks);
    let mut dirty_group_tiles = HashMap::new();
    let Some(render_tree) = render_tree else {
        return DirtyExecutionPlan {
            force_group_rerender,
            dirty_leaf_tiles,
            dirty_group_tiles,
        };
    };
    if !force_group_rerender {
        let propagation_engine = DirtyPropagationEngine::new(group_tile_count);
        let node_dirty_tiles =
            propagation_engine.collect_node_tile_masks(render_tree, layer_dirty_rect_masks);
        split_node_dirty_tiles(
            node_dirty_tiles,
            &mut dirty_leaf_tiles,
            &mut dirty_group_tiles,
        );
    }
    DirtyExecutionPlan {
        force_group_rerender,
        dirty_leaf_tiles,
        dirty_group_tiles,
    }
}

#[derive(Default)]
struct CompositePlanPerfStats {
    leaf_nodes: usize,
    rebuilt_leaf_nodes: usize,
    rebuilt_leaf_partial_tiles: usize,
    rebuilt_leaf_unknown_tiles: usize,
    group_nodes: usize,
    rerender_group_nodes: usize,
    rerender_group_tiles: usize,
    cache_group_nodes: usize,
}

fn collect_composite_plan_perf_stats(node: &CompositeNodePlan, stats: &mut CompositePlanPerfStats) {
    match node {
        CompositeNodePlan::Leaf {
            should_rebuild,
            dirty_tiles,
            ..
        } => {
            stats.leaf_nodes = stats
                .leaf_nodes
                .checked_add(1)
                .expect("leaf node perf count overflow");
            if *should_rebuild {
                stats.rebuilt_leaf_nodes = stats
                    .rebuilt_leaf_nodes
                    .checked_add(1)
                    .expect("rebuilt leaf node perf count overflow");
                match dirty_tiles {
                    Some(DirtyTileMask::Partial(tiles)) => {
                        stats.rebuilt_leaf_partial_tiles = stats
                            .rebuilt_leaf_partial_tiles
                            .checked_add(tiles.len())
                            .expect("rebuilt leaf partial tile perf count overflow");
                    }
                    _ => {
                        stats.rebuilt_leaf_unknown_tiles = stats
                            .rebuilt_leaf_unknown_tiles
                            .checked_add(1)
                            .expect("rebuilt leaf unknown tile perf count overflow");
                    }
                }
            }
        }
        CompositeNodePlan::Group {
            decision, children, ..
        } => {
            stats.group_nodes = stats
                .group_nodes
                .checked_add(1)
                .expect("group node perf count overflow");
            if matches!(decision.mode, crate::GroupRerenderMode::Rerender) {
                stats.rerender_group_nodes = stats
                    .rerender_group_nodes
                    .checked_add(1)
                    .expect("rerender group node perf count overflow");
                if let Some(tiles) = decision.rerender_tiles.as_ref() {
                    stats.rerender_group_tiles = stats
                        .rerender_group_tiles
                        .checked_add(tiles.len())
                        .expect("rerender group tile perf count overflow");
                }
            } else {
                stats.cache_group_nodes = stats
                    .cache_group_nodes
                    .checked_add(1)
                    .expect("cache group node perf count overflow");
            }
            for child in children {
                collect_composite_plan_perf_stats(child, stats);
            }
        }
    }
}

static FRAME_PLAN_LOG_COUNT: AtomicU32 = AtomicU32::new(0);
static EXECUTE_PLAN_LOG_COUNT: AtomicU32 = AtomicU32::new(0);
const DEFAULT_BRUSH_RADIUS_PIXELS: i32 = 3;

fn assert_brush_dab_write_region_in_slot(
    write_min_x: u32,
    write_min_y: u32,
    write_max_x: u32,
    write_max_y: u32,
    tile_address: TileAddress,
    atlas_layout: TileAtlasLayout,
) {
    let (slot_origin_x, slot_origin_y) = tile_address.atlas_slot_origin_pixels_in(atlas_layout);
    let slot_min_x = i64::from(slot_origin_x);
    let slot_min_y = i64::from(slot_origin_y);
    let slot_max_x = slot_min_x + i64::from(TILE_STRIDE) - 1;
    let slot_max_y = slot_min_y + i64::from(TILE_STRIDE) - 1;
    let write_min_x = i64::from(write_min_x);
    let write_min_y = i64::from(write_min_y);
    let write_max_x = i64::from(write_max_x);
    let write_max_y = i64::from(write_max_y);
    if write_min_x < slot_min_x
        || write_max_x > slot_max_x
        || write_min_y < slot_min_y
        || write_max_y > slot_max_y
    {
        panic!(
            "brush dab write region crosses tile slot boundary: write_bounds=({}, {})-({}, {}) slot_bounds=({}, {})-({}, {}) tile_address={:?}",
            write_min_x,
            write_min_y,
            write_max_x,
            write_max_y,
            slot_min_x,
            slot_min_y,
            slot_max_x,
            slot_max_y,
            tile_address
        );
    }
}

impl Renderer {
    pub fn bind_brush_buffer_tiles(
        &mut self,
        stroke_session_id: u64,
        tile_bindings: Vec<(BufferTileCoordinate, TileKey)>,
    ) {
        let stroke_tiles = self
            .brush_work_state
            .bound_buffer_tile_keys_by_stroke
            .entry(stroke_session_id)
            .or_default();
        let mut coordinate_by_key = HashMap::with_capacity(stroke_tiles.len());
        for (existing_coordinate, existing_key) in stroke_tiles.iter() {
            if let Some(previous_coordinate) =
                coordinate_by_key.insert(*existing_key, *existing_coordinate)
            {
                if previous_coordinate != *existing_coordinate {
                    panic!(
                        "renderer brush tile binding invariant violated before insert for stroke {}: key {:?} is mapped to both ({}, {}) and ({}, {})",
                        stroke_session_id,
                        existing_key,
                        previous_coordinate.tile_x,
                        previous_coordinate.tile_y,
                        existing_coordinate.tile_x,
                        existing_coordinate.tile_y
                    );
                }
            }
        }
        for (tile_coordinate, tile_key) in tile_bindings {
            if self
                .gpu_state
                .brush_buffer_store
                .resolve(tile_key)
                .is_none()
            {
                panic!(
                    "renderer received unresolved brush buffer tile key for stroke {} at ({}, {})",
                    stroke_session_id, tile_coordinate.tile_x, tile_coordinate.tile_y
                );
            }
            if let Some(previous_coordinate) = coordinate_by_key.get(&tile_key).copied() {
                if previous_coordinate != tile_coordinate {
                    panic!(
                        "renderer received duplicate brush tile key binding for stroke {}: key {:?} is mapped to both ({}, {}) and ({}, {})",
                        stroke_session_id,
                        tile_key,
                        previous_coordinate.tile_x,
                        previous_coordinate.tile_y,
                        tile_coordinate.tile_x,
                        tile_coordinate.tile_y
                    );
                }
            } else {
                coordinate_by_key.insert(tile_key, tile_coordinate);
            }
            let previous = stroke_tiles.insert(tile_coordinate, tile_key);
            if let Some(previous_key) = previous {
                if previous_key != tile_key {
                    panic!(
                        "renderer received conflicting brush tile binding for stroke {} at ({}, {}): previous={:?} new={:?}",
                        stroke_session_id,
                        tile_coordinate.tile_x,
                        tile_coordinate.tile_y,
                        previous_key,
                        tile_key
                    );
                }
            }
        }
    }

    pub fn apply_brush_control_command(
        &mut self,
        command: BrushControlCommand,
    ) -> Result<BrushControlAck, BrushControlError> {
        match command {
            BrushControlCommand::UpsertBrushProgram(program) => {
                Ok(self.upsert_brush_program(program))
            }
            BrushControlCommand::ActivateBrushProgram(activation) => {
                self.activate_brush_program(activation)
            }
            BrushControlCommand::UpsertReferenceSet(reference_set) => {
                Ok(self.upsert_reference_set(reference_set))
            }
        }
    }

    fn upsert_reference_set(&mut self, reference_set: ReferenceSetUpsert) -> BrushControlAck {
        self.brush_work_state.reference_sets.insert(
            reference_set.reference_set_id,
            ReferenceSetState {
                selection: reference_set.selection,
            },
        );
        BrushControlAck::ReferenceSetUpserted
    }

    fn upsert_brush_program(&mut self, program: BrushProgramUpsert) -> BrushControlAck {
        let key = BrushProgramKey {
            brush_id: program.brush_id,
            program_revision: program.program_revision,
        };
        if self
            .brush_work_state
            .prepared_programs
            .get(&key)
            .is_some_and(|prepared| prepared.payload_hash == program.payload_hash)
        {
            return BrushControlAck::AlreadyPrepared;
        }

        let wgsl_source = program.wgsl_source.clone();

        let shader_module =
            self.gpu_state
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("renderer.brush_program"),
                    source: wgpu::ShaderSource::Wgsl(wgsl_source.as_ref().into()),
                });
        let compute_pipeline =
            self.gpu_state
                .device
                .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("renderer.brush_program.pipeline"),
                    layout: Some(&self.gpu_state.brush_pipeline_layout),
                    module: &shader_module,
                    entry_point: Some("main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    cache: None,
                });
        self.brush_work_state.prepared_programs.insert(
            key,
            PreparedBrushProgram {
                payload_hash: program.payload_hash,
                _wgsl_source: wgsl_source,
                _compute_pipeline: compute_pipeline,
            },
        );
        BrushControlAck::Prepared
    }

    fn activate_brush_program(
        &mut self,
        activation: BrushProgramActivation,
    ) -> Result<BrushControlAck, BrushControlError> {
        let key = BrushProgramKey {
            brush_id: activation.brush_id,
            program_revision: activation.program_revision,
        };
        if !self.brush_work_state.prepared_programs.contains_key(&key) {
            return Err(BrushControlError::ProgramNotPrepared { key });
        }
        self.brush_work_state
            .active_program_by_brush
            .insert(activation.brush_id, activation.program_revision);
        Ok(BrushControlAck::Activated)
    }

    pub fn enqueue_brush_render_command(
        &mut self,
        command: BrushRenderCommand,
    ) -> Result<(), BrushRenderEnqueueError> {
        match &command {
            BrushRenderCommand::BeginStroke(begin) => self.validate_begin_stroke(*begin)?,
            BrushRenderCommand::AllocateBufferTiles(allocate) => {
                if !self
                    .brush_work_state
                    .active_strokes
                    .contains_key(&allocate.stroke_session_id)
                {
                    return Err(BrushRenderEnqueueError::UnknownStroke {
                        stroke_session_id: allocate.stroke_session_id,
                    });
                }
            }
            BrushRenderCommand::PushDabChunkF32(chunk) => {
                if !self
                    .brush_work_state
                    .active_strokes
                    .contains_key(&chunk.stroke_session_id)
                {
                    return Err(BrushRenderEnqueueError::UnknownStroke {
                        stroke_session_id: chunk.stroke_session_id,
                    });
                }
            }
            BrushRenderCommand::EndStroke(end) => {
                if !self
                    .brush_work_state
                    .active_strokes
                    .contains_key(&end.stroke_session_id)
                {
                    return Err(BrushRenderEnqueueError::UnknownStroke {
                        stroke_session_id: end.stroke_session_id,
                    });
                }
            }
            BrushRenderCommand::MergeBuffer(merge) => {
                let Some(expected_layer_id) = self
                    .brush_work_state
                    .ended_strokes_pending_merge
                    .get(&merge.stroke_session_id)
                    .copied()
                else {
                    return Err(BrushRenderEnqueueError::MergeBeforeStrokeEnd {
                        stroke_session_id: merge.stroke_session_id,
                    });
                };
                if expected_layer_id != merge.target_layer_id {
                    return Err(BrushRenderEnqueueError::MergeTargetLayerMismatch {
                        stroke_session_id: merge.stroke_session_id,
                        expected_layer_id,
                        received_layer_id: merge.target_layer_id,
                    });
                }
            }
        }
        if let BrushRenderCommand::PushDabChunkF32(chunk) = &command {
            self.brush_work_state.pending_dab_count = self
                .brush_work_state
                .pending_dab_count
                .checked_add(
                    u64::try_from(chunk.dab_count())
                        .expect("pending dab count conversion overflow"),
                )
                .expect("pending dab count overflow");
        }
        if let BrushRenderCommand::EndStroke(end) = &command {
            let target_layer_id = self
                .brush_work_state
                .stroke_target_layer
                .get(&end.stroke_session_id)
                .copied()
                .unwrap_or_else(|| {
                    panic!(
                        "target layer missing for ended stroke {}",
                        end.stroke_session_id
                    )
                });
            self.brush_work_state
                .active_strokes
                .remove(&end.stroke_session_id);
            self.brush_work_state
                .stroke_reference_set
                .remove(&end.stroke_session_id);
            self.brush_work_state
                .stroke_target_layer
                .remove(&end.stroke_session_id);
            self.brush_work_state
                .ended_strokes_pending_merge
                .insert(end.stroke_session_id, target_layer_id);
        }
        if let BrushRenderCommand::MergeBuffer(merge) = &command {
            self.brush_work_state
                .ended_strokes_pending_merge
                .remove(&merge.stroke_session_id);
        }
        self.brush_work_state.pending_commands.push_back(command);
        Ok(())
    }

    fn validate_begin_stroke(
        &mut self,
        begin: BrushStrokeBegin,
    ) -> Result<(), BrushRenderEnqueueError> {
        let key = BrushProgramKey {
            brush_id: begin.brush_id,
            program_revision: begin.program_revision,
        };
        if !self.brush_work_state.prepared_programs.contains_key(&key) {
            return Err(BrushRenderEnqueueError::ProgramNotPrepared { key });
        }
        let active_revision = self
            .brush_work_state
            .active_program_by_brush
            .get(&begin.brush_id)
            .copied();
        if active_revision != Some(begin.program_revision) {
            return Err(BrushRenderEnqueueError::ProgramNotActivated { key });
        }
        if !self.brush_work_state.ended_strokes_pending_merge.is_empty() {
            return Err(BrushRenderEnqueueError::BeginWithPendingMerge);
        }
        if !self
            .brush_work_state
            .reference_sets
            .contains_key(&begin.reference_set_id)
        {
            return Err(BrushRenderEnqueueError::ReferenceSetMissing {
                reference_set_id: begin.reference_set_id,
            });
        }
        if let Some(expected) = self
            .brush_work_state
            .active_strokes
            .get(&begin.stroke_session_id)
            .copied()
        {
            if expected != key {
                return Err(BrushRenderEnqueueError::StrokeProgramMismatch {
                    stroke_session_id: begin.stroke_session_id,
                    expected,
                    received: key,
                });
            }
        }
        self.brush_work_state
            .active_strokes
            .insert(begin.stroke_session_id, key);
        self.brush_work_state
            .stroke_reference_set
            .insert(begin.stroke_session_id, begin.reference_set_id);
        self.brush_work_state
            .stroke_target_layer
            .insert(begin.stroke_session_id, begin.target_layer_id);
        Ok(())
    }

    pub fn pending_brush_dab_count(&self) -> u64 {
        self.brush_work_state.pending_dab_count
    }

    pub fn pending_brush_command_count(&self) -> u64 {
        u64::try_from(self.brush_work_state.pending_commands.len())
            .expect("pending brush command count exceeds u64")
    }

    pub fn take_latest_gpu_timing_report(&mut self) -> Option<FrameGpuTimingReport> {
        self.poll_gpu_timing_reports();
        self.gpu_state.gpu_timing.latest_report.take()
    }

    pub fn present_frame(&mut self, frame_id: u64) -> Result<(), PresentError> {
        self.poll_gpu_timing_reports();
        let timing_slot_index = self.reserve_gpu_timing_slot(frame_id);

        self.gpu_state
            .tile_atlas
            .drain_and_execute(&self.gpu_state.queue)
            .map_err(PresentError::TileDrain)?;
        self.gpu_state
            .brush_buffer_atlas
            .drain_and_execute(&self.gpu_state.queue)
            .map_err(PresentError::TileDrain)?;
        self.consume_brush_batches_for_frame();

        let frame_plan = self.build_frame_plan(frame_id);

        let frame = self
            .gpu_state
            .surface
            .get_current_texture()
            .map_err(PresentError::Surface)?;
        let frame_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        {
            let mut clear_encoder =
                self.gpu_state
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("renderer.frame.clear"),
                    });
            self.write_gpu_timing_start(&mut clear_encoder, timing_slot_index);
            {
                let _clear_pass = clear_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("renderer.clear"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &frame_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.07,
                                g: 0.08,
                                b: 0.09,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            }
            self.gpu_state.queue.submit(Some(clear_encoder.finish()));
        }

        let frame_result = self.execute_frame_plan(frame_plan, &frame_view);
        self.resolve_gpu_timing_queries(timing_slot_index, frame_id);

        frame.present();
        self.commit_frame_result(frame_result);
        Ok(())
    }

    fn consume_brush_batches_for_frame(&mut self) {
        let max_commands = self.view_state.brush_command_quota;
        if max_commands == 0 {
            return;
        }

        let target_dabs = max_commands
            .checked_add(u32::from(self.brush_work_state.carry_credit_dabs))
            .expect("target brush dab count overflow");
        let mut consumed_dabs = 0u32;
        let mut brush_encoder: Option<wgpu::CommandEncoder> = None;
        while let Some(command) = self.brush_work_state.pending_commands.front().cloned() {
            match command {
                BrushRenderCommand::BeginStroke(begin) => {
                    let key = BrushProgramKey {
                        brush_id: begin.brush_id,
                        program_revision: begin.program_revision,
                    };
                    self.brush_work_state
                        .executing_strokes
                        .insert(begin.stroke_session_id, key);
                    let _ = self.brush_work_state.pending_commands.pop_front();
                }
                BrushRenderCommand::AllocateBufferTiles(allocate) => {
                    let stroke_tiles = self
                        .brush_work_state
                        .bound_buffer_tile_keys_by_stroke
                        .entry(allocate.stroke_session_id)
                        .or_default();
                    for tile_coordinate in allocate.tiles.iter().copied() {
                        if !stroke_tiles.contains_key(&tile_coordinate) {
                            panic!(
                                "renderer missing brush tile binding for stroke {} at ({}, {})",
                                allocate.stroke_session_id,
                                tile_coordinate.tile_x,
                                tile_coordinate.tile_y
                            );
                        }
                    }
                    let _ = self.brush_work_state.pending_commands.pop_front();
                }
                BrushRenderCommand::EndStroke(end) => {
                    self.brush_work_state
                        .executing_strokes
                        .remove(&end.stroke_session_id);
                    let _ = self.brush_work_state.pending_commands.pop_front();
                }
                BrushRenderCommand::MergeBuffer(merge) => {
                    self.dispatch_brush_merge(&mut brush_encoder);
                    self.brush_work_state
                        .bound_buffer_tile_keys_by_stroke
                        .remove(&merge.stroke_session_id)
                        .unwrap_or_else(|| {
                            panic!(
                                "merge requested for stroke {} without bound brush tile keys",
                                merge.stroke_session_id
                            )
                        });
                    let _ = self.brush_work_state.pending_commands.pop_front();
                }
                BrushRenderCommand::PushDabChunkF32(chunk) => {
                    let chunk_dabs =
                        u32::try_from(chunk.dab_count()).expect("brush dab count exceeds u32");
                    if consumed_dabs.saturating_add(chunk_dabs) > target_dabs {
                        break;
                    }
                    let _stroke_program_key = self
                        .brush_work_state
                        .executing_strokes
                        .get(&chunk.stroke_session_id)
                        .copied()
                        .unwrap_or_else(|| {
                            panic!(
                                "brush stroke {} missing begin command before dab chunk",
                                chunk.stroke_session_id
                            )
                        });
                    self.dispatch_brush_chunk(&mut brush_encoder, &chunk);
                    consumed_dabs = consumed_dabs
                        .checked_add(chunk_dabs)
                        .expect("consumed brush dabs overflow");
                    let _ = self.brush_work_state.pending_commands.pop_front();
                    self.brush_work_state.pending_dab_count = self
                        .brush_work_state
                        .pending_dab_count
                        .checked_sub(u64::from(chunk_dabs))
                        .expect("pending dab count underflow");
                }
            }
        }

        if let Some(encoder) = brush_encoder {
            self.gpu_state.queue.submit(Some(encoder.finish()));
        }

        self.brush_work_state.carry_credit_dabs = u8::try_from(
            target_dabs
                % u32::try_from(BRUSH_DAB_CHUNK_CAPACITY)
                    .expect("brush dab chunk capacity exceeds u32"),
        )
        .expect("carry credit dabs exceeds u8");
    }

    fn dispatch_brush_chunk(
        &mut self,
        brush_encoder: &mut Option<wgpu::CommandEncoder>,
        chunk: &render_protocol::BrushDabChunkF32,
    ) {
        let _stroke_program_key = self
            .brush_work_state
            .executing_strokes
            .get(&chunk.stroke_session_id)
            .copied()
            .unwrap_or_else(|| {
                panic!(
                    "brush stroke {} missing active execution state while dispatching chunk",
                    chunk.stroke_session_id
                )
            });
        let bound_tile_keys = self
            .brush_work_state
            .bound_buffer_tile_keys_by_stroke
            .get(&chunk.stroke_session_id)
            .unwrap_or_else(|| {
                panic!(
                    "brush stroke {} has no bound buffer tile keys before dab dispatch",
                    chunk.stroke_session_id
                )
            });
        let atlas_layout = self.gpu_state.brush_buffer_atlas.layout();
        let mut mapped_dabs = Vec::with_capacity(chunk.dab_count().saturating_mul(4));
        for index in 0..chunk.dab_count() {
            self.append_dab_write_commands(
                chunk.canvas_x()[index],
                chunk.canvas_y()[index],
                chunk.pressure()[index],
                bound_tile_keys,
                atlas_layout,
                &mut mapped_dabs,
            );
        }
        if mapped_dabs.is_empty() {
            return;
        }
        if mapped_dabs.len() > crate::BRUSH_DAB_WRITE_MAX_COMMANDS {
            panic!(
                "expanded brush dab command count {} exceeds brush write buffer capacity {}",
                mapped_dabs.len(),
                crate::BRUSH_DAB_WRITE_MAX_COMMANDS
            );
        }
        if brush_encoder.is_none() {
            *brush_encoder = Some(self.gpu_state.device.create_command_encoder(
                &wgpu::CommandEncoderDescriptor {
                    label: Some("renderer.brush_execution"),
                },
            ));
        }
        let encoder = brush_encoder
            .as_mut()
            .expect("brush command encoder must exist");
        self.gpu_state.queue.write_buffer(
            &self.gpu_state.brush_dab_write_buffer,
            0,
            bytemuck::cast_slice(&mapped_dabs),
        );
        self.gpu_state.queue.write_buffer(
            &self.gpu_state.brush_dab_write_meta_buffer,
            0,
            bytemuck::bytes_of(&crate::BrushDabWriteMetaGpu {
                dab_count: u32::try_from(mapped_dabs.len()).expect("dab command count exceeds u32"),
                texture_width: atlas_layout.atlas_width,
                texture_height: atlas_layout.atlas_height,
                _padding0: 0,
            }),
        );
        let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("renderer.brush_execution.pass"),
            timestamp_writes: None,
        });
        compute_pass.set_pipeline(&self.gpu_state.brush_dab_write_pipeline);
        compute_pass.set_bind_group(0, &self.gpu_state.brush_dab_write_bind_group, &[]);
        compute_pass.dispatch_workgroups(
            u32::try_from(mapped_dabs.len()).expect("dab dispatch count exceeds u32"),
            1,
            1,
        );
    }

    fn dispatch_brush_merge(&mut self, brush_encoder: &mut Option<wgpu::CommandEncoder>) {
        if brush_encoder.is_none() {
            *brush_encoder = Some(self.gpu_state.device.create_command_encoder(
                &wgpu::CommandEncoderDescriptor {
                    label: Some("renderer.brush_merge"),
                },
            ));
        }
    }

    fn append_dab_write_commands(
        &self,
        canvas_x: f32,
        canvas_y: f32,
        pressure: f32,
        bound_tile_keys: &HashMap<BufferTileCoordinate, tiles::TileKey>,
        atlas_layout: TileAtlasLayout,
        mapped_dabs: &mut Vec<crate::BrushDabWriteGpu>,
    ) {
        if !canvas_x.is_finite() || !canvas_y.is_finite() || !pressure.is_finite() {
            panic!("dab values must be finite");
        }
        if DEFAULT_BRUSH_RADIUS_PIXELS < 0 {
            panic!("brush radius must be non-negative");
        }
        let tile_size_f32 = TILE_SIZE as f32;
        let radius = DEFAULT_BRUSH_RADIUS_PIXELS as f32;
        let min_tile_x = Self::tile_index_for_canvas_value(canvas_x - radius, tile_size_f32);
        let max_tile_x = Self::tile_index_for_canvas_value(canvas_x + radius, tile_size_f32);
        let min_tile_y = Self::tile_index_for_canvas_value(canvas_y - radius, tile_size_f32);
        let max_tile_y = Self::tile_index_for_canvas_value(canvas_y + radius, tile_size_f32);
        for tile_y in min_tile_y..=max_tile_y {
            for tile_x in min_tile_x..=max_tile_x {
                let tile_coordinate = BufferTileCoordinate { tile_x, tile_y };
                let tile_key = bound_tile_keys
                    .get(&tile_coordinate)
                    .copied()
                    .unwrap_or_else(|| {
                        panic!(
                            "dab mapped to unbound tile key for stroke buffer: tile=({}, {})",
                            tile_x, tile_y
                        )
                    });
                let tile_address = self
                    .gpu_state
                    .brush_buffer_store
                    .resolve(tile_key)
                    .unwrap_or_else(|| panic!("bound brush buffer tile key must resolve"));
                let tile_origin_x = tile_x as f32 * tile_size_f32;
                let tile_origin_y = tile_y as f32 * tile_size_f32;
                let local_center_x = (canvas_x - tile_origin_x).floor();
                let local_center_y = (canvas_y - tile_origin_y).floor();
                let (content_origin_x, content_origin_y) =
                    tile_address.atlas_content_origin_pixels_in(atlas_layout);
                let center_x = i64::from(content_origin_x) + local_center_x as i64;
                let center_y = i64::from(content_origin_y) + local_center_y as i64;
                let raw_min_x = center_x - i64::from(DEFAULT_BRUSH_RADIUS_PIXELS);
                let raw_max_x = center_x + i64::from(DEFAULT_BRUSH_RADIUS_PIXELS);
                let raw_min_y = center_y - i64::from(DEFAULT_BRUSH_RADIUS_PIXELS);
                let raw_max_y = center_y + i64::from(DEFAULT_BRUSH_RADIUS_PIXELS);
                let (slot_origin_x, slot_origin_y) =
                    tile_address.atlas_slot_origin_pixels_in(atlas_layout);
                let slot_min_x = i64::from(slot_origin_x);
                let slot_min_y = i64::from(slot_origin_y);
                let slot_max_x = slot_min_x + i64::from(TILE_STRIDE) - 1;
                let slot_max_y = slot_min_y + i64::from(TILE_STRIDE) - 1;
                let texture_max_x = i64::from(atlas_layout.atlas_width) - 1;
                let texture_max_y = i64::from(atlas_layout.atlas_height) - 1;
                let write_min_x = raw_min_x.max(slot_min_x).max(0);
                let write_min_y = raw_min_y.max(slot_min_y).max(0);
                let write_max_x = raw_max_x.min(slot_max_x).min(texture_max_x);
                let write_max_y = raw_max_y.min(slot_max_y).min(texture_max_y);
                if write_min_x > write_max_x || write_min_y > write_max_y {
                    continue;
                }
                let write_min_x = u32::try_from(write_min_x).expect("write min x out of u32");
                let write_min_y = u32::try_from(write_min_y).expect("write min y out of u32");
                let write_max_x = u32::try_from(write_max_x).expect("write max x out of u32");
                let write_max_y = u32::try_from(write_max_y).expect("write max y out of u32");
                assert_brush_dab_write_region_in_slot(
                    write_min_x,
                    write_min_y,
                    write_max_x,
                    write_max_y,
                    tile_address,
                    atlas_layout,
                );
                mapped_dabs.push(crate::BrushDabWriteGpu {
                    write_min_x,
                    write_min_y,
                    write_max_x,
                    write_max_y,
                    atlas_layer: tile_address.atlas_layer,
                    pressure: pressure.clamp(0.0, 1.0),
                });
            }
        }
    }

    fn tile_index_for_canvas_value(value: f32, tile_size: f32) -> i32 {
        let tile_index = (value / tile_size).floor();
        if tile_index < i32::MIN as f32 || tile_index > i32::MAX as f32 {
            panic!("tile index out of i32 range");
        }
        tile_index as i32
    }

    fn snapshot_revision(&self) -> u64 {
        self.frame_state
            .bound_tree
            .as_ref()
            .map_or(0, |snapshot| snapshot.revision)
    }

    fn build_frame_plan(&mut self, frame_id: u64) -> FramePlan {
        refresh_cached_render_tree_if_dirty(&mut self.frame_state);
        let render_tree = self.frame_state.cached_render_tree.clone();
        mark_dirty_from_tile_history(
            &mut self.frame_state,
            self.data_state.render_data_resolver.as_ref(),
            render_tree.as_ref(),
        );

        let render_tree = self.frame_state.cached_render_tree.take();
        let layer_dirty_rect_masks = self
            .frame_state
            .dirty_state_store
            .resolve_layer_dirty_rect_masks(self.data_state.render_data_resolver.as_ref());
        let force_group_rerender = self
            .frame_state
            .dirty_state_store
            .is_document_composite_dirty();
        let (tiles_per_row, tiles_per_column) = self.group_tile_grid();
        let dirty_plan = build_dirty_execution_plan(
            render_tree.as_ref(),
            &layer_dirty_rect_masks,
            force_group_rerender,
            group_tile_count(tiles_per_row, tiles_per_column),
        );
        let composite_plan = render_tree
            .as_ref()
            .map(|render_tree| self.build_composite_node_plan(render_tree, &dirty_plan, None));
        if crate::renderer_perf_log_enabled() || crate::renderer_perf_jsonl_enabled() {
            let dirty_leaf_full = dirty_plan
                .dirty_leaf_tiles
                .values()
                .filter(|mask| matches!(mask, DirtyTileMask::Full))
                .count();
            let dirty_leaf_partial_tiles: usize = dirty_plan
                .dirty_leaf_tiles
                .values()
                .map(|mask| match mask {
                    DirtyTileMask::Partial(tiles) => tiles.len(),
                    DirtyTileMask::Full => 0,
                })
                .sum();
            let dirty_group_full = dirty_plan
                .dirty_group_tiles
                .values()
                .filter(|mask| matches!(mask, DirtyTileMask::Full))
                .count();
            let dirty_group_partial_tiles: usize = dirty_plan
                .dirty_group_tiles
                .values()
                .map(|mask| match mask {
                    DirtyTileMask::Partial(tiles) => tiles.len(),
                    DirtyTileMask::Full => 0,
                })
                .sum();
            let mut composite_stats = CompositePlanPerfStats::default();
            if let Some(root_plan) = composite_plan.as_ref() {
                collect_composite_plan_perf_stats(root_plan, &mut composite_stats);
            }
            if crate::renderer_perf_log_enabled() {
                eprintln!(
                    "[renderer_perf] frame_plan frame_id={} dirty_layers={} dirty_leaf_full={} dirty_leaf_partial_tiles={} dirty_group_full={} dirty_group_partial_tiles={} rerender_group_nodes={} rerender_group_tiles={} rebuilt_leaf_nodes={} rebuilt_leaf_partial_tiles={} rebuilt_leaf_unknown_tiles={}",
                    frame_id,
                    layer_dirty_rect_masks.len(),
                    dirty_leaf_full,
                    dirty_leaf_partial_tiles,
                    dirty_group_full,
                    dirty_group_partial_tiles,
                    composite_stats.rerender_group_nodes,
                    composite_stats.rerender_group_tiles,
                    composite_stats.rebuilt_leaf_nodes,
                    composite_stats.rebuilt_leaf_partial_tiles,
                    composite_stats.rebuilt_leaf_unknown_tiles,
                );
            }
            if crate::renderer_perf_jsonl_enabled() {
                crate::renderer_perf_jsonl_write(&format!(
                    "{{\"event\":\"frame_plan\",\"frame_id\":{},\"dirty_layers\":{},\"dirty_leaf_full\":{},\"dirty_leaf_partial_tiles\":{},\"dirty_group_full\":{},\"dirty_group_partial_tiles\":{},\"rerender_group_nodes\":{},\"rerender_group_tiles\":{},\"rebuilt_leaf_nodes\":{},\"rebuilt_leaf_partial_tiles\":{},\"rebuilt_leaf_unknown_tiles\":{}}}",
                    frame_id,
                    layer_dirty_rect_masks.len(),
                    dirty_leaf_full,
                    dirty_leaf_partial_tiles,
                    dirty_group_full,
                    dirty_group_partial_tiles,
                    composite_stats.rerender_group_nodes,
                    composite_stats.rerender_group_tiles,
                    composite_stats.rebuilt_leaf_nodes,
                    composite_stats.rebuilt_leaf_partial_tiles,
                    composite_stats.rebuilt_leaf_unknown_tiles,
                ));
            }
        }
        if FRAME_PLAN_LOG_COUNT.fetch_add(1, Ordering::Relaxed) < 8 {
            eprintln!(
                "[renderer] frame_plan frame_id={} has_render_tree={} dirty_layers={} force_group_rerender={} has_composite_plan={}",
                frame_id,
                render_tree.is_some(),
                layer_dirty_rect_masks.len(),
                force_group_rerender,
                composite_plan.is_some()
            );
        }

        FramePlan {
            version: self
                .frame_state
                .frame_sync
                .version(frame_id, self.snapshot_revision()),
            render_tree,
            composite_plan,
            composite_matrix: self.group_cache_slot_matrix(),
        }
    }

    fn execute_frame_plan(
        &mut self,
        frame_plan: FramePlan,
        frame_view: &wgpu::TextureView,
    ) -> FrameExecutionResult {
        if EXECUTE_PLAN_LOG_COUNT.fetch_add(1, Ordering::Relaxed) < 8 {
            eprintln!(
                "[renderer] execute_frame_plan composite_plan={}",
                frame_plan.composite_plan.is_some()
            );
        }
        if let Some(composite_plan) = frame_plan.composite_plan.as_ref() {
            self.gpu_state.queue.write_buffer(
                &self.gpu_state.view_uniform_buffer,
                0,
                bytemuck::bytes_of(&frame_plan.composite_matrix),
            );

            let mut composite_encoder =
                self.gpu_state
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("renderer.frame.composite"),
                    });
            let composite_context = CompositePassContext {
                target_view: frame_view,
                emission: CompositeEmission::CacheOnly,
                viewport_mode: ViewportMode::Ignore,
            };
            self.render_composite_node_plan(
                composite_plan,
                &composite_context,
                &mut composite_encoder,
            );
            self.gpu_state
                .queue
                .submit(Some(composite_encoder.finish()));

            self.gpu_state.queue.write_buffer(
                &self.gpu_state.view_uniform_buffer,
                0,
                bytemuck::bytes_of(&self.view_state.view_matrix),
            );
            self.view_state.view_matrix_dirty = false;

            let mut view_encoder =
                self.gpu_state
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("renderer.frame.view"),
                    });
            self.draw_root_group_to_surface(frame_view, &mut view_encoder);
            self.gpu_state.queue.submit(Some(view_encoder.finish()));
        }

        FrameExecutionResult {
            version: frame_plan.version,
            render_tree: frame_plan.render_tree,
        }
    }

    fn commit_frame_result(&mut self, frame_result: FrameExecutionResult) {
        assert!(
            self.frame_state
                .frame_sync
                .can_commit(frame_result.version, self.snapshot_revision()),
            "frame result must match current renderer epoch and snapshot"
        );
        self.frame_state.cached_render_tree = frame_result.render_tree;
        self.frame_state.dirty_state_store.clear_layer_dirty_masks();
        self.frame_state
            .dirty_state_store
            .clear_document_composite_dirty();
        self.frame_state
            .frame_sync
            .commit(frame_result.version, self.snapshot_revision());
    }

    fn reserve_gpu_timing_slot(&mut self, frame_id: u64) -> Option<usize> {
        if self.gpu_state.gpu_timing.query_set.is_none() {
            return None;
        }
        let slot_count = self.gpu_state.gpu_timing.slots.len();
        if slot_count == 0 {
            return None;
        }
        let slot_index = usize::try_from(frame_id).expect("frame id exceeds usize") % slot_count;
        if !matches!(
            self.gpu_state.gpu_timing.slots[slot_index].state,
            GpuFrameTimingSlotState::Idle
        ) {
            return None;
        }
        Some(slot_index)
    }

    fn write_gpu_timing_start(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        timing_slot_index: Option<usize>,
    ) {
        let Some(slot_index) = timing_slot_index else {
            return;
        };
        let query_set = self
            .gpu_state
            .gpu_timing
            .query_set
            .as_ref()
            .expect("gpu timing query set must exist for reserved slot");
        let start_query_index = u32::try_from(
            slot_index
                .checked_mul(2)
                .expect("gpu timing query index overflow"),
        )
        .expect("gpu timing query index exceeds u32");
        encoder.write_timestamp(query_set, start_query_index);
    }

    fn resolve_gpu_timing_queries(&mut self, timing_slot_index: Option<usize>, frame_id: u64) {
        let Some(slot_index) = timing_slot_index else {
            return;
        };
        let query_set = self
            .gpu_state
            .gpu_timing
            .query_set
            .as_ref()
            .expect("gpu timing query set must exist for reserved slot");
        let start_query_index = u32::try_from(
            slot_index
                .checked_mul(2)
                .expect("gpu timing query index overflow"),
        )
        .expect("gpu timing query index exceeds u32");
        let end_query_index = start_query_index
            .checked_add(1)
            .expect("gpu timing query index overflow");

        let slot = &mut self.gpu_state.gpu_timing.slots[slot_index];
        let mut resolve_encoder =
            self.gpu_state
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("renderer.frame.gpu_timing.resolve"),
                });
        resolve_encoder.write_timestamp(query_set, end_query_index);
        resolve_encoder.resolve_query_set(
            query_set,
            start_query_index..start_query_index + 2,
            &slot.resolve_buffer,
            0,
        );
        resolve_encoder.copy_buffer_to_buffer(
            &slot.resolve_buffer,
            0,
            &slot.readback_buffer,
            0,
            16,
        );
        self.gpu_state.queue.submit(Some(resolve_encoder.finish()));
        slot.state = GpuFrameTimingSlotState::Submitted { frame_id };
    }

    fn poll_gpu_timing_reports(&mut self) {
        if self.gpu_state.gpu_timing.query_set.is_none() {
            return;
        }
        let _ = self.gpu_state.device.poll(wgpu::PollType::Poll);
        let timestamp_period_ns = self.gpu_state.gpu_timing.timestamp_period_ns;
        let mut latest_report = None;

        for slot in &mut self.gpu_state.gpu_timing.slots {
            let next_state = match std::mem::replace(&mut slot.state, GpuFrameTimingSlotState::Idle)
            {
                GpuFrameTimingSlotState::Idle => GpuFrameTimingSlotState::Idle,
                GpuFrameTimingSlotState::Submitted { frame_id } => {
                    let (sender, receiver) = mpsc::channel();
                    slot.readback_buffer
                        .slice(..)
                        .map_async(wgpu::MapMode::Read, move |result| {
                            sender.send(result).expect("send gpu timing map result");
                        });
                    GpuFrameTimingSlotState::Mapping { frame_id, receiver }
                }
                GpuFrameTimingSlotState::Mapping { frame_id, receiver } => {
                    match receiver.try_recv() {
                        Ok(Ok(())) => {
                            let mapped = slot.readback_buffer.slice(..).get_mapped_range();
                            let timestamps: &[u64] = bytemuck::cast_slice(&mapped);
                            if timestamps.len() >= 2 {
                                let delta_ticks = timestamps[1].saturating_sub(timestamps[0]);
                                let delta_micros =
                                    (delta_ticks as f64) * timestamp_period_ns / 1_000.0;
                                latest_report = Some(FrameGpuTimingReport {
                                    frame_id,
                                    gpu_time_micros: delta_micros.max(0.0).round() as u64,
                                });
                            }
                            drop(mapped);
                            slot.readback_buffer.unmap();
                            GpuFrameTimingSlotState::Idle
                        }
                        Ok(Err(_)) => GpuFrameTimingSlotState::Idle,
                        Err(mpsc::TryRecvError::Empty) => {
                            GpuFrameTimingSlotState::Mapping { frame_id, receiver }
                        }
                        Err(mpsc::TryRecvError::Disconnected) => GpuFrameTimingSlotState::Idle,
                    }
                }
            };
            slot.state = next_state;
        }
        if latest_report.is_some() {
            self.gpu_state.gpu_timing.latest_report = latest_report;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use render_protocol::{BlendMode, ImageHandle, RenderNodeSnapshot};
    use slotmap::KeyData;
    use tiles::{
        TILE_GUTTER, TILE_STRIDE, TileAddress, TileAtlasLayout, TileDirtyBitset, TileDirtyQuery,
    };

    use super::{assert_brush_dab_write_region_in_slot, mark_dirty_from_tile_history};
    use crate::{
        DirtyRectMask, DirtyStateStore, FrameState, FrameSync, LayerDirtyVersion,
        RenderDataResolver, TILE_SIZE,
    };

    #[derive(Default)]
    struct HistoryResolver {
        dirty_by_layer: HashMap<u64, tiles::DirtySinceResult>,
        layer_versions: HashMap<u64, u64>,
    }

    impl RenderDataResolver for HistoryResolver {
        fn document_size(&self) -> (u32, u32) {
            (TILE_SIZE * 4, TILE_SIZE * 4)
        }

        fn visit_image_tiles(
            &self,
            _image_handle: ImageHandle,
            _visitor: &mut dyn FnMut(u32, u32, tiles::TileKey),
        ) {
        }

        fn resolve_tile_address(&self, _tile_key: tiles::TileKey) -> Option<tiles::TileAddress> {
            None
        }

        fn layer_dirty_since(
            &self,
            layer_id: u64,
            _since_version: u64,
        ) -> Option<tiles::DirtySinceResult> {
            self.dirty_by_layer.get(&layer_id).cloned()
        }

        fn layer_version(&self, layer_id: u64) -> Option<u64> {
            self.layer_versions.get(&layer_id).copied()
        }
    }

    #[test]
    fn merge_handle_swap_should_keep_incremental_dirty_from_history() {
        let layer_id = 7u64;
        let render_tree = RenderNodeSnapshot::Group {
            group_id: 0,
            blend: BlendMode::Normal,
            children: vec![RenderNodeSnapshot::Leaf {
                layer_id,
                blend: BlendMode::Normal,
                image_handle: ImageHandle::from(KeyData::from_ffi(12)),
            }]
            .into_boxed_slice()
            .into(),
        };

        let mut dirty_tiles = TileDirtyBitset::new(4, 4).expect("create dirty bitset");
        dirty_tiles.set(1, 0).expect("set dirty tile");
        dirty_tiles.set(2, 1).expect("set dirty tile");
        let resolver = HistoryResolver {
            dirty_by_layer: HashMap::from([(
                layer_id,
                tiles::DirtySinceResult::HasChanges(TileDirtyQuery {
                    latest_version: 99,
                    dirty_tiles,
                }),
            )]),
            layer_versions: HashMap::from([(layer_id, 99)]),
        };

        let mut frame_state = FrameState {
            bound_tree: None,
            cached_render_tree: None,
            render_tree_dirty: false,
            dirty_state_store: DirtyStateStore::default(),
            frame_sync: FrameSync::default(),
            layer_dirty_versions: HashMap::from([(
                layer_id,
                LayerDirtyVersion { last_version: 98 },
            )]),
        };

        mark_dirty_from_tile_history(&mut frame_state, &resolver, Some(&render_tree));
        let masks = frame_state
            .dirty_state_store
            .resolve_layer_dirty_rect_masks(&resolver);
        let Some(mask) = masks.get(&layer_id) else {
            panic!("expected layer dirty mask after merge");
        };

        let DirtyRectMask::Rects(rects) = mask else {
            panic!("expected partial dirty from merge buffer tiles, got {mask:?}");
        };
        let tiles = rects
            .iter()
            .map(|rect| {
                (
                    u32::try_from(rect.min_x).expect("min x non-negative") / TILE_SIZE,
                    u32::try_from(rect.min_y).expect("min y non-negative") / TILE_SIZE,
                )
            })
            .collect::<HashSet<_>>();
        assert_eq!(tiles, HashSet::from([(1, 0), (2, 1)]));
        assert!(matches!(
            frame_state.layer_dirty_versions.get(&layer_id),
            Some(entry) if entry.last_version == 99
        ));
    }

    #[test]
    fn brush_dab_region_assert_allows_write_within_slot() {
        let atlas_layout = TileAtlasLayout {
            tiles_per_row: 1,
            tiles_per_column: 1,
            atlas_width: TILE_STRIDE,
            atlas_height: TILE_STRIDE,
        };
        let tile_address = TileAddress {
            atlas_layer: 0,
            tile_index: 0,
        };
        assert_brush_dab_write_region_in_slot(20, 21, 30, 31, tile_address, atlas_layout);
    }

    #[test]
    fn brush_dab_region_assert_allows_content_edge_for_clipped_write() {
        let atlas_layout = TileAtlasLayout {
            tiles_per_row: 1,
            tiles_per_column: 1,
            atlas_width: TILE_STRIDE,
            atlas_height: TILE_STRIDE,
        };
        let tile_address = TileAddress {
            atlas_layer: 0,
            tile_index: 0,
        };
        assert_brush_dab_write_region_in_slot(
            TILE_GUTTER,
            TILE_GUTTER,
            TILE_GUTTER + 2,
            TILE_GUTTER + 2,
            tile_address,
            atlas_layout,
        );
    }

    #[test]
    #[should_panic(expected = "brush dab write region crosses tile slot boundary")]
    fn brush_dab_region_assert_panics_when_crossing_slot() {
        let atlas_layout = TileAtlasLayout {
            tiles_per_row: 1,
            tiles_per_column: 1,
            atlas_width: TILE_STRIDE,
            atlas_height: TILE_STRIDE,
        };
        let tile_address = TileAddress {
            atlas_layer: 0,
            tile_index: 0,
        };
        assert_brush_dab_write_region_in_slot(
            0,
            0,
            TILE_STRIDE + 1,
            TILE_GUTTER + 2,
            tile_address,
            atlas_layout,
        );
    }
}
