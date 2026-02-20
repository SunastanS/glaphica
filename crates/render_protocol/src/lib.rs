//! Agent collaboration rule for protocol message field changes:
//! - Receiver/executor side may implement first and then report.
//! - Initiator/caller side must report first and only modify after approval.
//!
//! Apply this rule to all message-passing fields defined in this crate.

use std::sync::Arc;

slotmap::new_key_type! {
    pub struct ImageHandle;
}

pub type TransformMatrix4x4 = [f32; 16];
pub const BRUSH_COMMAND_BATCH_CAPACITY: usize = 16;
pub const BRUSH_DAB_CHUNK_CAPACITY: usize = 16;

pub type BrushId = u64;
pub type ProgramRevision = u64;
pub type StrokeSessionId = u64;
pub type ReferenceSetId = u64;
pub type LayerId = u64;
pub type TxToken = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BrushProgramKey {
    pub brush_id: BrushId,
    pub program_revision: ProgramRevision,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrushProgramUpsert {
    pub brush_id: BrushId,
    pub program_revision: ProgramRevision,
    pub payload_hash: u64,
    pub wgsl_source: Arc<str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrushProgramActivation {
    pub brush_id: BrushId,
    pub program_revision: ProgramRevision,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrushStrokeBegin {
    pub stroke_session_id: StrokeSessionId,
    pub brush_id: BrushId,
    pub program_revision: ProgramRevision,
    pub reference_set_id: ReferenceSetId,
    pub target_layer_id: LayerId,
    pub discontinuity_before: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceLayerSelection {
    CurrentLayer,
    CurrentAndBelow,
    AllLayers,
    ExplicitLayer { layer_id: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceSetUpsert {
    pub reference_set_id: ReferenceSetId,
    pub selection: ReferenceLayerSelection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushDabChunkBuildError {
    TooManyDabs,
    MismatchedFieldLengths,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrushDabChunkF32 {
    pub stroke_session_id: StrokeSessionId,
    len: u8,
    canvas_x: [f32; BRUSH_DAB_CHUNK_CAPACITY],
    canvas_y: [f32; BRUSH_DAB_CHUNK_CAPACITY],
    pressure: [f32; BRUSH_DAB_CHUNK_CAPACITY],
}

impl BrushDabChunkF32 {
    pub fn from_slices(
        stroke_session_id: StrokeSessionId,
        canvas_x: &[f32],
        canvas_y: &[f32],
        pressure: &[f32],
    ) -> Result<Self, BrushDabChunkBuildError> {
        if canvas_x.len() > BRUSH_DAB_CHUNK_CAPACITY {
            return Err(BrushDabChunkBuildError::TooManyDabs);
        }
        if canvas_x.len() != canvas_y.len() || canvas_x.len() != pressure.len() {
            return Err(BrushDabChunkBuildError::MismatchedFieldLengths);
        }

        let mut chunk_canvas_x = [0.0; BRUSH_DAB_CHUNK_CAPACITY];
        let mut chunk_canvas_y = [0.0; BRUSH_DAB_CHUNK_CAPACITY];
        let mut chunk_pressure = [0.0; BRUSH_DAB_CHUNK_CAPACITY];
        for index in 0..canvas_x.len() {
            chunk_canvas_x[index] = canvas_x[index];
            chunk_canvas_y[index] = canvas_y[index];
            chunk_pressure[index] = pressure[index];
        }

        Ok(Self {
            stroke_session_id,
            len: u8::try_from(canvas_x.len()).expect("dab count exceeds u8"),
            canvas_x: chunk_canvas_x,
            canvas_y: chunk_canvas_y,
            pressure: chunk_pressure,
        })
    }

    pub fn dab_count(&self) -> usize {
        self.len as usize
    }

    pub fn canvas_x(&self) -> &[f32] {
        &self.canvas_x[..self.dab_count()]
    }

    pub fn canvas_y(&self) -> &[f32] {
        &self.canvas_y[..self.dab_count()]
    }

    pub fn pressure(&self) -> &[f32] {
        &self.pressure[..self.dab_count()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrushStrokeEnd {
    pub stroke_session_id: StrokeSessionId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrushBufferMerge {
    pub stroke_session_id: StrokeSessionId,
    pub target_layer_id: LayerId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StrokeExecutionReceiptId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrokeExecutionReceipt {
    pub receipt_id: StrokeExecutionReceiptId,
    pub stroke_session_id: StrokeSessionId,
    pub tx_token: TxToken,
    pub program_revision: Option<ProgramRevision>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RendererSubmissionId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergePlanMeta {
    pub stroke_session_id: StrokeSessionId,
    pub tx_token: TxToken,
    pub program_revision: Option<ProgramRevision>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeAuditMeta {
    pub frame_id: u64,
    pub renderer_submission_id: RendererSubmissionId,
    pub op_trace_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeOpStage {
    Merge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeErrorContext {
    pub receipt_id: StrokeExecutionReceiptId,
    pub stroke_session_id: StrokeSessionId,
    pub tx_token: TxToken,
    pub frame_id: u64,
    pub renderer_submission_id: RendererSubmissionId,
    pub op_stage: MergeOpStage,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrokeExecutionFailure {
    pub receipt: StrokeExecutionReceipt,
    pub error_ctx: MergeErrorContext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionStatus {
    Pending,
    Succeeded,
    Failed(MergeErrorContext),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptProgress {
    pub receipt: StrokeExecutionReceipt,
    pub status: ExecutionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeExecutionResult {
    Succeeded,
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionReport {
    pub frame_id: u64,
    pub renderer_submission_id: Option<RendererSubmissionId>,
    pub receipt_ids: Vec<StrokeExecutionReceiptId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptTerminalState {
    Finalized,
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpuMergeOp {
    pub base_tile: Option<GpuTileRef>,
    pub stroke_tile: GpuTileRef,
    pub output_tile: GpuTileRef,
    pub blend_mode: BlendMode,
    pub opacity: f32,
    pub op_trace_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuTileRef {
    pub atlas_layer: u32,
    pub tile_index: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BrushControlCommand {
    UpsertBrushProgram(BrushProgramUpsert),
    ActivateBrushProgram(BrushProgramActivation),
    UpsertReferenceSet(ReferenceSetUpsert),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushControlAck {
    Prepared,
    AlreadyPrepared,
    Activated,
    ReferenceSetUpserted,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BrushRenderCommand {
    BeginStroke(BrushStrokeBegin),
    PushDabChunkF32(BrushDabChunkF32),
    MergeBuffer(BrushBufferMerge),
    EndStroke(BrushStrokeEnd),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushCommandBatchBuildError {
    TooManyCommands,
    MismatchedFieldLengths,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrushCommandBatch {
    pub revision: u64,
    pub stroke_session_id: u64,
    pub discontinuity_before: bool,
    len: u8,
    canvas_x: [f32; BRUSH_COMMAND_BATCH_CAPACITY],
    canvas_y: [f32; BRUSH_COMMAND_BATCH_CAPACITY],
    pressure: [f32; BRUSH_COMMAND_BATCH_CAPACITY],
}

impl BrushCommandBatch {
    pub fn from_slices(
        revision: u64,
        stroke_session_id: u64,
        discontinuity_before: bool,
        canvas_x: &[f32],
        canvas_y: &[f32],
        pressure: &[f32],
    ) -> Result<Self, BrushCommandBatchBuildError> {
        if canvas_x.len() > BRUSH_COMMAND_BATCH_CAPACITY {
            return Err(BrushCommandBatchBuildError::TooManyCommands);
        }
        if canvas_x.len() != canvas_y.len() || canvas_x.len() != pressure.len() {
            return Err(BrushCommandBatchBuildError::MismatchedFieldLengths);
        }

        let mut batch_canvas_x = [0.0; BRUSH_COMMAND_BATCH_CAPACITY];
        let mut batch_canvas_y = [0.0; BRUSH_COMMAND_BATCH_CAPACITY];
        let mut batch_pressure = [0.0; BRUSH_COMMAND_BATCH_CAPACITY];
        for index in 0..canvas_x.len() {
            batch_canvas_x[index] = canvas_x[index];
            batch_canvas_y[index] = canvas_y[index];
            batch_pressure[index] = pressure[index];
        }

        Ok(Self {
            revision,
            stroke_session_id,
            discontinuity_before,
            len: u8::try_from(canvas_x.len()).expect("brush command count exceeds u8"),
            canvas_x: batch_canvas_x,
            canvas_y: batch_canvas_y,
            pressure: batch_pressure,
        })
    }

    pub fn command_count(&self) -> usize {
        self.len as usize
    }

    pub fn canvas_x(&self) -> &[f32] {
        &self.canvas_x[..self.command_count()]
    }

    pub fn canvas_y(&self) -> &[f32] {
        &self.canvas_y[..self.command_count()]
    }

    pub fn pressure(&self) -> &[f32] {
        &self.pressure[..self.command_count()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub origin_x: u32,
    pub origin_y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderTreeSnapshot {
    pub revision: u64,
    pub root: Arc<RenderNodeSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderNodeSnapshot {
    Leaf {
        layer_id: u64,
        blend: BlendMode,
        image_handle: ImageHandle,
    },
    Group {
        group_id: u64,
        blend: BlendMode,
        children: Arc<[RenderNodeSnapshot]>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendMode {
    Normal,
    Multiply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendModePipelineStrategy {
    SurfaceAlphaBlend,
    SurfaceMultiplyBlend,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupPassStrategy {
    IsolatedOffscreenComposite,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderStepSupportMatrix {
    normal_blend_strategy: BlendModePipelineStrategy,
    multiply_blend_strategy: BlendModePipelineStrategy,
    group_strategy: GroupPassStrategy,
}

impl RenderStepSupportMatrix {
    pub const fn current_executable_semantics() -> Self {
        Self {
            normal_blend_strategy: BlendModePipelineStrategy::SurfaceAlphaBlend,
            multiply_blend_strategy: BlendModePipelineStrategy::SurfaceMultiplyBlend,
            group_strategy: GroupPassStrategy::IsolatedOffscreenComposite,
        }
    }

    pub const fn blend_strategy(&self, blend_mode: BlendMode) -> BlendModePipelineStrategy {
        match blend_mode {
            BlendMode::Normal => self.normal_blend_strategy,
            BlendMode::Multiply => self.multiply_blend_strategy,
        }
    }

    pub const fn group_strategy(&self) -> GroupPassStrategy {
        self.group_strategy
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderStepUnsupportedReason {
    BlendModeUnsupported { blend_mode: BlendMode },
    GroupCompositingUnsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderStepValidationError {
    pub step_index: usize,
    pub reason: RenderStepUnsupportedReason,
}

impl RenderTreeSnapshot {
    pub fn validate_executable(
        &self,
        support: &RenderStepSupportMatrix,
    ) -> Result<(), RenderStepValidationError> {
        let mut stack = vec![self.root.as_ref()];
        let mut node_index = 0usize;
        while let Some(node) = stack.pop() {
            match node {
                RenderNodeSnapshot::Leaf { blend, .. } => {
                    if matches!(
                        support.blend_strategy(*blend),
                        BlendModePipelineStrategy::Unsupported
                    ) {
                        return Err(RenderStepValidationError {
                            step_index: node_index,
                            reason: RenderStepUnsupportedReason::BlendModeUnsupported {
                                blend_mode: *blend,
                            },
                        });
                    }
                }
                RenderNodeSnapshot::Group {
                    blend, children, ..
                } => {
                    if matches!(
                        support.blend_strategy(*blend),
                        BlendModePipelineStrategy::Unsupported
                    ) {
                        return Err(RenderStepValidationError {
                            step_index: node_index,
                            reason: RenderStepUnsupportedReason::BlendModeUnsupported {
                                blend_mode: *blend,
                            },
                        });
                    }
                    if matches!(support.group_strategy(), GroupPassStrategy::Unsupported) {
                        return Err(RenderStepValidationError {
                            step_index: node_index,
                            reason: RenderStepUnsupportedReason::GroupCompositingUnsupported,
                        });
                    }
                    for child in children.iter().rev() {
                        stack.push(child);
                    }
                }
            }
            node_index = node_index
                .checked_add(1)
                .expect("render tree node index overflow");
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RenderOp {
    SetViewTransform { matrix: TransformMatrix4x4 },
    SetViewport(Viewport),
    BindRenderTree(RenderTreeSnapshot),
    MarkLayerDirty { layer_id: u64 },
    SetBrushCommandQuota { max_commands: u32 },
    DropStaleWorkBeforeRevision { revision: u64 },
    PresentToSurface,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(root: RenderNodeSnapshot) -> RenderTreeSnapshot {
        RenderTreeSnapshot {
            revision: 7,
            root: Arc::new(root),
        }
    }

    #[test]
    fn current_matrix_accepts_multiply_leaf() {
        let root = RenderNodeSnapshot::Leaf {
            layer_id: 11,
            blend: BlendMode::Multiply,
            image_handle: ImageHandle::default(),
        };
        let snapshot = snapshot(root);

        snapshot
            .validate_executable(&RenderStepSupportMatrix::current_executable_semantics())
            .expect("multiply should be supported by current executable semantics");
    }

    #[test]
    fn current_matrix_accepts_group_boundaries() {
        let root = RenderNodeSnapshot::Group {
            group_id: 0,
            blend: BlendMode::Normal,
            children: Arc::from(
                vec![RenderNodeSnapshot::Leaf {
                    layer_id: 5,
                    blend: BlendMode::Normal,
                    image_handle: ImageHandle::default(),
                }]
                .into_boxed_slice(),
            ),
        };
        let snapshot = snapshot(root);

        snapshot
            .validate_executable(&RenderStepSupportMatrix::current_executable_semantics())
            .expect("group boundary should be valid as isolated compositing in current semantics");
    }

    #[test]
    fn group_blend_reports_unsupported_mode() {
        let root = RenderNodeSnapshot::Group {
            group_id: 7,
            blend: BlendMode::Multiply,
            children: Arc::from(Vec::<RenderNodeSnapshot>::new().into_boxed_slice()),
        };
        let snapshot = snapshot(root);

        let support = RenderStepSupportMatrix {
            normal_blend_strategy: BlendModePipelineStrategy::SurfaceAlphaBlend,
            multiply_blend_strategy: BlendModePipelineStrategy::Unsupported,
            group_strategy: GroupPassStrategy::IsolatedOffscreenComposite,
        };

        let error = snapshot
            .validate_executable(&support)
            .expect_err("group multiply blend should be rejected when unsupported");
        assert_eq!(error.step_index, 0);
        assert_eq!(
            error.reason,
            RenderStepUnsupportedReason::BlendModeUnsupported {
                blend_mode: BlendMode::Multiply,
            }
        );
    }

    #[test]
    fn brush_batch_from_slices_preserves_lengths() {
        let batch = BrushCommandBatch::from_slices(
            9,
            100,
            false,
            &[1.0, 2.0, 3.0],
            &[4.0, 5.0, 6.0],
            &[0.5, 0.6, 0.7],
        )
        .expect("build brush command batch");

        assert_eq!(batch.command_count(), 3);
        assert_eq!(batch.canvas_x(), &[1.0, 2.0, 3.0]);
        assert_eq!(batch.canvas_y(), &[4.0, 5.0, 6.0]);
        assert_eq!(batch.pressure(), &[0.5, 0.6, 0.7]);
    }

    #[test]
    fn brush_batch_rejects_mismatched_lengths() {
        let error = BrushCommandBatch::from_slices(1, 2, false, &[1.0, 2.0], &[3.0], &[0.5, 0.6])
            .expect_err("batch should reject mismatched field lengths");
        assert_eq!(error, BrushCommandBatchBuildError::MismatchedFieldLengths);
    }

    #[test]
    fn dab_chunk_from_slices_preserves_lengths() {
        let chunk = BrushDabChunkF32::from_slices(99, &[1.0, 2.0], &[3.0, 4.0], &[0.6, 0.7])
            .expect("build brush dab chunk");

        assert_eq!(chunk.stroke_session_id, 99);
        assert_eq!(chunk.dab_count(), 2);
        assert_eq!(chunk.canvas_x(), &[1.0, 2.0]);
        assert_eq!(chunk.canvas_y(), &[3.0, 4.0]);
        assert_eq!(chunk.pressure(), &[0.6, 0.7]);
    }

    #[test]
    fn dab_chunk_rejects_mismatched_lengths() {
        let error = BrushDabChunkF32::from_slices(4, &[1.0], &[2.0, 3.0], &[0.4])
            .expect_err("dab chunk should reject mismatched field lengths");
        assert_eq!(error, BrushDabChunkBuildError::MismatchedFieldLengths);
    }
}
