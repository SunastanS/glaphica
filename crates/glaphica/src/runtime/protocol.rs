/// Runtime command protocol.
///
/// Defines the command/receipt interface between AppCore and GpuRuntime.
/// Commands are coarse-grained: one command per major operation.
use render_protocol::{BrushRenderCommand, RenderTreeSnapshot};
use renderer::MergeCompletionNotice;

/// Coarse-grained commands from AppCore to GpuRuntime.
#[derive(Debug)]
pub enum RuntimeCommand<'a> {
    /// Present a frame and drain tile operations.
    PresentFrame { frame_id: u64 },

    /// Resize the surface.
    Resize { width: u32, height: u32 },

    /// Bind a new render tree.
    BindRenderTree {
        snapshot: &'a RenderTreeSnapshot,
        reason: &'static str,
    },

    /// Enqueue brush render commands.
    EnqueueBrushCommands { commands: &'a [BrushRenderCommand] },

    /// Poll merge completion notices from renderer.
    PollMergeNotices { frame_id: u64 },
}

/// Receipts returned by GpuRuntime after executing commands.
#[derive(Debug)]
pub enum RuntimeReceipt {
    /// Frame presented successfully.
    FramePresented { executed_tile_count: usize },

    /// Surface resized.
    Resized,

    /// Render tree bound.
    RenderTreeBound,

    /// Brush commands enqueued.
    BrushCommandsEnqueued { dab_count: u64 },

    /// Merge notices polled.
    MergeNotices { notices: Vec<MergeCompletionNotice> },
}

/// Runtime errors.
#[derive(Debug)]
pub enum RuntimeError {
    /// Present failed.
    PresentError(renderer::PresentError),

    /// Surface error (subset of PresentError).
    SurfaceError(wgpu::SurfaceError),

    /// Resize failed.
    ResizeError(String),

    /// Brush enqueue failed.
    BrushEnqueueError(renderer::BrushRenderEnqueueError),
}

impl From<renderer::PresentError> for RuntimeError {
    fn from(err: renderer::PresentError) -> Self {
        RuntimeError::PresentError(err)
    }
}

impl From<wgpu::SurfaceError> for RuntimeError {
    fn from(err: wgpu::SurfaceError) -> Self {
        RuntimeError::SurfaceError(err)
    }
}

impl From<renderer::BrushRenderEnqueueError> for RuntimeError {
    fn from(err: renderer::BrushRenderEnqueueError) -> Self {
        RuntimeError::BrushEnqueueError(err)
    }
}
