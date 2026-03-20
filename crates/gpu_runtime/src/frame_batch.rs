use std::sync::Arc;
use std::time::Duration;

use brushes::BrushDrawInputLayout;
use brushes::BrushLayoutRegistry;
use document::SharedRenderTree;
use glaphica_core::{ImageDirtyTracker, TileDirtyTracker};
use thread_protocol::{DrawOp, GpuCmdMsg, WriteOp};

use crate::RenderExecutor;
use crate::atlas_runtime::AtlasStorageRuntime;
use crate::brush_runtime::{BrushGpuDispatchError, BrushGpuRuntime};
use crate::context::GpuContext;
use crate::render_executor::RenderContext;
use crate::wgpu_brush_executor::WgpuBrushContext;

pub struct FrameBatch {
    encoder: wgpu::CommandEncoder,
    gpu_context: Arc<GpuContext>,
    has_commands: bool,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct FrameBatchPerfStats {
    pub render_tree_collect: Duration,
    pub parametric_materialize: Duration,
    pub render_tree_composite: Duration,
    pub queue_submit: Duration,
    pub parametric_cmd_count: usize,
    pub parametric_dst_tile_count: usize,
    pub render_cmd_count: usize,
    pub render_dst_tile_count: usize,
    pub render_source_count: usize,
}

pub struct FrameBatchContext<'a> {
    pub gpu_context: &'a GpuContext,
    pub atlas_storage: &'a AtlasStorageRuntime,
    pub render_executor: &'a mut RenderExecutor,
    pub brush_runtime: &'a mut BrushGpuRuntime<crate::wgpu_brush_executor::WgpuBrushExecutor>,
    pub brush_layouts: &'a BrushLayoutRegistry,
    pub shared_tree: &'a SharedRenderTree,
    pub image_dirty_tracker: &'a mut ImageDirtyTracker,
    pub tile_dirty_tracker: &'a mut TileDirtyTracker,
}

#[derive(Debug)]
pub enum FrameBatchError {
    BrushError(BrushGpuDispatchError),
    RenderError(crate::RenderExecutorError),
}

impl FrameBatch {
    pub fn new(gpu_context: &Arc<GpuContext>) -> Self {
        let encoder = gpu_context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-batch"),
            });
        Self {
            encoder,
            gpu_context: gpu_context.clone(),
            has_commands: false,
        }
    }

    fn encode_gpu_cmd(
        &mut self,
        cmd: &GpuCmdMsg,
        ctx: &mut FrameBatchContext<'_>,
        prevalidated_layout: Option<BrushDrawInputLayout>,
    ) -> Result<(), FrameBatchError> {
        match cmd {
            GpuCmdMsg::DrawOp(draw_op) => {
                let mut brush_ctx = WgpuBrushContext {
                    gpu_context: ctx.gpu_context,
                    atlas_storage: ctx.atlas_storage,
                };
                match prevalidated_layout {
                    Some(layout) => {
                        ctx.brush_runtime
                            .apply_draw_op_with_encoder_prevalidated(
                                &mut brush_ctx,
                                draw_op,
                                layout,
                                &mut self.encoder,
                            )
                            .map_err(FrameBatchError::BrushError)?;
                    }
                    None => {
                        ctx.brush_runtime
                            .apply_draw_op_with_encoder(
                                &mut brush_ctx,
                                draw_op,
                                ctx.brush_layouts,
                                &mut self.encoder,
                            )
                            .map_err(FrameBatchError::BrushError)?;
                    }
                }

                ctx.image_dirty_tracker
                    .mark(draw_op.node_id, draw_op.tile_index);
                ctx.tile_dirty_tracker.mark(draw_op.tile_key);
                self.has_commands = true;
            }

            GpuCmdMsg::CopyOp(copy_op) => {
                let mut render_ctx = RenderContext {
                    gpu_context: ctx.gpu_context,
                    atlas_storage: ctx.atlas_storage,
                };

                ctx.render_executor
                    .copy_tile_with_encoder(&mut self.encoder, &mut render_ctx, copy_op)
                    .map_err(FrameBatchError::RenderError)?;

                ctx.tile_dirty_tracker.mark(copy_op.dst_tile_key);
                self.has_commands = true;
            }

            GpuCmdMsg::WriteOp(write_op) => {
                let mut render_ctx = RenderContext {
                    gpu_context: ctx.gpu_context,
                    atlas_storage: ctx.atlas_storage,
                };

                ctx.render_executor
                    .write_tile_with_encoder(&mut self.encoder, &mut render_ctx, write_op)
                    .map_err(FrameBatchError::RenderError)?;

                ctx.tile_dirty_tracker.mark(write_op.dst_tile_key);
                self.has_commands = true;
            }
            GpuCmdMsg::CompositeOp(composite_op) => {
                let mut render_ctx = RenderContext {
                    gpu_context: ctx.gpu_context,
                    atlas_storage: ctx.atlas_storage,
                };

                ctx.render_executor
                    .composite_tile_with_encoder(&mut self.encoder, &mut render_ctx, composite_op)
                    .map_err(FrameBatchError::RenderError)?;

                ctx.tile_dirty_tracker.mark(composite_op.dst_tile_key);
                self.has_commands = true;
            }

            GpuCmdMsg::ClearOp(clear_op) => {
                let mut render_ctx = RenderContext {
                    gpu_context: ctx.gpu_context,
                    atlas_storage: ctx.atlas_storage,
                };

                ctx.render_executor
                    .clear_tile_with_encoder(&mut self.encoder, &mut render_ctx, clear_op)
                    .map_err(FrameBatchError::RenderError)?;

                ctx.tile_dirty_tracker.mark(clear_op.tile_key);
                self.has_commands = true;
            }

            GpuCmdMsg::RenderTreeUpdated(_) | GpuCmdMsg::TileSlotKeyUpdate(_) => {}
        }
        Ok(())
    }

    pub fn push_command(
        &mut self,
        cmd: &GpuCmdMsg,
        ctx: &mut FrameBatchContext<'_>,
    ) -> Result<(), FrameBatchError> {
        self.encode_gpu_cmd(cmd, ctx, None)
    }

    pub fn push_command_with_layout(
        &mut self,
        cmd: &GpuCmdMsg,
        ctx: &mut FrameBatchContext<'_>,
        prevalidated_layout: Option<BrushDrawInputLayout>,
    ) -> Result<(), FrameBatchError> {
        self.encode_gpu_cmd(cmd, ctx, prevalidated_layout)
    }

    pub fn push_draw_batch(
        &mut self,
        draw_ops: &[&DrawOp],
        layouts: &[BrushDrawInputLayout],
        ctx: &mut FrameBatchContext<'_>,
    ) -> Result<(), FrameBatchError> {
        if draw_ops.is_empty() {
            return Ok(());
        }

        let mut brush_ctx = WgpuBrushContext {
            gpu_context: ctx.gpu_context,
            atlas_storage: ctx.atlas_storage,
        };

        ctx.brush_runtime
            .apply_draw_ops_with_encoder_prevalidated_batch(
                &mut brush_ctx,
                draw_ops,
                layouts,
                &mut self.encoder,
            )
            .map_err(FrameBatchError::BrushError)?;

        for draw_op in draw_ops {
            ctx.image_dirty_tracker
                .mark(draw_op.node_id, draw_op.tile_index);
            ctx.tile_dirty_tracker.mark(draw_op.tile_key);
        }
        self.has_commands = true;
        Ok(())
    }

    pub fn push_write_batch(
        &mut self,
        write_ops: &[&WriteOp],
        ctx: &mut FrameBatchContext<'_>,
    ) -> Result<(), FrameBatchError> {
        if write_ops.is_empty() {
            return Ok(());
        }

        let mut render_ctx = RenderContext {
            gpu_context: ctx.gpu_context,
            atlas_storage: ctx.atlas_storage,
        };
        ctx.render_executor
            .write_tiles_with_encoder(&mut self.encoder, &mut render_ctx, write_ops)
            .map_err(FrameBatchError::RenderError)?;

        for write_op in write_ops {
            ctx.tile_dirty_tracker.mark(write_op.dst_tile_key);
        }
        self.has_commands = true;
        Ok(())
    }

    fn execute_render_commands(
        &mut self,
        ctx: &mut FrameBatchContext<'_>,
        stats: &mut FrameBatchPerfStats,
    ) -> Result<(), FrameBatchError> {
        if ctx.image_dirty_tracker.is_empty() {
            return Ok(());
        }

        let collect_started = std::time::Instant::now();
        let tree = ctx.shared_tree.read();
        let parametric_cmds = tree.build_parametric_cmds(ctx.image_dirty_tracker);
        let render_cmds = tree.build_render_cmds(ctx.image_dirty_tracker);
        stats.render_tree_collect = collect_started.elapsed();
        stats.parametric_cmd_count = parametric_cmds.len();
        stats.parametric_dst_tile_count = parametric_cmds
            .iter()
            .map(|cmd| cmd.dst_tile_keys.len())
            .sum();
        stats.render_cmd_count = render_cmds.len();
        stats.render_dst_tile_count = render_cmds.iter().map(|cmd| cmd.to.len()).sum();
        stats.render_source_count = render_cmds
            .iter()
            .map(|cmd| {
                cmd.sources
                    .iter()
                    .map(|source| match source {
                        document::RenderSource::Tile { tile_keys, .. } => tile_keys.len(),
                        document::RenderSource::Parametric { .. } => cmd.to.len(),
                    })
                    .sum::<usize>()
            })
            .sum();

        if !parametric_cmds.is_empty() {
            let mut render_ctx = RenderContext {
                gpu_context: ctx.gpu_context,
                atlas_storage: ctx.atlas_storage,
            };

            let materialize_started = std::time::Instant::now();
            ctx.render_executor
                .materialize_parametric_with_encoder(
                    &mut self.encoder,
                    &mut render_ctx,
                    &parametric_cmds,
                )
                .map_err(FrameBatchError::RenderError)?;
            stats.parametric_materialize = materialize_started.elapsed();

            for cmd in &parametric_cmds {
                for &dst_tile_key in &cmd.dst_tile_keys {
                    ctx.tile_dirty_tracker.mark(dst_tile_key);
                }
            }
            self.has_commands = true;
        }

        if !render_cmds.is_empty() {
            let mut render_ctx = RenderContext {
                gpu_context: ctx.gpu_context,
                atlas_storage: ctx.atlas_storage,
            };

            let composite_started = std::time::Instant::now();
            ctx.render_executor
                .execute_with_encoder(&mut self.encoder, &mut render_ctx, &render_cmds)
                .map_err(FrameBatchError::RenderError)?;
            stats.render_tree_composite = composite_started.elapsed();

            self.has_commands = true;
        }

        Ok(())
    }

    fn submit(self) -> Option<wgpu::SubmissionIndex> {
        if self.has_commands {
            return Some(self.gpu_context.queue.submit(Some(self.encoder.finish())));
        }
        None
    }

    pub fn submit_only(self) -> Option<wgpu::SubmissionIndex> {
        self.submit()
    }

    pub fn finish(
        mut self,
        ctx: &mut FrameBatchContext<'_>,
    ) -> Result<(Option<wgpu::SubmissionIndex>, FrameBatchPerfStats), FrameBatchError> {
        let mut stats = FrameBatchPerfStats::default();
        self.execute_render_commands(ctx, &mut stats)?;
        ctx.image_dirty_tracker.clear();
        ctx.tile_dirty_tracker.clear();
        let submit_started = std::time::Instant::now();
        let submission = self.submit();
        stats.queue_submit = submit_started.elapsed();
        Ok((submission, stats))
    }
}
