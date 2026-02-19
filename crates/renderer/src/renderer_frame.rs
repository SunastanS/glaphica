//! Frame-level orchestration.
//!
//! This module builds `FramePlan`, executes composite/view passes, and commits
//! frame results after synchronization checks.

use std::collections::HashMap;
use std::sync::mpsc;

use render_protocol::{
    BRUSH_DAB_CHUNK_CAPACITY, BrushControlAck, BrushControlCommand, BrushProgramActivation,
    BrushProgramKey, BrushProgramUpsert, BrushRenderCommand, BrushStrokeBegin, ReferenceSetUpsert,
};

use crate::{
    BrushControlError, BrushRenderEnqueueError, CompositeEmission, CompositePassContext,
    DirtyExecutionPlan, DirtyPropagationEngine, DirtyRectMask, DirtyTileMask, FrameExecutionResult,
    FrameGpuTimingReport, FramePlan, FrameState, GpuFrameTimingSlotState, PreparedBrushProgram,
    PresentError, ReferenceSetState, RenderNodeKey, RenderTreeNode, Renderer, ViewportMode,
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

impl Renderer {
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
                compute_pipeline,
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
                BrushRenderCommand::EndStroke(end) => {
                    self.brush_work_state
                        .executing_strokes
                        .remove(&end.stroke_session_id);
                    let _ = self.brush_work_state.pending_commands.pop_front();
                }
                BrushRenderCommand::MergeBuffer(_merge) => {
                    self.dispatch_brush_merge(&mut brush_encoder);
                    let _ = self.brush_work_state.pending_commands.pop_front();
                }
                BrushRenderCommand::PushDabChunkF32(chunk) => {
                    let chunk_dabs =
                        u32::try_from(chunk.dab_count()).expect("brush dab count exceeds u32");
                    if consumed_dabs.saturating_add(chunk_dabs) > target_dabs {
                        break;
                    }
                    let stroke_program_key = self
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
                    self.dispatch_brush_chunk(
                        &mut brush_encoder,
                        stroke_program_key,
                        chunk.dab_count(),
                    );
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
        stroke_program_key: BrushProgramKey,
        dab_count: usize,
    ) {
        let prepared_program = self
            .brush_work_state
            .prepared_programs
            .get(&stroke_program_key)
            .unwrap_or_else(|| {
                panic!(
                    "brush program missing at execute time: brush_id={} revision={}",
                    stroke_program_key.brush_id, stroke_program_key.program_revision
                )
            });
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
        let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("renderer.brush_execution.pass"),
            timestamp_writes: None,
        });
        compute_pass.set_pipeline(&prepared_program.compute_pipeline);
        compute_pass.dispatch_workgroups(
            u32::try_from(dab_count).expect("dab dispatch count exceeds u32"),
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

    fn snapshot_revision(&self) -> u64 {
        self.frame_state
            .bound_tree
            .as_ref()
            .map_or(0, |snapshot| snapshot.revision)
    }

    fn build_frame_plan(&mut self, frame_id: u64) -> FramePlan {
        refresh_cached_render_tree_if_dirty(&mut self.frame_state);

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
