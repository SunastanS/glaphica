use protocol::MergeItem;
/// Runtime command protocol.
///
/// Defines the command/receipt interface between AppCore and GpuRuntime.
/// Commands are coarse-grained: one command per major operation.
///
/// Design note: Commands own their data (no lifetime parameters) to keep
/// the command interface simple and avoid lifetime propagation.
use std::sync::mpsc::Sender;

use render_protocol::{
    BrushRenderCommand, MergeAuditMeta, MergeExecutionResult, RenderTreeSnapshot,
    StrokeExecutionReceiptId,
};
use renderer::MergeCompletionNotice;
use tiles::TileMergeCompletionNoticeId;
use view::ViewTransform;

/// Coarse-grained commands from AppCore to GpuRuntime.
/// Commands own their data to avoid lifetime complexity.
#[derive(Debug)]
pub enum RuntimeCommand {
    /// Present a frame and drain tile operations.
    PresentFrame { frame_id: u64 },

    /// Resize the surface (with view_transform).
    Resize {
        width: u32,
        height: u32,
        view_transform: ViewTransform,
    },

    /// Resize with handshake (for Phase 4 initialization, no view_transform).
    ResizeHandshake {
        width: u32,
        height: u32,
        ack_sender: Sender<Result<(), RuntimeError>>,
    },

    /// Initialize with handshake (for Phase 4 startup).
    Init {
        ack_sender: Sender<Result<(), RuntimeError>>,
    },

    /// Shutdown engine thread (explicit handshake).
    Shutdown { reason: String },

    /// Bind a new render tree.
    BindRenderTree {
        snapshot: RenderTreeSnapshot,
        reason: &'static str,
    },

    /// Enqueue brush render commands (batch).
    EnqueueBrushCommands { commands: Vec<BrushRenderCommand> },

    /// Enqueue a single brush render command.
    EnqueueBrushCommand { command: BrushRenderCommand },

    /// Poll merge completion notices from renderer.
    PollMergeNotices { frame_id: u64 },

    /// Process merge completions (coarse-grained: submit + poll + initial processing).
    ProcessMergeCompletions { frame_id: u64 },
}

/// Receipts returned by GpuRuntime after executing commands.
#[derive(Debug)]
pub enum RuntimeReceipt {
    /// Frame presented successfully.
    FramePresented { executed_tile_count: usize },

    /// Surface resized.
    Resized,

    /// Surface resized (handshake ack).
    ResizeHandshakeAck,

    /// Initialization completed.
    InitComplete,

    /// Shutdown acknowledged.
    ShutdownAck { reason: String },

    /// Render tree bound.
    RenderTreeBound,

    /// Brush commands enqueued.
    BrushCommandsEnqueued { dab_count: u64 },

    /// Single brush command enqueued.
    BrushCommandEnqueued,

    /// Merge notices polled.
    MergeNotices { notices: Vec<MergeCompletionNotice> },

    /// Merge completions processed (GPU side).
    MergeCompletionsProcessed {
        submission_receipt_ids: Vec<StrokeExecutionReceiptId>,
        renderer_notices: Vec<RendererNotice>,
    },
}

/// Renderer notice for merge completion processing.
#[derive(Debug, Clone)]
pub struct RendererNotice {
    pub receipt_id: StrokeExecutionReceiptId,
    pub audit_meta: MergeAuditMeta,
    pub result: MergeExecutionResult,
    pub notice_id: TileMergeCompletionNoticeId,
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

    /// Merge submit failed.
    MergeSubmit(renderer::MergeSubmitError),

    /// Merge poll failed.
    MergePoll(renderer::MergePollError),

    /// Shutdown requested.
    ShutdownRequested { reason: String },

    /// Engine thread disconnected.
    EngineThreadDisconnected,

    /// Feedback queue timeout (release mode graceful degradation).
    FeedbackQueueTimeout,

    /// Handshake timeout.
    HandshakeTimeout { operation: &'static str },
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

impl From<renderer::MergeSubmitError> for RuntimeError {
    fn from(err: renderer::MergeSubmitError) -> Self {
        RuntimeError::MergeSubmit(err)
    }
}

impl From<renderer::MergePollError> for RuntimeError {
    fn from(err: renderer::MergePollError) -> Self {
        RuntimeError::MergePoll(err)
    }
}

// RuntimeError conversion helpers (replaces panic-prone From impls)
impl RuntimeError {
    /// Convert to BrushRenderEnqueueError if possible.
    /// Returns Err(self) if the variant doesn't match.
    #[must_use]
    pub fn into_brush_enqueue(self) -> Result<renderer::BrushRenderEnqueueError, Self> {
        match self {
            RuntimeError::BrushEnqueueError(e) => Ok(e),
            other => Err(other),
        }
    }

    /// Convert to MergeSubmitError if possible.
    /// Returns Err(self) if the variant doesn't match.
    #[must_use]
    pub fn into_merge_submit(self) -> Result<renderer::MergeSubmitError, Self> {
        match self {
            RuntimeError::MergeSubmit(e) => Ok(e),
            other => Err(other),
        }
    }

    /// Convert to MergePollError if possible.
    /// Returns Err(self) if the variant doesn't match.
    #[must_use]
    pub fn into_merge_poll(self) -> Result<renderer::MergePollError, Self> {
        match self {
            RuntimeError::MergePoll(e) => Ok(e),
            other => Err(other),
        }
    }
}

// MergeItem implementations for RuntimeReceipt and RuntimeError
// Required for GpuFeedbackMergeState

use std::hash::{Hash, Hasher};

impl MergeItem for RuntimeReceipt {
    type MergeKey = ReceiptMergeKey;
    
    fn merge_key(&self) -> Self::MergeKey {
        match self {
            RuntimeReceipt::FramePresented { executed_tile_count } => {
                ReceiptMergeKey::FramePresented(*executed_tile_count)
            }
            RuntimeReceipt::Resized => ReceiptMergeKey::Resized,
            RuntimeReceipt::ResizeHandshakeAck => ReceiptMergeKey::ResizeHandshakeAck,
            RuntimeReceipt::InitComplete => ReceiptMergeKey::InitComplete,
            RuntimeReceipt::ShutdownAck { reason } => {
                ReceiptMergeKey::ShutdownAck(reason.clone())
            }
            RuntimeReceipt::RenderTreeBound => ReceiptMergeKey::RenderTreeBound,
            RuntimeReceipt::BrushCommandsEnqueued { dab_count } => {
                ReceiptMergeKey::BrushCommandsEnqueued(*dab_count)
            }
            RuntimeReceipt::BrushCommandEnqueued => ReceiptMergeKey::BrushCommandEnqueued,
            RuntimeReceipt::MergeNotices { .. } => ReceiptMergeKey::MergeNotices,
            RuntimeReceipt::MergeCompletionsProcessed { .. } => {
                ReceiptMergeKey::MergeCompletionsProcessed
            }
        }
    }
}

impl MergeItem for RuntimeError {
    type MergeKey = ErrorMergeKey;
    
    fn merge_key(&self) -> Self::MergeKey {
        match self {
            RuntimeError::PresentError(_) => ErrorMergeKey::PresentError,
            RuntimeError::SurfaceError(_) => ErrorMergeKey::SurfaceError,
            RuntimeError::ResizeError(_) => ErrorMergeKey::ResizeError,
            RuntimeError::BrushEnqueueError(_) => ErrorMergeKey::BrushEnqueueError,
            RuntimeError::MergeSubmit(_) => ErrorMergeKey::MergeSubmit,
            RuntimeError::MergePoll(_) => ErrorMergeKey::MergePoll,
            RuntimeError::ShutdownRequested { .. } => ErrorMergeKey::ShutdownRequested,
            RuntimeError::EngineThreadDisconnected => ErrorMergeKey::EngineThreadDisconnected,
            RuntimeError::FeedbackQueueTimeout => ErrorMergeKey::FeedbackQueueTimeout,
            RuntimeError::HandshakeTimeout { .. } => ErrorMergeKey::HandshakeTimeout,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ReceiptMergeKey {
    FramePresented(usize),
    Resized,
    ResizeHandshakeAck,
    InitComplete,
    ShutdownAck(String),
    RenderTreeBound,
    BrushCommandsEnqueued(u64),
    BrushCommandEnqueued,
    MergeNotices,
    MergeCompletionsProcessed,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ErrorMergeKey {
    PresentError,
    SurfaceError,
    ResizeError,
    BrushEnqueueError,
    MergeSubmit,
    MergePoll,
    ShutdownRequested,
    EngineThreadDisconnected,
    FeedbackQueueTimeout,
    HandshakeTimeout,
}

