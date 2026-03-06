use std::sync::Arc;

use brushes::BrushLayoutRegistry;
use document::SharedRenderTree;
use frame_scheduler::FrameHandler;
use glaphica_core::{ImageDirtyTracker, TileDirtyTracker};
use thread_protocol::GpuCmdMsg;

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
    ) -> Result<(), FrameBatchError> {
        match cmd {
            GpuCmdMsg::DrawOp(draw_op) => {
                let mut brush_ctx = WgpuBrushContext {
                    gpu_context: ctx.gpu_context,
                    atlas_storage: ctx.atlas_storage,
                };

                ctx.brush_runtime
                    .apply_draw_op_with_encoder(
                        &mut brush_ctx,
                        draw_op,
                        ctx.brush_layouts,
                        &mut self.encoder,
                    )
                    .map_err(FrameBatchError::BrushError)?;

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
        self.encode_gpu_cmd(cmd, ctx)
    }

    fn execute_render_commands(
        &mut self,
        ctx: &mut FrameBatchContext<'_>,
    ) -> Result<(), FrameBatchError> {
        if ctx.image_dirty_tracker.is_empty() {
            return Ok(());
        }

        let tree = ctx.shared_tree.read();
        let render_cmds = tree.build_render_cmds(ctx.image_dirty_tracker);

        if !render_cmds.is_empty() {
            let mut render_ctx = RenderContext {
                gpu_context: ctx.gpu_context,
                atlas_storage: ctx.atlas_storage,
            };

            ctx.render_executor
                .execute_with_encoder(&mut self.encoder, &mut render_ctx, &render_cmds)
                .map_err(FrameBatchError::RenderError)?;

            self.has_commands = true;
        }

        Ok(())
    }

    fn submit(self) {
        if self.has_commands {
            self.gpu_context.queue.submit(Some(self.encoder.finish()));
        }
    }

    pub fn submit_only(self) {
        self.submit();
    }

    pub fn finish(mut self, ctx: &mut FrameBatchContext<'_>) -> Result<(), FrameBatchError> {
        self.execute_render_commands(ctx)?;
        ctx.image_dirty_tracker.clear();
        ctx.tile_dirty_tracker.clear();
        self.submit();
        Ok(())
    }
}

impl<'a> FrameHandler<FrameBatchContext<'a>> for FrameBatch {
    type Error = FrameBatchError;

    fn handle(&mut self, cmd: &GpuCmdMsg, ctx: &mut FrameBatchContext<'a>) {
        if let Err(error) = self.push_command(cmd, ctx) {
            eprintln!("frame batch command failed: {error:?}");
        }
    }

    fn finalize_frame(self, ctx: &mut FrameBatchContext<'a>) {
        if let Err(error) = self.finish(ctx) {
            eprintln!("frame batch finalize failed: {error:?}");
        }
    }
}
