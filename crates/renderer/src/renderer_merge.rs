//! Merge submission and receipt orchestration.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc;
use std::time::Instant;

use render_protocol::{
    BlendMode, ExecutionStatus, GpuMergeOp, GpuTileRef, MergeAuditMeta, MergeErrorContext,
    MergeExecutionResult, MergeOpStage, MergePlanMeta, ReceiptProgress, ReceiptTerminalState,
    RendererSubmissionId, StrokeExecutionFailure, StrokeExecutionReceipt, StrokeExecutionReceiptId,
    SubmissionReport,
};
use tiles::TILE_STRIDE;

use crate::Renderer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeEnqueueError {
    DuplicateReceipt {
        receipt_id: StrokeExecutionReceiptId,
    },
    EmptyMergeOps {
        receipt_id: StrokeExecutionReceiptId,
    },
    ReceiptMetaMismatch {
        receipt_id: StrokeExecutionReceiptId,
        receipt_stroke_session_id: u64,
        meta_stroke_session_id: u64,
        receipt_tx_token: u64,
        meta_tx_token: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeSubmitError {
    ZeroBudget,
    UnknownReceipt {
        receipt_id: StrokeExecutionReceiptId,
    },
    IllegalState {
        receipt_id: StrokeExecutionReceiptId,
        state: &'static str,
    },
    DuplicateSubmissionInFlight {
        renderer_submission_id: RendererSubmissionId,
    },
    UnknownSubmission {
        renderer_submission_id: RendererSubmissionId,
    },
    InvalidGpuMergeOp {
        renderer_submission_id: RendererSubmissionId,
        receipt_id: Option<StrokeExecutionReceiptId>,
        op_trace_id: u64,
        reason: &'static str,
        tile_ref: Option<GpuTileRef>,
        tile_ref_role: Option<MergeTileRefRole>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeTileRefRole {
    Base,
    Stroke,
    Output,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergePollError {
    UnknownSubmission {
        renderer_submission_id: RendererSubmissionId,
    },
    DuplicateAckableNotice {
        receipt_id: StrokeExecutionReceiptId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeCompletionNotice {
    pub receipt_id: StrokeExecutionReceiptId,
    pub audit_meta: MergeAuditMeta,
    pub result: MergeExecutionResult,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeAckError {
    NoticeNotAckable {
        receipt_id: StrokeExecutionReceiptId,
    },
    NoticeMismatch {
        receipt_id: StrokeExecutionReceiptId,
    },
    UnknownReceipt {
        receipt_id: StrokeExecutionReceiptId,
    },
    UnknownSubmission {
        receipt_id: StrokeExecutionReceiptId,
        renderer_submission_id: RendererSubmissionId,
    },
    ReceiptSubmissionMismatch {
        receipt_id: StrokeExecutionReceiptId,
        expected_submission_id: RendererSubmissionId,
        received_submission_id: RendererSubmissionId,
    },
    ReceiptNotInSubmission {
        receipt_id: StrokeExecutionReceiptId,
        renderer_submission_id: RendererSubmissionId,
    },
    IllegalState {
        receipt_id: StrokeExecutionReceiptId,
        state: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeFinalizeError {
    UnknownReceipt {
        receipt_id: StrokeExecutionReceiptId,
    },
    SubmissionStillInFlight {
        receipt_id: StrokeExecutionReceiptId,
        renderer_submission_id: RendererSubmissionId,
    },
    IllegalTransition {
        receipt_id: StrokeExecutionReceiptId,
        state: &'static str,
        terminal_state: ReceiptTerminalState,
    },
}

#[derive(Debug, Clone)]
struct MergeReceiptEntry {
    receipt: StrokeExecutionReceipt,
    gpu_merge_ops: Vec<GpuMergeOp>,
    state: MergeReceiptState,
}

#[derive(Debug, Clone)]
enum MergeReceiptState {
    Planned,
    Submitted {
        renderer_submission_id: RendererSubmissionId,
        frame_id: u64,
    },
    Pending {
        renderer_submission_id: RendererSubmissionId,
        frame_id: u64,
    },
    Succeeded,
    Failed,
}

impl MergeReceiptState {
    fn label(&self) -> &'static str {
        match self {
            Self::Planned => "Planned",
            Self::Submitted { .. } => "Submitted",
            Self::Pending { .. } => "Pending",
            Self::Succeeded => "Succeeded",
            Self::Failed => "Failed",
        }
    }
}

#[derive(Debug, Clone)]
struct SubmissionEntry {
    frame_id: u64,
    receipt_ids: Vec<StrokeExecutionReceiptId>,
}

#[derive(Debug, Clone, Copy)]
struct SubmissionGpuOp {
    receipt_id: StrokeExecutionReceiptId,
    gpu_merge_op: GpuMergeOp,
}

#[derive(Debug, Clone)]
struct PreparedSubmission {
    report: SubmissionReport,
    submission_gpu_ops: Vec<SubmissionGpuOp>,
}

#[derive(Debug)]
struct InFlightSubmission {
    renderer_submission_id: RendererSubmissionId,
    frame_id: u64,
    done_receiver: mpsc::Receiver<()>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GpuSubmissionCompletion {
    Completed,
    CallbackDisconnected,
    DeviceError {
        message: String,
    },
    Lost {
        reason: wgpu::DeviceLostReason,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompletedSubmission {
    renderer_submission_id: RendererSubmissionId,
    frame_id: u64,
    completion: GpuSubmissionCompletion,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct MergeUniformGpu {
    base_slot_origin_uv: [f32; 2],
    stroke_slot_origin_uv: [f32; 2],
    slot_uv_size: [f32; 2],
    base_layer: f32,
    stroke_layer: f32,
    has_base: f32,
    opacity: f32,
    blend_mode: u32,
    _padding0: [u32; 3],
}

fn tile_ref_slot_origin_uv(tile_ref: GpuTileRef, atlas_layout: tiles::TileAtlasLayout) -> [f32; 2] {
    let tiles_per_row = atlas_layout.tiles_per_row;
    let tiles_per_column = atlas_layout.tiles_per_column;
    assert!(tiles_per_row > 0, "tiles_per_row must be positive");
    assert!(tiles_per_column > 0, "tiles_per_column must be positive");
    let tile_index = u32::from(tile_ref.tile_index);
    let tile_x = tile_index % tiles_per_row;
    let tile_y = tile_index / tiles_per_row;
    let slot_x = tile_x
        .checked_mul(TILE_STRIDE)
        .expect("tile slot x overflow");
    let slot_y = tile_y
        .checked_mul(TILE_STRIDE)
        .expect("tile slot y overflow");
    [
        slot_x as f32 / atlas_layout.atlas_width as f32,
        slot_y as f32 / atlas_layout.atlas_height as f32,
    ]
}

fn tile_ref_slot_origin(
    tile_ref: GpuTileRef,
    atlas_layout: tiles::TileAtlasLayout,
) -> wgpu::Origin3d {
    let tiles_per_row = atlas_layout.tiles_per_row;
    let tiles_per_column = atlas_layout.tiles_per_column;
    let tile_index = u32::from(tile_ref.tile_index);
    let tile_x = tile_index % tiles_per_row;
    let tile_y = tile_index / tiles_per_row;
    assert!(tile_y < tiles_per_column, "tile index out of bounds");
    wgpu::Origin3d {
        x: tile_x
            .checked_mul(TILE_STRIDE)
            .expect("tile slot x overflow"),
        y: tile_y
            .checked_mul(TILE_STRIDE)
            .expect("tile slot y overflow"),
        z: tile_ref.atlas_layer,
    }
}

fn merge_output_copy_extent() -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: TILE_STRIDE,
        height: TILE_STRIDE,
        depth_or_array_layers: 1,
    }
}

fn validate_tile_ref(
    tile_ref: GpuTileRef,
    atlas_layout: tiles::TileAtlasLayout,
    atlas_layer_count: u32,
) -> bool {
    if tile_ref.atlas_layer >= atlas_layer_count {
        return false;
    }
    let tile_index = u32::from(tile_ref.tile_index);
    tile_index
        < atlas_layout
            .tiles_per_row
            .saturating_mul(atlas_layout.tiles_per_column)
}

fn blend_mode_to_u32(mode: BlendMode) -> u32 {
    match mode {
        BlendMode::Normal => 0,
        BlendMode::Multiply => 1,
    }
}

#[derive(Debug, Default)]
pub(crate) struct MergeOrchestrator {
    next_submission_id: u64,
    receipts: HashMap<StrokeExecutionReceiptId, MergeReceiptEntry>,
    pending_receipts: VecDeque<StrokeExecutionReceiptId>,
    submissions: HashMap<RendererSubmissionId, SubmissionEntry>,
    in_flight_submissions: Vec<InFlightSubmission>,
    ackable_notices: HashMap<StrokeExecutionReceiptId, MergeCompletionNotice>,
    progress_queue: VecDeque<ReceiptProgress>,
    succeeded_receipts: VecDeque<StrokeExecutionReceipt>,
    failed_receipts: VecDeque<StrokeExecutionFailure>,
}

impl MergeOrchestrator {
    pub(crate) fn enqueue_planned_merge(
        &mut self,
        receipt: StrokeExecutionReceipt,
        gpu_merge_ops: Vec<GpuMergeOp>,
        meta: MergePlanMeta,
    ) -> Result<(), MergeEnqueueError> {
        if gpu_merge_ops.is_empty() {
            return Err(MergeEnqueueError::EmptyMergeOps {
                receipt_id: receipt.receipt_id,
            });
        }
        if receipt.stroke_session_id != meta.stroke_session_id || receipt.tx_token != meta.tx_token
        {
            return Err(MergeEnqueueError::ReceiptMetaMismatch {
                receipt_id: receipt.receipt_id,
                receipt_stroke_session_id: receipt.stroke_session_id,
                meta_stroke_session_id: meta.stroke_session_id,
                receipt_tx_token: receipt.tx_token,
                meta_tx_token: meta.tx_token,
            });
        }
        if self.receipts.contains_key(&receipt.receipt_id) {
            return Err(MergeEnqueueError::DuplicateReceipt {
                receipt_id: receipt.receipt_id,
            });
        }
        self.pending_receipts.push_back(receipt.receipt_id);
        self.receipts.insert(
            receipt.receipt_id,
            MergeReceiptEntry {
                receipt,
                gpu_merge_ops,
                state: MergeReceiptState::Planned,
            },
        );
        Ok(())
    }

    fn prepare_pending_submission(
        &mut self,
        frame_id: u64,
        budget: u32,
    ) -> Result<PreparedSubmission, MergeSubmitError> {
        if budget == 0 {
            return Err(MergeSubmitError::ZeroBudget);
        }
        let submitted_receipt_ids: Vec<_> = self
            .pending_receipts
            .iter()
            .take(budget as usize)
            .copied()
            .collect();

        if submitted_receipt_ids.is_empty() {
            return Ok(PreparedSubmission {
                report: SubmissionReport {
                    frame_id,
                    renderer_submission_id: None,
                    receipt_ids: Vec::new(),
                },
                submission_gpu_ops: Vec::new(),
            });
        }

        let renderer_submission_id = RendererSubmissionId(self.next_submission_id);
        self.next_submission_id = self
            .next_submission_id
            .checked_add(1)
            .expect("renderer submission id overflow");

        let mut submission_gpu_ops = Vec::new();
        for receipt_id in &submitted_receipt_ids {
            let entry = self
                .receipts
                .get(receipt_id)
                .ok_or(MergeSubmitError::UnknownReceipt {
                    receipt_id: *receipt_id,
                })?;
            if !matches!(entry.state, MergeReceiptState::Planned) {
                return Err(MergeSubmitError::IllegalState {
                    receipt_id: *receipt_id,
                    state: entry.state.label(),
                });
            }
            submission_gpu_ops.extend(entry.gpu_merge_ops.iter().copied().map(|gpu_merge_op| {
                SubmissionGpuOp {
                    receipt_id: *receipt_id,
                    gpu_merge_op,
                }
            }));
        }

        Ok(PreparedSubmission {
            report: SubmissionReport {
                frame_id,
                renderer_submission_id: Some(renderer_submission_id),
                receipt_ids: submitted_receipt_ids,
            },
            submission_gpu_ops,
        })
    }

    fn commit_submitted_submission(
        &mut self,
        prepared: PreparedSubmission,
        done_receiver: mpsc::Receiver<()>,
    ) -> SubmissionReport {
        let Some(renderer_submission_id) = prepared.report.renderer_submission_id else {
            return prepared.report;
        };
        let frame_id = prepared.report.frame_id;

        assert!(
            !self.submissions.contains_key(&renderer_submission_id),
            "internal invariant violated: submission already exists before commit ({renderer_submission_id:?})"
        );
        assert!(
            !self
                .in_flight_submissions
                .iter()
                .any(|entry| entry.renderer_submission_id == renderer_submission_id),
            "internal invariant violated: submission already in-flight before commit ({renderer_submission_id:?})"
        );

        for (index, receipt_id) in prepared.report.receipt_ids.iter().enumerate() {
            let pending_receipt_id = self.pending_receipts.get(index).expect(
                "internal invariant violated: pending queue shorter than prepared submission",
            );
            assert_eq!(
                *pending_receipt_id, *receipt_id,
                "internal invariant violated: pending queue order diverged before commit"
            );

            let entry = self
                .receipts
                .get(receipt_id)
                .expect("internal invariant violated: prepared receipt missing at commit");
            assert!(
                matches!(entry.state, MergeReceiptState::Planned),
                "internal invariant violated: prepared receipt not Planned at commit"
            );
        }

        for _ in 0..prepared.report.receipt_ids.len() {
            self.pending_receipts
                .pop_front()
                .expect("pending queue must contain committed receipt");
        }

        for receipt_id in &prepared.report.receipt_ids {
            let entry = self.receipts.get_mut(receipt_id).expect(
                "internal invariant violated: prepared receipt missing during state transition",
            );
            entry.state = MergeReceiptState::Submitted {
                renderer_submission_id,
                frame_id,
            };
            entry.state = MergeReceiptState::Pending {
                renderer_submission_id,
                frame_id,
            };
            self.progress_queue.push_back(ReceiptProgress {
                receipt: entry.receipt.clone(),
                status: ExecutionStatus::Pending,
            });
        }

        self.submissions.insert(
            renderer_submission_id,
            SubmissionEntry {
                frame_id,
                receipt_ids: prepared.report.receipt_ids.clone(),
            },
        );

        self.in_flight_submissions.push(InFlightSubmission {
            renderer_submission_id,
            frame_id,
            done_receiver,
        });

        prepared.report
    }

    pub(crate) fn drain_receipt_progress_events(&mut self, _frame_id: u64) -> Vec<ReceiptProgress> {
        self.progress_queue.drain(..).collect()
    }

    fn drain_completed_submissions(&mut self) -> Vec<CompletedSubmission> {
        let mut completed = Vec::new();
        let mut still_pending = Vec::new();
        for in_flight in self.in_flight_submissions.drain(..) {
            match in_flight.done_receiver.try_recv() {
                Ok(()) => {
                    completed.push(CompletedSubmission {
                        renderer_submission_id: in_flight.renderer_submission_id,
                        frame_id: in_flight.frame_id,
                        completion: GpuSubmissionCompletion::Completed,
                    });
                }
                Err(mpsc::TryRecvError::Empty) => still_pending.push(in_flight),
                Err(mpsc::TryRecvError::Disconnected) => {
                    completed.push(CompletedSubmission {
                        renderer_submission_id: in_flight.renderer_submission_id,
                        frame_id: in_flight.frame_id,
                        completion: GpuSubmissionCompletion::CallbackDisconnected,
                    });
                }
            }
        }
        self.in_flight_submissions = still_pending;
        completed
    }

    fn fail_all_in_flight_submissions(
        &mut self,
        completion: GpuSubmissionCompletion,
    ) -> Vec<CompletedSubmission> {
        self.in_flight_submissions
            .drain(..)
            .map(|in_flight| CompletedSubmission {
                renderer_submission_id: in_flight.renderer_submission_id,
                frame_id: in_flight.frame_id,
                completion: completion.clone(),
            })
            .collect()
    }

    fn has_in_flight_submissions(&self) -> bool {
        !self.in_flight_submissions.is_empty()
    }

    fn submission_receipt_ids(
        &self,
        renderer_submission_id: RendererSubmissionId,
    ) -> Result<Vec<StrokeExecutionReceiptId>, MergePollError> {
        let submission = self.submissions.get(&renderer_submission_id).ok_or(
            MergePollError::UnknownSubmission {
                renderer_submission_id,
            },
        )?;
        let _ = submission.frame_id;
        Ok(submission.receipt_ids.clone())
    }

    fn collect_completion_notices(
        &mut self,
        completed_submissions: Vec<CompletedSubmission>,
    ) -> Result<Vec<MergeCompletionNotice>, MergePollError> {
        let mut notices = Vec::new();
        for completed in completed_submissions {
            let receipt_ids = self.submission_receipt_ids(completed.renderer_submission_id)?;
            for receipt_id in receipt_ids {
                let result = match &completed.completion {
                    GpuSubmissionCompletion::Completed => MergeExecutionResult::Succeeded,
                    GpuSubmissionCompletion::CallbackDisconnected => MergeExecutionResult::Failed {
                        message: "merge submission callback disconnected".to_owned(),
                    },
                    GpuSubmissionCompletion::DeviceError { message } => {
                        MergeExecutionResult::Failed {
                            message: message.clone(),
                        }
                    }
                    GpuSubmissionCompletion::Lost { reason, message } => {
                        MergeExecutionResult::Failed {
                            message: format!("gpu device lost ({reason:?}): {message}"),
                        }
                    }
                };
                let notice = MergeCompletionNotice {
                    receipt_id,
                    audit_meta: MergeAuditMeta {
                        frame_id: completed.frame_id,
                        renderer_submission_id: completed.renderer_submission_id,
                        op_trace_id: None,
                    },
                    result,
                };
                if self.ackable_notices.contains_key(&receipt_id) {
                    return Err(MergePollError::DuplicateAckableNotice { receipt_id });
                }
                self.ackable_notices.insert(receipt_id, notice.clone());
                notices.push(notice);
            }
        }
        Ok(notices)
    }

    pub(crate) fn ack_merge_result(
        &mut self,
        notice: MergeCompletionNotice,
    ) -> Result<(), MergeAckError> {
        let receipt_id = notice.receipt_id;
        let Some(registered_notice) = self.ackable_notices.get(&receipt_id) else {
            return Err(MergeAckError::NoticeNotAckable { receipt_id });
        };
        if registered_notice != &notice {
            return Err(MergeAckError::NoticeMismatch { receipt_id });
        }

        let submission = self
            .submissions
            .get(&notice.audit_meta.renderer_submission_id)
            .ok_or(MergeAckError::UnknownSubmission {
                receipt_id,
                renderer_submission_id: notice.audit_meta.renderer_submission_id,
            })?;
        if !submission.receipt_ids.contains(&receipt_id) {
            return Err(MergeAckError::ReceiptNotInSubmission {
                receipt_id,
                renderer_submission_id: notice.audit_meta.renderer_submission_id,
            });
        }

        let entry = self
            .receipts
            .get_mut(&receipt_id)
            .ok_or(MergeAckError::UnknownReceipt { receipt_id })?;

        let state_submission_id = match &entry.state {
            MergeReceiptState::Pending {
                renderer_submission_id,
                frame_id,
            } => {
                let _ = frame_id;
                *renderer_submission_id
            }
            MergeReceiptState::Submitted {
                renderer_submission_id,
                frame_id,
            } => {
                let _ = frame_id;
                *renderer_submission_id
            }
            _ => {
                return Err(MergeAckError::IllegalState {
                    receipt_id,
                    state: entry.state.label(),
                });
            }
        };
        if state_submission_id != notice.audit_meta.renderer_submission_id {
            return Err(MergeAckError::ReceiptSubmissionMismatch {
                receipt_id,
                expected_submission_id: state_submission_id,
                received_submission_id: notice.audit_meta.renderer_submission_id,
            });
        }

        match notice.result {
            MergeExecutionResult::Succeeded => {
                entry.state = MergeReceiptState::Succeeded;
                self.succeeded_receipts.push_back(entry.receipt.clone());
                self.progress_queue.push_back(ReceiptProgress {
                    receipt: entry.receipt.clone(),
                    status: ExecutionStatus::Succeeded,
                });
            }
            MergeExecutionResult::Failed { message } => {
                let error_ctx = MergeErrorContext {
                    receipt_id,
                    stroke_session_id: entry.receipt.stroke_session_id,
                    tx_token: entry.receipt.tx_token,
                    frame_id: notice.audit_meta.frame_id,
                    renderer_submission_id: notice.audit_meta.renderer_submission_id,
                    op_stage: MergeOpStage::Merge,
                    message,
                };
                let failure = StrokeExecutionFailure {
                    receipt: entry.receipt.clone(),
                    error_ctx: error_ctx.clone(),
                };
                entry.state = MergeReceiptState::Failed;
                self.failed_receipts.push_back(failure);
                self.progress_queue.push_back(ReceiptProgress {
                    receipt: entry.receipt.clone(),
                    status: ExecutionStatus::Failed(error_ctx),
                });
            }
        }

        self.ackable_notices.remove(&receipt_id);

        Ok(())
    }

    pub(crate) fn ack_receipt_terminal_state(
        &mut self,
        receipt_id: StrokeExecutionReceiptId,
        terminal_state: ReceiptTerminalState,
    ) -> Result<(), MergeFinalizeError> {
        if let Some(renderer_submission_id) = self.receipt_submission_id(receipt_id) {
            if self.is_submission_in_flight(renderer_submission_id) {
                return Err(MergeFinalizeError::SubmissionStillInFlight {
                    receipt_id,
                    renderer_submission_id,
                });
            }
        }

        let state = self
            .receipts
            .get(&receipt_id)
            .ok_or(MergeFinalizeError::UnknownReceipt { receipt_id })?
            .state
            .clone();
        match terminal_state {
            ReceiptTerminalState::Finalized => match &state {
                MergeReceiptState::Succeeded => {
                    self.receipts.remove(&receipt_id);
                    self.prune_submission_if_terminal(receipt_id);
                    Ok(())
                }
                _ => Err(MergeFinalizeError::IllegalTransition {
                    receipt_id,
                    state: state.label(),
                    terminal_state,
                }),
            },
            ReceiptTerminalState::Aborted => match &state {
                MergeReceiptState::Failed => {
                    self.receipts.remove(&receipt_id);
                    self.prune_submission_if_terminal(receipt_id);
                    Ok(())
                }
                _ => Err(MergeFinalizeError::IllegalTransition {
                    receipt_id,
                    state: state.label(),
                    terminal_state,
                }),
            },
        }
    }

    fn prune_submission_if_terminal(&mut self, receipt_id: StrokeExecutionReceiptId) {
        let Some(renderer_submission_id) = self.receipt_submission_id(receipt_id) else {
            return;
        };
        let should_remove_submission = self
            .submissions
            .get(&renderer_submission_id)
            .map(|submission| {
                submission
                    .receipt_ids
                    .iter()
                    .all(|id| !self.receipts.contains_key(id))
            })
            .unwrap_or(false);
        if should_remove_submission {
            self.submissions.remove(&renderer_submission_id);
        }
    }

    fn is_submission_in_flight(&self, renderer_submission_id: RendererSubmissionId) -> bool {
        self.in_flight_submissions
            .iter()
            .any(|entry| entry.renderer_submission_id == renderer_submission_id)
    }

    fn receipt_submission_id(
        &self,
        receipt_id: StrokeExecutionReceiptId,
    ) -> Option<RendererSubmissionId> {
        self.submissions
            .iter()
            .find_map(|(submission_id, submission)| {
                submission
                    .receipt_ids
                    .contains(&receipt_id)
                    .then_some(*submission_id)
            })
    }

    pub(crate) fn take_succeeded_receipts(&mut self) -> Vec<StrokeExecutionReceipt> {
        self.succeeded_receipts.drain(..).collect()
    }

    pub(crate) fn take_failed_receipts(&mut self) -> Vec<StrokeExecutionFailure> {
        self.failed_receipts.drain(..).collect()
    }
}

impl Renderer {
    pub fn enqueue_planned_merge(
        &mut self,
        receipt: StrokeExecutionReceipt,
        gpu_merge_ops: Vec<GpuMergeOp>,
        meta: MergePlanMeta,
    ) -> Result<(), MergeEnqueueError> {
        self.merge_orchestrator
            .enqueue_planned_merge(receipt, gpu_merge_ops, meta)
    }

    pub fn submit_pending_merges(
        &mut self,
        frame_id: u64,
        budget: u32,
    ) -> Result<SubmissionReport, MergeSubmitError> {
        let prepared = self
            .merge_orchestrator
            .prepare_pending_submission(frame_id, budget)?;
        let Some(renderer_submission_id) = prepared.report.renderer_submission_id else {
            return Ok(prepared.report);
        };
        let submission_receipt_count = prepared.report.receipt_ids.len();
        let submission_op_count = prepared.submission_gpu_ops.len();
        let submit_started = Instant::now();

        self.encode_merge_submission(renderer_submission_id, &prepared.submission_gpu_ops)?;

        let (done_sender, done_receiver) = mpsc::channel();
        self.gpu_state.queue.on_submitted_work_done(move || {
            if let Err(error) = done_sender.send(()) {
                eprintln!("merge completion callback channel send failed: {error}");
            }
        });
        if crate::renderer_perf_log_enabled() {
            eprintln!(
                "[renderer_perf] merge_submit frame_id={} submission_id={} receipts={} ops={} cpu_submit_ms={:.3}",
                frame_id,
                renderer_submission_id.0,
                submission_receipt_count,
                submission_op_count,
                submit_started.elapsed().as_secs_f64() * 1_000.0,
            );
        }
        Ok(self
            .merge_orchestrator
            .commit_submitted_submission(prepared, done_receiver))
    }

    pub fn poll_completion_notices(
        &mut self,
        frame_id: u64,
    ) -> Result<Vec<MergeCompletionNotice>, MergePollError> {
        let poll_started = Instant::now();
        let mut completed_submissions = Vec::new();
        if let Err(error) = self.gpu_state.device.poll(wgpu::PollType::Poll) {
            completed_submissions.extend(self.merge_orchestrator.fail_all_in_flight_submissions(
                GpuSubmissionCompletion::DeviceError {
                    message: format!("device poll failed on frame {frame_id}: {error}",),
                },
            ));
        }

        while let Ok((reason, message)) = self.gpu_state.merge_device_lost_receiver.try_recv() {
            completed_submissions.extend(
                self.merge_orchestrator.fail_all_in_flight_submissions(
                    GpuSubmissionCompletion::Lost { reason, message },
                ),
            );
        }

        let mut uncaptured_messages = Vec::new();
        while let Ok(message) = self.gpu_state.merge_uncaptured_error_receiver.try_recv() {
            uncaptured_messages.push(message);
        }
        if !uncaptured_messages.is_empty() && crate::renderer_brush_trace_enabled() {
            eprintln!(
                "[brush_trace] merge_poll observed_uncaptured_gpu_errors frame_id={} in_flight_merge_submissions={} messages={}",
                frame_id,
                self.merge_orchestrator.has_in_flight_submissions(),
                uncaptured_messages.join(" | ")
            );
        }
        if !uncaptured_messages.is_empty() && self.merge_orchestrator.has_in_flight_submissions() {
            if crate::renderer_brush_trace_enabled() {
                eprintln!(
                    "[brush_trace] merge_poll failing in-flight merges due to uncaptured gpu error; error source may be outside merge submission"
                );
            }
            completed_submissions.extend(self.merge_orchestrator.fail_all_in_flight_submissions(
                GpuSubmissionCompletion::DeviceError {
                    message: format!(
                        "uncaptured gpu error while merge submissions were in flight on frame {frame_id}: {}",
                        uncaptured_messages.join(" | "),
                    ),
                },
            ));
        }

        completed_submissions.extend(self.merge_orchestrator.drain_completed_submissions());

        let notices = self
            .merge_orchestrator
            .collect_completion_notices(completed_submissions)?;
        if crate::renderer_perf_log_enabled() && !notices.is_empty() {
            let success_count = notices
                .iter()
                .filter(|notice| matches!(notice.result, MergeExecutionResult::Succeeded))
                .count();
            let failure_count = notices.len().saturating_sub(success_count);
            eprintln!(
                "[renderer_perf] merge_poll frame_id={} notices={} success={} failure={} cpu_poll_ms={:.3}",
                frame_id,
                notices.len(),
                success_count,
                failure_count,
                poll_started.elapsed().as_secs_f64() * 1_000.0,
            );
        }
        Ok(notices)
    }

    pub fn ack_merge_result(&mut self, notice: MergeCompletionNotice) -> Result<(), MergeAckError> {
        self.merge_orchestrator.ack_merge_result(notice)
    }

    pub fn drain_receipt_progress_events(&mut self, frame_id: u64) -> Vec<ReceiptProgress> {
        self.merge_orchestrator
            .drain_receipt_progress_events(frame_id)
    }

    pub fn ack_receipt_terminal_state(
        &mut self,
        receipt_id: StrokeExecutionReceiptId,
        terminal_state: ReceiptTerminalState,
    ) -> Result<(), MergeFinalizeError> {
        self.merge_orchestrator
            .ack_receipt_terminal_state(receipt_id, terminal_state)
    }

    pub fn take_succeeded_receipts(&mut self) -> Vec<StrokeExecutionReceipt> {
        self.merge_orchestrator.take_succeeded_receipts()
    }

    pub fn take_failed_receipts(&mut self) -> Vec<StrokeExecutionFailure> {
        self.merge_orchestrator.take_failed_receipts()
    }

    fn encode_merge_submission(
        &mut self,
        renderer_submission_id: RendererSubmissionId,
        gpu_merge_ops: &[SubmissionGpuOp],
    ) -> Result<(), MergeSubmitError> {
        if gpu_merge_ops.is_empty() {
            return Err(MergeSubmitError::InvalidGpuMergeOp {
                renderer_submission_id,
                receipt_id: None,
                op_trace_id: 0,
                reason: "empty merge op batch",
                tile_ref: None,
                tile_ref_role: None,
            });
        }
        let layer_atlas_layout = self.gpu_state.tile_atlas.layout();
        let layer_atlas_layer_count = self
            .gpu_state
            .tile_atlas
            .texture()
            .size()
            .depth_or_array_layers;
        let stroke_atlas_layout = self.gpu_state.brush_buffer_atlas.layout();
        let stroke_atlas_layer_count = self
            .gpu_state
            .brush_buffer_atlas
            .texture()
            .size()
            .depth_or_array_layers;
        if layer_atlas_layout != stroke_atlas_layout {
            return Err(MergeSubmitError::InvalidGpuMergeOp {
                renderer_submission_id,
                receipt_id: None,
                op_trace_id: 0,
                reason: "merge requires matching layer/stroke atlas layouts",
                tile_ref: None,
                tile_ref_role: None,
            });
        }
        let slot_uv_size = [
            TILE_STRIDE as f32 / layer_atlas_layout.atlas_width as f32,
            TILE_STRIDE as f32 / layer_atlas_layout.atlas_height as f32,
        ];
        for (op_index, submission_gpu_op) in gpu_merge_ops.iter().enumerate() {
            let receipt_id = submission_gpu_op.receipt_id;
            let gpu_merge_op = submission_gpu_op.gpu_merge_op;
            if !(0.0..=1.0).contains(&gpu_merge_op.opacity) {
                return Err(MergeSubmitError::InvalidGpuMergeOp {
                    renderer_submission_id,
                    receipt_id: Some(receipt_id),
                    op_trace_id: gpu_merge_op.op_trace_id,
                    reason: "merge opacity must be within [0, 1]",
                    tile_ref: None,
                    tile_ref_role: None,
                });
            }
            if !validate_tile_ref(
                gpu_merge_op.stroke_tile,
                stroke_atlas_layout,
                stroke_atlas_layer_count,
            ) {
                return Err(MergeSubmitError::InvalidGpuMergeOp {
                    renderer_submission_id,
                    receipt_id: Some(receipt_id),
                    op_trace_id: gpu_merge_op.op_trace_id,
                    reason: "merge tile reference is out of atlas bounds",
                    tile_ref: Some(gpu_merge_op.stroke_tile),
                    tile_ref_role: Some(MergeTileRefRole::Stroke),
                });
            }
            if !validate_tile_ref(
                gpu_merge_op.output_tile,
                layer_atlas_layout,
                layer_atlas_layer_count,
            ) {
                return Err(MergeSubmitError::InvalidGpuMergeOp {
                    renderer_submission_id,
                    receipt_id: Some(receipt_id),
                    op_trace_id: gpu_merge_op.op_trace_id,
                    reason: "merge tile reference is out of atlas bounds",
                    tile_ref: Some(gpu_merge_op.output_tile),
                    tile_ref_role: Some(MergeTileRefRole::Output),
                });
            }
            if let Some(base_tile) = gpu_merge_op.base_tile {
                if !validate_tile_ref(base_tile, layer_atlas_layout, layer_atlas_layer_count) {
                    return Err(MergeSubmitError::InvalidGpuMergeOp {
                        renderer_submission_id,
                        receipt_id: Some(receipt_id),
                        op_trace_id: gpu_merge_op.op_trace_id,
                        reason: "merge tile reference is out of atlas bounds",
                        tile_ref: Some(base_tile),
                        tile_ref_role: Some(MergeTileRefRole::Base),
                    });
                }
            }

            if layer_atlas_layer_count == 0 || stroke_atlas_layer_count == 0 {
                return Err(MergeSubmitError::InvalidGpuMergeOp {
                    renderer_submission_id,
                    receipt_id: Some(receipt_id),
                    op_trace_id: gpu_merge_op.op_trace_id,
                    reason: "merge atlas has zero array layers",
                    tile_ref: None,
                    tile_ref_role: None,
                });
            }
            let merge_uniform = MergeUniformGpu {
                base_slot_origin_uv: gpu_merge_op.base_tile.map_or([0.0, 0.0], |base_tile| {
                    tile_ref_slot_origin_uv(base_tile, layer_atlas_layout)
                }),
                stroke_slot_origin_uv: tile_ref_slot_origin_uv(
                    gpu_merge_op.stroke_tile,
                    layer_atlas_layout,
                ),
                slot_uv_size,
                base_layer: gpu_merge_op
                    .base_tile
                    .map_or(0.0, |tile| tile.atlas_layer as f32),
                stroke_layer: gpu_merge_op.stroke_tile.atlas_layer as f32,
                has_base: if gpu_merge_op.base_tile.is_some() {
                    1.0
                } else {
                    0.0
                },
                opacity: gpu_merge_op.opacity,
                blend_mode: blend_mode_to_u32(gpu_merge_op.blend_mode),
                _padding0: [0, 0, 0],
            };
            self.gpu_state.queue.write_buffer(
                &self.gpu_state.merge_uniform_buffer,
                0,
                bytemuck::bytes_of(&merge_uniform),
            );
            if crate::renderer_brush_trace_enabled() && op_index < 6 {
                eprintln!(
                    "[brush_trace] merge_encode_op submission_id={} op_index={} op_trace_id={} base={:?} stroke={:?} output={:?}",
                    renderer_submission_id.0,
                    op_index,
                    gpu_merge_op.op_trace_id,
                    gpu_merge_op.base_tile,
                    gpu_merge_op.stroke_tile,
                    gpu_merge_op.output_tile,
                );
            }

            let mut encoder =
                self.gpu_state
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("renderer.merge_submission.op"),
                    });

            {
                let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("renderer.merge_submission.pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.gpu_state.merge_scratch_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                render_pass.set_pipeline(&self.gpu_state.merge_pipeline);
                render_pass.set_bind_group(0, &self.gpu_state.merge_bind_group, &[]);
                render_pass.draw(0..3, 0..1);
            }

            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.gpu_state._merge_scratch_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: self.gpu_state.tile_atlas.texture(),
                    mip_level: 0,
                    origin: tile_ref_slot_origin(gpu_merge_op.output_tile, layer_atlas_layout),
                    aspect: wgpu::TextureAspect::All,
                },
                merge_output_copy_extent(),
            );
            self.gpu_state.queue.submit(Some(encoder.finish()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(id: u64) -> StrokeExecutionReceipt {
        StrokeExecutionReceipt {
            receipt_id: StrokeExecutionReceiptId(id),
            stroke_session_id: id * 10,
            tx_token: id * 100,
            program_revision: Some(7),
        }
    }

    fn merge_ops() -> Vec<GpuMergeOp> {
        vec![GpuMergeOp {
            base_tile: None,
            stroke_tile: GpuTileRef {
                atlas_layer: 0,
                tile_index: 0,
            },
            output_tile: GpuTileRef {
                atlas_layer: 0,
                tile_index: 1,
            },
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            op_trace_id: 1,
        }]
    }

    #[test]
    fn merge_output_copy_extent_must_cover_tile_stride_to_refresh_gutter() {
        let extent = merge_output_copy_extent();
        assert_eq!(
            extent.width, TILE_STRIDE,
            "merge writeback must copy full tile slot width including gutter"
        );
        assert_eq!(
            extent.height, TILE_STRIDE,
            "merge writeback must copy full tile slot height including gutter"
        );
        assert_eq!(extent.depth_or_array_layers, 1);
    }

    fn commit_submission_for_test(
        orchestrator: &mut MergeOrchestrator,
        frame_id: u64,
        budget: u32,
    ) -> (SubmissionReport, mpsc::Sender<()>) {
        let prepared = orchestrator
            .prepare_pending_submission(frame_id, budget)
            .expect("prepare pending submission");
        let report = prepared.report.clone();
        let (done_sender, done_receiver) = mpsc::channel();
        orchestrator.commit_submitted_submission(prepared, done_receiver);
        (report, done_sender)
    }

    fn complete_and_collect_single_notice(
        orchestrator: &mut MergeOrchestrator,
        done_sender: mpsc::Sender<()>,
    ) -> MergeCompletionNotice {
        done_sender
            .send(())
            .expect("mark submission completed for notice collection");
        let completed_submissions = orchestrator.drain_completed_submissions();
        let notices = orchestrator
            .collect_completion_notices(completed_submissions)
            .expect("collect completion notices");
        assert_eq!(notices.len(), 1);
        notices[0].clone()
    }

    #[test]
    fn prepare_submission_is_side_effect_free_until_commit() {
        let mut orchestrator = MergeOrchestrator::default();
        let receipt = receipt(9);
        orchestrator
            .enqueue_planned_merge(
                receipt.clone(),
                merge_ops(),
                MergePlanMeta {
                    stroke_session_id: receipt.stroke_session_id,
                    tx_token: receipt.tx_token,
                    program_revision: receipt.program_revision,
                },
            )
            .expect("enqueue planned merge");

        let prepared = orchestrator
            .prepare_pending_submission(21, 1)
            .expect("prepare pending submission");
        assert_eq!(prepared.report.receipt_ids, vec![receipt.receipt_id]);

        let state = orchestrator
            .receipts
            .get(&receipt.receipt_id)
            .expect("receipt must exist")
            .state
            .label();
        assert_eq!(state, "Planned");
        assert_eq!(orchestrator.pending_receipts.len(), 1);
        assert!(orchestrator.submissions.is_empty());
        assert!(orchestrator.in_flight_submissions.is_empty());

        let (done_sender, done_receiver) = mpsc::channel();
        orchestrator.commit_submitted_submission(prepared, done_receiver);
        done_sender.send(()).expect("send completion");

        let state = orchestrator
            .receipts
            .get(&receipt.receipt_id)
            .expect("receipt must exist")
            .state
            .label();
        assert_eq!(state, "Pending");
        assert!(orchestrator.pending_receipts.is_empty());
    }

    #[test]
    fn submit_and_ack_success_flow_reaches_finalized() {
        let mut orchestrator = MergeOrchestrator::default();
        let receipt = receipt(1);
        orchestrator
            .enqueue_planned_merge(
                receipt.clone(),
                merge_ops(),
                MergePlanMeta {
                    stroke_session_id: receipt.stroke_session_id,
                    tx_token: receipt.tx_token,
                    program_revision: receipt.program_revision,
                },
            )
            .expect("enqueue planned merge");

        let (report, done_sender) = commit_submission_for_test(&mut orchestrator, 10, 1);
        assert_eq!(report.receipt_ids, vec![receipt.receipt_id]);
        report
            .renderer_submission_id
            .expect("submission id must be assigned");

        let progress = orchestrator.drain_receipt_progress_events(10);
        assert_eq!(progress.len(), 1);
        assert!(matches!(progress[0].status, ExecutionStatus::Pending));

        let notice = complete_and_collect_single_notice(&mut orchestrator, done_sender);

        orchestrator.ack_merge_result(notice).expect("ack success");

        let progress = orchestrator.drain_receipt_progress_events(11);
        assert_eq!(progress.len(), 1);
        assert!(matches!(progress[0].status, ExecutionStatus::Succeeded));
        assert_eq!(
            orchestrator.take_succeeded_receipts(),
            vec![receipt.clone()]
        );

        orchestrator
            .ack_receipt_terminal_state(receipt.receipt_id, ReceiptTerminalState::Finalized)
            .expect("finalize receipt");
        assert!(orchestrator.receipts.is_empty());
        assert!(orchestrator.submissions.is_empty());
    }

    #[test]
    fn duplicate_ack_is_rejected() {
        let mut orchestrator = MergeOrchestrator::default();
        let receipt = receipt(2);
        orchestrator
            .enqueue_planned_merge(
                receipt.clone(),
                merge_ops(),
                MergePlanMeta {
                    stroke_session_id: receipt.stroke_session_id,
                    tx_token: receipt.tx_token,
                    program_revision: receipt.program_revision,
                },
            )
            .expect("enqueue planned merge");
        let (report, done_sender) = commit_submission_for_test(&mut orchestrator, 12, 1);
        report
            .renderer_submission_id
            .expect("submission id must be assigned");
        let notice = complete_and_collect_single_notice(&mut orchestrator, done_sender);

        orchestrator
            .ack_merge_result(notice.clone())
            .expect("first ack success");
        let error = orchestrator
            .ack_merge_result(notice)
            .expect_err("duplicate ack must fail fast");
        assert!(matches!(error, MergeAckError::NoticeNotAckable { .. }));
    }

    #[test]
    fn completion_notice_does_not_implicitly_ack_receipt() {
        let mut orchestrator = MergeOrchestrator::default();
        let receipt = receipt(4);
        orchestrator
            .enqueue_planned_merge(
                receipt.clone(),
                merge_ops(),
                MergePlanMeta {
                    stroke_session_id: receipt.stroke_session_id,
                    tx_token: receipt.tx_token,
                    program_revision: receipt.program_revision,
                },
            )
            .expect("enqueue planned merge");
        let (report, done_sender) = commit_submission_for_test(&mut orchestrator, 14, 1);
        report
            .renderer_submission_id
            .expect("submission id must be assigned");
        done_sender.send(()).expect("mark submission completed");

        let completed_submissions = orchestrator.drain_completed_submissions();
        let notices = orchestrator
            .collect_completion_notices(completed_submissions)
            .expect("collect completion notices");
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].receipt_id, receipt.receipt_id);
        assert!(matches!(notices[0].result, MergeExecutionResult::Succeeded));

        orchestrator
            .ack_merge_result(notices[0].clone())
            .expect("manual ack must succeed");
    }

    #[test]
    fn forged_ack_without_polled_notice_is_rejected() {
        let mut orchestrator = MergeOrchestrator::default();
        let receipt = receipt(5);
        orchestrator
            .enqueue_planned_merge(
                receipt.clone(),
                merge_ops(),
                MergePlanMeta {
                    stroke_session_id: receipt.stroke_session_id,
                    tx_token: receipt.tx_token,
                    program_revision: receipt.program_revision,
                },
            )
            .expect("enqueue planned merge");
        let (report, _done_sender) = commit_submission_for_test(&mut orchestrator, 15, 1);
        let submission_id = report
            .renderer_submission_id
            .expect("submission id must be assigned");

        let error = orchestrator
            .ack_merge_result(MergeCompletionNotice {
                receipt_id: receipt.receipt_id,
                result: MergeExecutionResult::Succeeded,
                audit_meta: MergeAuditMeta {
                    frame_id: 15,
                    renderer_submission_id: submission_id,
                    op_trace_id: None,
                },
            })
            .expect_err("forged ack must fail fast");
        assert!(matches!(error, MergeAckError::NoticeNotAckable { .. }));
    }

    #[test]
    fn terminal_ack_is_rejected_while_submission_in_flight() {
        let mut orchestrator = MergeOrchestrator::default();
        let receipt = receipt(6);
        orchestrator
            .enqueue_planned_merge(
                receipt.clone(),
                merge_ops(),
                MergePlanMeta {
                    stroke_session_id: receipt.stroke_session_id,
                    tx_token: receipt.tx_token,
                    program_revision: receipt.program_revision,
                },
            )
            .expect("enqueue planned merge");
        let (_report, done_sender) = commit_submission_for_test(&mut orchestrator, 16, 1);
        let finalize_error = orchestrator
            .ack_receipt_terminal_state(receipt.receipt_id, ReceiptTerminalState::Finalized)
            .expect_err("terminal ack must fail while submission is in flight");
        assert!(matches!(
            finalize_error,
            MergeFinalizeError::SubmissionStillInFlight { .. }
        ));

        let notice = complete_and_collect_single_notice(&mut orchestrator, done_sender);
        orchestrator
            .ack_merge_result(notice)
            .expect("ack succeeds after completion notice");
        orchestrator
            .ack_receipt_terminal_state(receipt.receipt_id, ReceiptTerminalState::Finalized)
            .expect("finalize succeeds after ack and completion");
    }

    #[test]
    fn failed_receipt_can_abort_and_cannot_finalize() {
        let mut orchestrator = MergeOrchestrator::default();
        let receipt = receipt(3);
        orchestrator
            .enqueue_planned_merge(
                receipt.clone(),
                merge_ops(),
                MergePlanMeta {
                    stroke_session_id: receipt.stroke_session_id,
                    tx_token: receipt.tx_token,
                    program_revision: receipt.program_revision,
                },
            )
            .expect("enqueue planned merge");
        let (report, _done_sender) = commit_submission_for_test(&mut orchestrator, 13, 1);
        report
            .renderer_submission_id
            .expect("submission id must be assigned");
        let completed_submissions =
            orchestrator.fail_all_in_flight_submissions(GpuSubmissionCompletion::DeviceError {
                message: "gpu merge failed".to_owned(),
            });
        let notices = orchestrator
            .collect_completion_notices(completed_submissions)
            .expect("collect failed completion notices");
        assert_eq!(notices.len(), 1);
        let notice = notices[0].clone();

        orchestrator
            .ack_merge_result(notice)
            .expect("ack failed result");

        let failures = orchestrator.take_failed_receipts();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].receipt.receipt_id, receipt.receipt_id);

        let finalize_error = orchestrator
            .ack_receipt_terminal_state(receipt.receipt_id, ReceiptTerminalState::Finalized)
            .expect_err("failed receipt cannot finalize");
        assert!(matches!(
            finalize_error,
            MergeFinalizeError::IllegalTransition { .. }
        ));

        orchestrator
            .ack_receipt_terminal_state(receipt.receipt_id, ReceiptTerminalState::Aborted)
            .expect("abort failed receipt");
        assert!(orchestrator.receipts.is_empty());
        assert!(orchestrator.submissions.is_empty());
    }
}
