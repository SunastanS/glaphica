/// GPU Runtime module.
///
/// Manages GPU resources and executes rendering commands on the main thread.
/// Communicates with AppCore via the runtime protocol.
pub mod protocol;

use std::sync::Arc;

use render_protocol::BrushRenderCommand;
use renderer::{Renderer, ViewOpSender};
use tiles::{GenericR32FloatTileAtlasStore, TileAtlasStore};
use winit::dpi::PhysicalSize;

pub use protocol::{RuntimeCommand, RuntimeError, RuntimeReceipt};

/// GPU Runtime - manages GPU resources and executes rendering commands.
///
/// Runs on the main thread. All operations are synchronous (no cross-thread communication).
pub struct GpuRuntime {
    /// Core renderer with GPU resources.
    renderer: Renderer,

    /// View operation sender for render tree updates.
    view_sender: ViewOpSender,

    /// Tile atlas store (CPU-side allocation).
    /// AppCore also holds a clone of this Arc for merge operations.
    pub(crate) atlas_store: Arc<TileAtlasStore>,

    /// Brush buffer store (CPU-side allocation).
    /// AppCore also holds a clone of this Arc for merge operations.
    pub(crate) brush_buffer_store: Arc<GenericR32FloatTileAtlasStore>,

    /// Current surface size.
    surface_size: PhysicalSize<u32>,

    /// Next frame ID to use.
    next_frame_id: u64,
}

impl GpuRuntime {
    /// Create a new GpuRuntime from existing components.
    pub fn new(
        renderer: Renderer,
        view_sender: ViewOpSender,
        atlas_store: Arc<TileAtlasStore>,
        brush_buffer_store: Arc<GenericR32FloatTileAtlasStore>,
        surface_size: PhysicalSize<u32>,
        next_frame_id: u64,
    ) -> Self {
        Self {
            renderer,
            view_sender,
            atlas_store,
            brush_buffer_store,
            surface_size,
            next_frame_id,
        }
    }

    /// Execute a runtime command and return a receipt.
    pub fn execute(&mut self, command: RuntimeCommand) -> Result<RuntimeReceipt, RuntimeError> {
        match command {
            RuntimeCommand::PresentFrame { frame_id } => {
                // Drain view ops before presenting
                self.renderer.drain_view_ops();

                // Present frame and get tile execution count
                match self.renderer.present_frame(frame_id) {
                    Ok(()) => {
                        // For now, return 0 executed tiles - this can be enriched later
                        Ok(RuntimeReceipt::FramePresented {
                            executed_tile_count: 0,
                        })
                    }
                    Err(err) => Err(err.into()),
                }
            }

            RuntimeCommand::Resize {
                width,
                height,
                view_transform,
            } => {
                self.renderer.resize(width, height);
                self.surface_size = PhysicalSize::new(width, height);
                crate::push_view_state(&self.view_sender, &view_transform, self.surface_size);
                Ok(RuntimeReceipt::Resized)
            }

            RuntimeCommand::BindRenderTree { .. } => {
                // TODO: Implement when migrating bind logic
                Ok(RuntimeReceipt::RenderTreeBound)
            }

            RuntimeCommand::EnqueueBrushCommands { commands } => {
                let mut dab_count = 0u64;
                for command in commands {
                    match command {
                        BrushRenderCommand::PushDabChunkF32(_) => {
                            dab_count += 1;
                        }
                        _ => {}
                    }
                    self.renderer.enqueue_brush_render_command(command)?;
                }
                Ok(RuntimeReceipt::BrushCommandsEnqueued { dab_count })
            }

            RuntimeCommand::EnqueueBrushCommand { command } => {
                self.renderer.enqueue_brush_render_command(command)?;
                Ok(RuntimeReceipt::BrushCommandEnqueued)
            }

            RuntimeCommand::PollMergeNotices { frame_id: _ } => {
                // TODO: Implement merge polling when migrating merge logic
                Ok(RuntimeReceipt::MergeNotices {
                    notices: Vec::new(),
                })
            }

            RuntimeCommand::ProcessMergeCompletions { frame_id } => {
                // GPU side: submit pending merges and poll completion notices
                let submission_report = self
                    .renderer
                    .submit_pending_merges(frame_id, u32::MAX)
                    .map_err(RuntimeError::from)?;

                let renderer_notices = self
                    .renderer
                    .poll_completion_notices(frame_id)
                    .map_err(RuntimeError::from)?;

                // Convert to protocol types
                let protocol_notices: Vec<protocol::RendererNotice> = renderer_notices
                    .into_iter()
                    .map(|notice| {
                        let notice_id = crate::notice_id_from_renderer(&notice);
                        protocol::RendererNotice {
                            receipt_id: notice.receipt_id,
                            audit_meta: notice.audit_meta,
                            result: notice.result,
                            notice_id,
                        }
                    })
                    .collect();

                Ok(RuntimeReceipt::MergeCompletionsProcessed {
                    submission_receipt_ids: submission_report.receipt_ids,
                    renderer_notices: protocol_notices,
                })
            }
        }
    }

    /// Get the next frame ID and increment the counter.
    pub fn next_frame_id(&mut self) -> u64 {
        let id = self.next_frame_id;
        self.next_frame_id += 1;
        id
    }

    /// Get the current surface size.
    pub fn surface_size(&self) -> PhysicalSize<u32> {
        self.surface_size
    }

    /// Get the view sender.
    pub fn view_sender(&self) -> &ViewOpSender {
        &self.view_sender
    }

    /// Get a reference to the renderer.
    ///
    /// This is for read-only access. For mutations, use `execute()` with commands.
    pub fn renderer(&self) -> &Renderer {
        &self.renderer
    }

    /// Get a mutable reference to the renderer.
    ///
    /// Use with caution - prefer command interface for mutations.
    pub fn renderer_mut(&mut self) -> &mut Renderer {
        &mut self.renderer
    }

    /// Get the atlas store.
    pub fn atlas_store(&self) -> &Arc<TileAtlasStore> {
        &self.atlas_store
    }

    /// Get the brush buffer store.
    pub fn brush_buffer_store(&self) -> &Arc<GenericR32FloatTileAtlasStore> {
        &self.brush_buffer_store
    }

    /// Bind brush buffer tiles for a stroke.
    pub fn bind_brush_buffer_tiles(
        &mut self,
        stroke_session_id: u64,
        tile_bindings: Vec<(render_protocol::BufferTileCoordinate, tiles::TileKey)>,
    ) {
        self.renderer
            .bind_brush_buffer_tiles(stroke_session_id, tile_bindings);
    }

    /// Drain view operations before rendering.
    ///
    /// This is a runtime-level operation that must be called before presenting.
    pub fn drain_view_ops(&mut self) {
        self.renderer.drain_view_ops();
    }
}
