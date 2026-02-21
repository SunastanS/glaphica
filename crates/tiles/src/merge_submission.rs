use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use render_protocol::{
    BlendMode, GpuMergeOp, GpuTileRef, LayerId, MergeAuditMeta, MergeExecutionResult,
    MergePlanMeta, ProgramRevision, StrokeExecutionReceipt, StrokeExecutionReceiptId,
    StrokeSessionId, TxToken,
};

use crate::{
    TileAddress, TileAllocError, TileAtlasStore, TileKey, TileMergeCompletionCallback,
    TileMergeCompletionNotice, TileMergeCompletionNoticeId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptState {
    Pending,
    Succeeded,
    Failed,
    Finalized,
    Aborted,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MergePlanTileOp {
    pub tile_x: u32,
    pub tile_y: u32,
    pub existing_layer_key: Option<TileKey>,
    pub stroke_buffer_key: TileKey,
    pub blend_mode: BlendMode,
    pub opacity: f32,
    pub op_trace_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileKeyMapping {
    pub tile_x: u32,
    pub tile_y: u32,
    pub layer_id: LayerId,
    pub previous_key: Option<TileKey>,
    pub new_key: TileKey,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MergePlanRequest {
    pub stroke_session_id: StrokeSessionId,
    pub tx_token: TxToken,
    pub program_revision: Option<ProgramRevision>,
    pub layer_id: LayerId,
    pub tile_ops: Vec<MergePlanTileOp>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RendererSubmitPayload {
    pub receipt: StrokeExecutionReceipt,
    pub gpu_merge_ops: Vec<GpuMergeOp>,
    pub meta: MergePlanMeta,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MergeSubmission {
    pub receipt_id: StrokeExecutionReceiptId,
    pub new_key_mappings: Vec<TileKeyMapping>,
    pub drop_key_list: Vec<TileKey>,
    pub renderer_submit_payload: RendererSubmitPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AckOutcome {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TilesBusinessResult {
    CanFinalize {
        receipt_id: StrokeExecutionReceiptId,
        stroke_session_id: StrokeSessionId,
        layer_id: LayerId,
        new_key_mappings: Vec<TileKeyMapping>,
        drop_key_list: Vec<TileKey>,
    },
    RequiresAbort {
        receipt_id: StrokeExecutionReceiptId,
        stroke_session_id: StrokeSessionId,
        layer_id: LayerId,
        new_key_mappings: Vec<TileKeyMapping>,
        drop_key_list: Vec<TileKey>,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeCompletionAuditRecord {
    pub notice_id: TileMergeCompletionNoticeId,
    pub audit_meta: MergeAuditMeta,
    pub result: MergeExecutionResult,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeAuditRecord {
    pub receipt_id: StrokeExecutionReceiptId,
    pub stroke_session_id: StrokeSessionId,
    pub tx_token: TxToken,
    pub program_revision: Option<ProgramRevision>,
    pub layer_id: LayerId,
    pub receipt_state: ReceiptState,
    pub completion: Option<MergeCompletionAuditRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TileMergeError {
    EmptyMergePlan {
        stroke_session_id: StrokeSessionId,
        layer_id: LayerId,
    },
    ReentrantDownstreamSubmission,
    ReceiptIdSpaceExhausted,
    TileAlloc {
        stroke_session_id: StrokeSessionId,
        layer_id: LayerId,
        source: TileAllocError,
    },
    DuplicateTxToken {
        stroke_session_id: StrokeSessionId,
        tx_token: TxToken,
        existing_receipt_id: StrokeExecutionReceiptId,
    },
    SharedPreviousKeyNotAllowed {
        stroke_session_id: StrokeSessionId,
        layer_id: LayerId,
        previous_key: TileKey,
        first_tile_x: u32,
        first_tile_y: u32,
        duplicate_tile_x: u32,
        duplicate_tile_y: u32,
    },
    UnknownTileKey {
        receipt_id: Option<StrokeExecutionReceiptId>,
        stroke_session_id: StrokeSessionId,
        layer_id: LayerId,
        key: TileKey,
        stage: &'static str,
    },
    UnknownReceipt {
        receipt_id: StrokeExecutionReceiptId,
    },
    UnknownCompletionNotice {
        receipt_id: StrokeExecutionReceiptId,
        notice_id: TileMergeCompletionNoticeId,
    },
    DuplicateCompletionNotice {
        receipt_id: StrokeExecutionReceiptId,
        notice_id: TileMergeCompletionNoticeId,
    },
    NoticeReceiptMismatch {
        expected_receipt_id: StrokeExecutionReceiptId,
        notice_receipt_id: StrokeExecutionReceiptId,
        notice_id: TileMergeCompletionNoticeId,
    },
    IllegalStateTransition {
        receipt_id: StrokeExecutionReceiptId,
        stroke_session_id: StrokeSessionId,
        layer_id: LayerId,
        from: ReceiptState,
        to: ReceiptState,
    },
}

pub trait MergeTileStore {
    fn allocate(&self) -> Result<TileKey, TileAllocError>;
    fn release(&self, key: TileKey) -> bool;
    fn resolve(&self, key: TileKey) -> Option<TileAddress>;
    fn mark_keys_active(&self, keys: &[TileKey]);
    fn retain_keys(&self, retain_id: u64, keys: &[TileKey]);
}

impl MergeTileStore for TileAtlasStore {
    fn allocate(&self) -> Result<TileKey, TileAllocError> {
        TileAtlasStore::allocate(self)
    }

    fn release(&self, key: TileKey) -> bool {
        TileAtlasStore::release(self, key)
    }

    fn resolve(&self, key: TileKey) -> Option<TileAddress> {
        TileAtlasStore::resolve(self, key)
    }

    fn mark_keys_active(&self, keys: &[TileKey]) {
        TileAtlasStore::mark_keys_active(self, keys);
    }

    fn retain_keys(&self, retain_id: u64, keys: &[TileKey]) {
        TileAtlasStore::retain_keys(self, retain_id, keys);
    }
}

impl MergeTileStore for Arc<TileAtlasStore> {
    fn allocate(&self) -> Result<TileKey, TileAllocError> {
        TileAtlasStore::allocate(self)
    }

    fn release(&self, key: TileKey) -> bool {
        TileAtlasStore::release(self, key)
    }

    fn resolve(&self, key: TileKey) -> Option<TileAddress> {
        TileAtlasStore::resolve(self, key)
    }

    fn mark_keys_active(&self, keys: &[TileKey]) {
        TileAtlasStore::mark_keys_active(self, keys);
    }

    fn retain_keys(&self, retain_id: u64, keys: &[TileKey]) {
        TileAtlasStore::retain_keys(self, retain_id, keys);
    }
}

#[derive(Debug)]
struct ReceiptEntry {
    stroke_session_id: StrokeSessionId,
    tx_token: TxToken,
    program_revision: Option<ProgramRevision>,
    layer_id: LayerId,
    state: ReceiptState,
    completion: Option<MergeCompletionAuditRecord>,
    new_key_mappings: Vec<TileKeyMapping>,
    drop_key_list: Vec<TileKey>,
}

#[derive(Debug)]
pub struct TileMergeEngine<S: MergeTileStore> {
    store: S,
    next_receipt_id: AtomicU64,
    submitted_tokens: HashMap<SubmissionKey, StrokeExecutionReceiptId>,
    receipts: HashMap<StrokeExecutionReceiptId, ReceiptEntry>,
    ackable_notices: HashMap<NoticeKey, TileMergeCompletionNotice>,
    consumed_notices: HashSet<NoticeKey>,
    completion_notice_queue: VecDeque<TileMergeCompletionNotice>,
    business_results: VecDeque<TilesBusinessResult>,
    upstream_phase_active: bool,
}

type NoticeKey = (TileMergeCompletionNoticeId, StrokeExecutionReceiptId);
type SubmissionKey = (StrokeSessionId, TxToken);

impl<S: MergeTileStore> TileMergeEngine<S> {
    pub fn new(store: S) -> Self {
        Self {
            store,
            next_receipt_id: AtomicU64::new(0),
            submitted_tokens: HashMap::new(),
            receipts: HashMap::new(),
            ackable_notices: HashMap::new(),
            consumed_notices: HashSet::new(),
            completion_notice_queue: VecDeque::new(),
            business_results: VecDeque::new(),
            upstream_phase_active: false,
        }
    }

    pub fn submit_merge_plan(
        &mut self,
        request: MergePlanRequest,
    ) -> Result<MergeSubmission, TileMergeError> {
        if self.upstream_phase_active {
            return Err(TileMergeError::ReentrantDownstreamSubmission);
        }
        if request.tile_ops.is_empty() {
            return Err(TileMergeError::EmptyMergePlan {
                stroke_session_id: request.stroke_session_id,
                layer_id: request.layer_id,
            });
        }
        let submission_key = (request.stroke_session_id, request.tx_token);
        if let Some(existing_receipt_id) = self.submitted_tokens.get(&submission_key) {
            return Err(TileMergeError::DuplicateTxToken {
                stroke_session_id: request.stroke_session_id,
                tx_token: request.tx_token,
                existing_receipt_id: *existing_receipt_id,
            });
        }

        let mut previous_key_tiles = HashMap::new();
        for tile_op in &request.tile_ops {
            let Some(previous_key) = tile_op.existing_layer_key else {
                continue;
            };
            if let Some((first_tile_x, first_tile_y)) =
                previous_key_tiles.insert(previous_key, (tile_op.tile_x, tile_op.tile_y))
            {
                return Err(TileMergeError::SharedPreviousKeyNotAllowed {
                    stroke_session_id: request.stroke_session_id,
                    layer_id: request.layer_id,
                    previous_key,
                    first_tile_x,
                    first_tile_y,
                    duplicate_tile_x: tile_op.tile_x,
                    duplicate_tile_y: tile_op.tile_y,
                });
            }
        }

        let receipt_id = next_receipt_id(&self.next_receipt_id)?;
        let receipt = StrokeExecutionReceipt {
            receipt_id,
            stroke_session_id: request.stroke_session_id,
            tx_token: request.tx_token,
            program_revision: request.program_revision,
        };

        let mut allocated_new_keys = Vec::with_capacity(request.tile_ops.len());
        let mut new_key_mappings = Vec::with_capacity(request.tile_ops.len());
        let mut gpu_merge_ops = Vec::with_capacity(request.tile_ops.len());

        for tile_op in &request.tile_ops {
            let stroke_tile = self.resolve_gpu_tile(
                tile_op.stroke_buffer_key,
                Some(receipt_id),
                request.stroke_session_id,
                request.layer_id,
                "submit.resolve_stroke_key",
            )?;

            let base_tile = if let Some(existing_layer_key) = tile_op.existing_layer_key {
                Some(self.resolve_gpu_tile(
                    existing_layer_key,
                    Some(receipt_id),
                    request.stroke_session_id,
                    request.layer_id,
                    "submit.resolve_existing_layer_key",
                )?)
            } else {
                None
            };

            let new_key = match self.store.allocate() {
                Ok(new_key) => new_key,
                Err(source) => {
                    rollback_new_keys(
                        &self.store,
                        &allocated_new_keys,
                        &request,
                        Some(receipt_id),
                    )?;
                    return Err(TileMergeError::TileAlloc {
                        stroke_session_id: request.stroke_session_id,
                        layer_id: request.layer_id,
                        source,
                    });
                }
            };
            allocated_new_keys.push(new_key);

            let output_tile = match self.store.resolve(new_key) {
                Some(address) => to_gpu_tile_ref(address),
                None => {
                    rollback_new_keys(
                        &self.store,
                        &allocated_new_keys,
                        &request,
                        Some(receipt_id),
                    )?;
                    return Err(TileMergeError::UnknownTileKey {
                        receipt_id: Some(receipt_id),
                        stroke_session_id: request.stroke_session_id,
                        layer_id: request.layer_id,
                        key: new_key,
                        stage: "submit.resolve_new_key",
                    });
                }
            };

            gpu_merge_ops.push(GpuMergeOp {
                base_tile,
                stroke_tile,
                output_tile,
                blend_mode: tile_op.blend_mode,
                opacity: tile_op.opacity,
                op_trace_id: tile_op.op_trace_id,
            });
            new_key_mappings.push(TileKeyMapping {
                tile_x: tile_op.tile_x,
                tile_y: tile_op.tile_y,
                layer_id: request.layer_id,
                previous_key: tile_op.existing_layer_key,
                new_key,
            });
        }

        self.store.mark_keys_active(&allocated_new_keys);
        let drop_key_list = collect_drop_keys(&new_key_mappings);
        self.receipts.insert(
            receipt_id,
            ReceiptEntry {
                stroke_session_id: request.stroke_session_id,
                tx_token: request.tx_token,
                program_revision: request.program_revision,
                layer_id: request.layer_id,
                state: ReceiptState::Pending,
                completion: None,
                new_key_mappings: new_key_mappings.clone(),
                drop_key_list: drop_key_list.clone(),
            },
        );
        self.submitted_tokens.insert(submission_key, receipt_id);

        Ok(MergeSubmission {
            receipt_id,
            new_key_mappings,
            drop_key_list,
            renderer_submit_payload: RendererSubmitPayload {
                receipt,
                gpu_merge_ops,
                meta: MergePlanMeta {
                    stroke_session_id: request.stroke_session_id,
                    tx_token: request.tx_token,
                    program_revision: request.program_revision,
                },
            },
        })
    }

    pub fn poll_submission_results(&mut self) -> Vec<TileMergeCompletionNotice> {
        let notices: Vec<TileMergeCompletionNotice> =
            self.completion_notice_queue.drain(..).collect();
        if !notices.is_empty() {
            self.upstream_phase_active = true;
        }
        notices
    }

    pub fn on_renderer_completion_signal(
        &mut self,
        receipt_id: StrokeExecutionReceiptId,
        audit_meta: MergeAuditMeta,
        result: MergeExecutionResult,
    ) -> Result<(), TileMergeError> {
        self.on_renderer_merge_completion(TileMergeCompletionNotice::new(
            receipt_id, audit_meta, result,
        ))
    }

    pub fn ack_merge_result(
        &mut self,
        receipt_id: StrokeExecutionReceiptId,
        notice_id: TileMergeCompletionNoticeId,
    ) -> Result<AckOutcome, TileMergeError> {
        let notice_key = (notice_id, receipt_id);
        let Some(notice) = self.ackable_notices.remove(&notice_key) else {
            if self.consumed_notices.contains(&notice_key) {
                return Err(TileMergeError::DuplicateCompletionNotice {
                    receipt_id,
                    notice_id,
                });
            }
            if let Some(expected_receipt_id) = self.receipt_for_notice_id(notice_id) {
                return Err(TileMergeError::NoticeReceiptMismatch {
                    expected_receipt_id,
                    notice_receipt_id: receipt_id,
                    notice_id,
                });
            }
            return Err(TileMergeError::UnknownCompletionNotice {
                receipt_id,
                notice_id,
            });
        };
        if notice.receipt_id != receipt_id {
            self.ackable_notices.insert(notice_key, notice.clone());
            return Err(TileMergeError::NoticeReceiptMismatch {
                expected_receipt_id: receipt_id,
                notice_receipt_id: notice.receipt_id,
                notice_id,
            });
        }

        let entry = self
            .receipts
            .get_mut(&receipt_id)
            .ok_or(TileMergeError::UnknownReceipt { receipt_id })?;

        let next_state = match notice.result {
            MergeExecutionResult::Succeeded => ReceiptState::Succeeded,
            MergeExecutionResult::Failed { .. } => ReceiptState::Failed,
        };
        if entry.state != ReceiptState::Pending {
            self.ackable_notices.insert(notice_key, notice.clone());
            return Err(TileMergeError::IllegalStateTransition {
                receipt_id,
                stroke_session_id: entry.stroke_session_id,
                layer_id: entry.layer_id,
                from: entry.state,
                to: next_state,
            });
        }

        entry.state = next_state;
        entry.completion = Some(MergeCompletionAuditRecord {
            notice_id,
            audit_meta: notice.audit_meta,
            result: notice.result.clone(),
        });
        self.consumed_notices.insert(notice_key);

        match &notice.result {
            MergeExecutionResult::Succeeded => {
                self.business_results
                    .push_back(TilesBusinessResult::CanFinalize {
                        receipt_id,
                        stroke_session_id: entry.stroke_session_id,
                        layer_id: entry.layer_id,
                        new_key_mappings: entry.new_key_mappings.clone(),
                        drop_key_list: entry.drop_key_list.clone(),
                    });
                Ok(AckOutcome::Succeeded)
            }
            MergeExecutionResult::Failed { message } => {
                self.business_results
                    .push_back(TilesBusinessResult::RequiresAbort {
                        receipt_id,
                        stroke_session_id: entry.stroke_session_id,
                        layer_id: entry.layer_id,
                        new_key_mappings: entry.new_key_mappings.clone(),
                        drop_key_list: entry.drop_key_list.clone(),
                        message: message.clone(),
                    });
                Ok(AckOutcome::Failed)
            }
        }
    }

    pub fn drain_business_results(&mut self) -> Vec<TilesBusinessResult> {
        let drained: Vec<TilesBusinessResult> = self.business_results.drain(..).collect();
        if self.completion_notice_queue.is_empty() && self.ackable_notices.is_empty() {
            self.upstream_phase_active = false;
        }
        drained
    }

    pub fn query_receipt_state(
        &self,
        receipt_id: StrokeExecutionReceiptId,
    ) -> Result<ReceiptState, TileMergeError> {
        let entry = self
            .receipts
            .get(&receipt_id)
            .ok_or(TileMergeError::UnknownReceipt { receipt_id })?;
        Ok(entry.state)
    }

    pub fn query_merge_audit_record(
        &self,
        receipt_id: StrokeExecutionReceiptId,
    ) -> Result<MergeAuditRecord, TileMergeError> {
        let entry = self
            .receipts
            .get(&receipt_id)
            .ok_or(TileMergeError::UnknownReceipt { receipt_id })?;
        Ok(MergeAuditRecord {
            receipt_id,
            stroke_session_id: entry.stroke_session_id,
            tx_token: entry.tx_token,
            program_revision: entry.program_revision,
            layer_id: entry.layer_id,
            receipt_state: entry.state,
            completion: entry.completion.clone(),
        })
    }

    pub fn finalize_receipt(
        &mut self,
        receipt_id: StrokeExecutionReceiptId,
    ) -> Result<(), TileMergeError> {
        let entry = self
            .receipts
            .get_mut(&receipt_id)
            .ok_or(TileMergeError::UnknownReceipt { receipt_id })?;
        if entry.state != ReceiptState::Succeeded {
            return Err(TileMergeError::IllegalStateTransition {
                receipt_id,
                stroke_session_id: entry.stroke_session_id,
                layer_id: entry.layer_id,
                from: entry.state,
                to: ReceiptState::Finalized,
            });
        }

        for old_key in &entry.drop_key_list {
            if self.store.resolve(*old_key).is_none() {
                return Err(TileMergeError::UnknownTileKey {
                    receipt_id: Some(receipt_id),
                    stroke_session_id: entry.stroke_session_id,
                    layer_id: entry.layer_id,
                    key: *old_key,
                    stage: "finalize.precheck_drop_key",
                });
            }
        }

        if !entry.drop_key_list.is_empty() {
            self.store
                .retain_keys(entry.stroke_session_id, &entry.drop_key_list);
        }
        entry.state = ReceiptState::Finalized;
        Ok(())
    }

    pub fn abort_receipt(
        &mut self,
        receipt_id: StrokeExecutionReceiptId,
    ) -> Result<(), TileMergeError> {
        let entry = self
            .receipts
            .get_mut(&receipt_id)
            .ok_or(TileMergeError::UnknownReceipt { receipt_id })?;
        if entry.state != ReceiptState::Failed {
            return Err(TileMergeError::IllegalStateTransition {
                receipt_id,
                stroke_session_id: entry.stroke_session_id,
                layer_id: entry.layer_id,
                from: entry.state,
                to: ReceiptState::Aborted,
            });
        }

        for mapping in &entry.new_key_mappings {
            if self.store.resolve(mapping.new_key).is_none() {
                return Err(TileMergeError::UnknownTileKey {
                    receipt_id: Some(receipt_id),
                    stroke_session_id: entry.stroke_session_id,
                    layer_id: entry.layer_id,
                    key: mapping.new_key,
                    stage: "abort.precheck_new_key",
                });
            }
        }

        for mapping in &entry.new_key_mappings {
            let released = self.store.release(mapping.new_key);
            if !released {
                return Err(TileMergeError::UnknownTileKey {
                    receipt_id: Some(receipt_id),
                    stroke_session_id: entry.stroke_session_id,
                    layer_id: entry.layer_id,
                    key: mapping.new_key,
                    stage: "abort.release_new_key",
                });
            }
        }
        entry.state = ReceiptState::Aborted;
        Ok(())
    }

    fn resolve_gpu_tile(
        &self,
        key: TileKey,
        receipt_id: Option<StrokeExecutionReceiptId>,
        stroke_session_id: StrokeSessionId,
        layer_id: LayerId,
        stage: &'static str,
    ) -> Result<GpuTileRef, TileMergeError> {
        let Some(address) = self.store.resolve(key) else {
            return Err(TileMergeError::UnknownTileKey {
                receipt_id,
                stroke_session_id,
                layer_id,
                key,
                stage,
            });
        };
        Ok(to_gpu_tile_ref(address))
    }

    fn receipt_for_notice_id(
        &self,
        notice_id: TileMergeCompletionNoticeId,
    ) -> Option<StrokeExecutionReceiptId> {
        if let Some((_, receipt_id)) = self
            .ackable_notices
            .keys()
            .find(|(candidate_notice_id, _)| *candidate_notice_id == notice_id)
        {
            return Some(*receipt_id);
        }
        self.consumed_notices
            .iter()
            .find(|(candidate_notice_id, _)| *candidate_notice_id == notice_id)
            .map(|(_, receipt_id)| *receipt_id)
    }
}

impl<S: MergeTileStore> TileMergeCompletionCallback for TileMergeEngine<S> {
    type Error = TileMergeError;

    fn on_renderer_merge_completion(
        &mut self,
        notice: TileMergeCompletionNotice,
    ) -> Result<(), Self::Error> {
        let receipt_id = notice.receipt_id;
        let Some(entry) = self.receipts.get(&receipt_id) else {
            return Err(TileMergeError::UnknownReceipt { receipt_id });
        };
        if entry.state != ReceiptState::Pending {
            return Err(TileMergeError::IllegalStateTransition {
                receipt_id,
                stroke_session_id: entry.stroke_session_id,
                layer_id: entry.layer_id,
                from: entry.state,
                to: entry.state,
            });
        }
        let notice_key = (notice.notice_id, notice.receipt_id);
        if let Some(existing_receipt_id) = self.receipt_for_notice_id(notice.notice_id) {
            if existing_receipt_id == notice.receipt_id {
                return Err(TileMergeError::DuplicateCompletionNotice {
                    receipt_id,
                    notice_id: notice.notice_id,
                });
            }
            return Err(TileMergeError::NoticeReceiptMismatch {
                expected_receipt_id: existing_receipt_id,
                notice_receipt_id: notice.receipt_id,
                notice_id: notice.notice_id,
            });
        }

        self.ackable_notices.insert(notice_key, notice.clone());
        self.completion_notice_queue.push_back(notice);
        Ok(())
    }
}

fn to_gpu_tile_ref(address: TileAddress) -> GpuTileRef {
    GpuTileRef {
        atlas_layer: address.atlas_layer,
        tile_index: address.tile_index,
    }
}

fn collect_drop_keys(new_key_mappings: &[TileKeyMapping]) -> Vec<TileKey> {
    let mut seen = HashSet::with_capacity(new_key_mappings.len());
    let mut drop_key_list = Vec::new();
    for mapping in new_key_mappings {
        let Some(previous_key) = mapping.previous_key else {
            continue;
        };
        if seen.insert(previous_key) {
            drop_key_list.push(previous_key);
        }
    }
    drop_key_list
}

fn next_receipt_id(
    next_receipt_id: &AtomicU64,
) -> Result<StrokeExecutionReceiptId, TileMergeError> {
    loop {
        let current = next_receipt_id.load(Ordering::Relaxed);
        let Some(next) = current.checked_add(1) else {
            return Err(TileMergeError::ReceiptIdSpaceExhausted);
        };
        if next_receipt_id
            .compare_exchange(current, next, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
        {
            return Ok(StrokeExecutionReceiptId(current));
        }
    }
}

fn rollback_new_keys<S: MergeTileStore>(
    store: &S,
    allocated_new_keys: &[TileKey],
    request: &MergePlanRequest,
    receipt_id: Option<StrokeExecutionReceiptId>,
) -> Result<(), TileMergeError> {
    for key in allocated_new_keys {
        let released = store.release(*key);
        if !released {
            return Err(TileMergeError::UnknownTileKey {
                receipt_id,
                stroke_session_id: request.stroke_session_id,
                layer_id: request.layer_id,
                key: *key,
                stage: "submit.rollback_release_new_key",
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default)]
    struct FakeTileStore {
        next_key: u64,
        next_tile_index: u16,
        map: HashMap<TileKey, TileAddress>,
        mark_active_calls: Vec<Vec<TileKey>>,
        retain_calls: Vec<(u64, Vec<TileKey>)>,
    }

    impl MergeTileStore for FakeTileStore {
        fn allocate(&self) -> Result<TileKey, TileAllocError> {
            panic!("allocate mutation requires mutable fake store")
        }

        fn release(&self, _key: TileKey) -> bool {
            panic!("release mutation requires mutable fake store")
        }

        fn resolve(&self, key: TileKey) -> Option<TileAddress> {
            self.map.get(&key).copied()
        }

        fn mark_keys_active(&self, _keys: &[TileKey]) {
            panic!("mark active mutation requires mutable fake store")
        }

        fn retain_keys(&self, _retain_id: u64, _keys: &[TileKey]) {
            panic!("retain mutation requires mutable fake store")
        }
    }

    #[derive(Debug, Default, Clone)]
    struct SharedFakeTileStore(Arc<Mutex<FakeTileStore>>);

    impl SharedFakeTileStore {
        fn seed_key(&self, key: TileKey, address: TileAddress) {
            let mut guard = self
                .0
                .lock()
                .expect("fake tile store mutex should not be poisoned");
            guard.map.insert(key, address);
        }

        fn has_key(&self, key: TileKey) -> bool {
            let guard = self
                .0
                .lock()
                .expect("fake tile store mutex should not be poisoned");
            guard.map.contains_key(&key)
        }

        fn remove_key_for_test(&self, key: TileKey) {
            let mut guard = self
                .0
                .lock()
                .expect("fake tile store mutex should not be poisoned");
            guard.map.remove(&key);
        }

        fn mark_active_calls(&self) -> Vec<Vec<TileKey>> {
            let guard = self
                .0
                .lock()
                .expect("fake tile store mutex should not be poisoned");
            guard.mark_active_calls.clone()
        }

        fn retain_calls(&self) -> Vec<(u64, Vec<TileKey>)> {
            let guard = self
                .0
                .lock()
                .expect("fake tile store mutex should not be poisoned");
            guard.retain_calls.clone()
        }
    }

    impl MergeTileStore for SharedFakeTileStore {
        fn allocate(&self) -> Result<TileKey, TileAllocError> {
            let mut guard = self
                .0
                .lock()
                .expect("fake tile store mutex should not be poisoned");
            let key = TileKey(guard.next_key);
            guard.next_key = guard
                .next_key
                .checked_add(1)
                .expect("fake key counter overflow");
            let address = TileAddress {
                atlas_layer: 0,
                tile_index: guard.next_tile_index,
            };
            guard.next_tile_index = guard
                .next_tile_index
                .checked_add(1)
                .expect("fake tile index overflow");
            guard.map.insert(key, address);
            Ok(key)
        }

        fn release(&self, key: TileKey) -> bool {
            let mut guard = self
                .0
                .lock()
                .expect("fake tile store mutex should not be poisoned");
            guard.map.remove(&key).is_some()
        }

        fn resolve(&self, key: TileKey) -> Option<TileAddress> {
            let guard = self
                .0
                .lock()
                .expect("fake tile store mutex should not be poisoned");
            guard.map.get(&key).copied()
        }

        fn mark_keys_active(&self, keys: &[TileKey]) {
            let mut guard = self
                .0
                .lock()
                .expect("fake tile store mutex should not be poisoned");
            for key in keys {
                if !guard.map.contains_key(key) {
                    panic!("cannot mark unknown key as active in fake store");
                }
            }
            guard.mark_active_calls.push(keys.to_vec());
        }

        fn retain_keys(&self, retain_id: u64, keys: &[TileKey]) {
            let mut guard = self
                .0
                .lock()
                .expect("fake tile store mutex should not be poisoned");
            for key in keys {
                if !guard.map.contains_key(key) {
                    panic!("cannot retain unknown key in fake store");
                }
            }
            guard.retain_calls.push((retain_id, keys.to_vec()));
        }
    }

    fn request_with_single_op_with_token(
        existing_layer_key: Option<TileKey>,
        tx_token: TxToken,
    ) -> MergePlanRequest {
        MergePlanRequest {
            stroke_session_id: 100,
            tx_token,
            program_revision: Some(3),
            layer_id: 22,
            tile_ops: vec![MergePlanTileOp {
                tile_x: 0,
                tile_y: 0,
                existing_layer_key,
                stroke_buffer_key: TileKey(500),
                blend_mode: BlendMode::Normal,
                opacity: 1.0,
                op_trace_id: 7,
            }],
        }
    }

    fn request_with_single_op(existing_layer_key: Option<TileKey>) -> MergePlanRequest {
        request_with_single_op_with_token(existing_layer_key, 900)
    }

    fn request_with_two_ops(
        first_existing_layer_key: Option<TileKey>,
        second_existing_layer_key: Option<TileKey>,
    ) -> MergePlanRequest {
        MergePlanRequest {
            stroke_session_id: 100,
            tx_token: 901,
            program_revision: Some(3),
            layer_id: 22,
            tile_ops: vec![
                MergePlanTileOp {
                    tile_x: 0,
                    tile_y: 0,
                    existing_layer_key: first_existing_layer_key,
                    stroke_buffer_key: TileKey(500),
                    blend_mode: BlendMode::Normal,
                    opacity: 1.0,
                    op_trace_id: 7,
                },
                MergePlanTileOp {
                    tile_x: 1,
                    tile_y: 0,
                    existing_layer_key: second_existing_layer_key,
                    stroke_buffer_key: TileKey(501),
                    blend_mode: BlendMode::Normal,
                    opacity: 1.0,
                    op_trace_id: 8,
                },
            ],
        }
    }

    fn completion_notice(
        submission: &MergeSubmission,
        result: MergeExecutionResult,
    ) -> TileMergeCompletionNotice {
        TileMergeCompletionNotice::new(
            submission.receipt_id,
            render_protocol::MergeAuditMeta {
                frame_id: 8,
                renderer_submission_id: render_protocol::RendererSubmissionId(
                    submission.receipt_id.0 + 33,
                ),
                op_trace_id: Some(7),
            },
            result,
        )
    }

    #[test]
    fn poll_submission_results_does_not_advance_receipt_state() {
        let store = SharedFakeTileStore::default();
        store.seed_key(
            TileKey(500),
            TileAddress {
                atlas_layer: 0,
                tile_index: 9,
            },
        );
        let mut engine = TileMergeEngine::new(store.clone());
        let submission = engine
            .submit_merge_plan(request_with_single_op(None))
            .expect("submit merge plan");

        let notice = completion_notice(&submission, MergeExecutionResult::Succeeded);
        engine
            .on_renderer_merge_completion(notice)
            .expect("enqueue completion notice");

        let polled = engine.poll_submission_results();
        assert_eq!(polled.len(), 1);
        assert_eq!(
            engine
                .query_receipt_state(submission.receipt_id)
                .expect("query pending state"),
            ReceiptState::Pending
        );
    }

    #[test]
    fn ack_merge_result_is_single_state_transition_entry() {
        let store = SharedFakeTileStore::default();
        store.seed_key(
            TileKey(500),
            TileAddress {
                atlas_layer: 0,
                tile_index: 9,
            },
        );
        let mut engine = TileMergeEngine::new(store.clone());
        let submission = engine
            .submit_merge_plan(request_with_single_op(None))
            .expect("submit merge plan");

        let notice = completion_notice(&submission, MergeExecutionResult::Succeeded);
        engine
            .on_renderer_merge_completion(notice.clone())
            .expect("enqueue completion notice");
        let _ = engine.poll_submission_results();

        let ack = engine
            .ack_merge_result(submission.receipt_id, notice.notice_id)
            .expect("ack completion");
        assert_eq!(ack, AckOutcome::Succeeded);
        assert_eq!(
            engine
                .query_receipt_state(submission.receipt_id)
                .expect("query succeeded state"),
            ReceiptState::Succeeded
        );
        assert_eq!(store.mark_active_calls().len(), 1);
    }

    #[test]
    fn query_merge_audit_record_reports_minimal_receipt_and_completion_metadata() {
        let store = SharedFakeTileStore::default();
        store.seed_key(
            TileKey(500),
            TileAddress {
                atlas_layer: 0,
                tile_index: 9,
            },
        );
        let mut engine = TileMergeEngine::new(store);
        let submission = engine
            .submit_merge_plan(request_with_single_op_with_token(None, 913))
            .expect("submit merge plan");

        let pending_audit = engine
            .query_merge_audit_record(submission.receipt_id)
            .expect("query pending audit record");
        assert_eq!(pending_audit.receipt_id, submission.receipt_id);
        assert_eq!(pending_audit.stroke_session_id, 100);
        assert_eq!(pending_audit.tx_token, 913);
        assert_eq!(pending_audit.program_revision, Some(3));
        assert_eq!(pending_audit.layer_id, 22);
        assert_eq!(pending_audit.receipt_state, ReceiptState::Pending);
        assert!(pending_audit.completion.is_none());

        let notice = completion_notice(
            &submission,
            MergeExecutionResult::Failed {
                message: "gpu merge failed".to_owned(),
            },
        );
        engine
            .on_renderer_merge_completion(notice.clone())
            .expect("enqueue completion notice");
        let _ = engine.poll_submission_results();
        engine
            .ack_merge_result(submission.receipt_id, notice.notice_id)
            .expect("ack failed completion");

        let failed_audit = engine
            .query_merge_audit_record(submission.receipt_id)
            .expect("query failed audit record");
        assert_eq!(failed_audit.receipt_state, ReceiptState::Failed);
        let completion = failed_audit
            .completion
            .expect("completion audit should be recorded after ack");
        assert_eq!(completion.notice_id, notice.notice_id);
        assert_eq!(completion.audit_meta, notice.audit_meta);
        assert_eq!(completion.result, notice.result);
    }

    #[test]
    fn duplicate_ack_is_fail_fast() {
        let store = SharedFakeTileStore::default();
        store.seed_key(
            TileKey(500),
            TileAddress {
                atlas_layer: 0,
                tile_index: 9,
            },
        );
        let mut engine = TileMergeEngine::new(store);
        let submission = engine
            .submit_merge_plan(request_with_single_op(None))
            .expect("submit merge plan");

        let notice = completion_notice(&submission, MergeExecutionResult::Succeeded);
        engine
            .on_renderer_merge_completion(notice.clone())
            .expect("enqueue completion notice");
        let _ = engine.poll_submission_results();

        engine
            .ack_merge_result(submission.receipt_id, notice.notice_id)
            .expect("first ack succeeds");

        let error = engine
            .ack_merge_result(submission.receipt_id, notice.notice_id)
            .expect_err("duplicate ack should fail");
        assert!(matches!(
            error,
            TileMergeError::DuplicateCompletionNotice {
                receipt_id,
                notice_id: _
            } if receipt_id == submission.receipt_id
        ));
    }

    #[test]
    fn illegal_state_transition_is_fail_fast() {
        let store = SharedFakeTileStore::default();
        store.seed_key(
            TileKey(500),
            TileAddress {
                atlas_layer: 0,
                tile_index: 9,
            },
        );
        let mut engine = TileMergeEngine::new(store);
        let submission = engine
            .submit_merge_plan(request_with_single_op(None))
            .expect("submit merge plan");
        let notice = completion_notice(
            &submission,
            MergeExecutionResult::Failed {
                message: "gpu failed".to_owned(),
            },
        );

        engine
            .on_renderer_merge_completion(notice.clone())
            .expect("enqueue completion notice");
        let _ = engine.poll_submission_results();
        engine
            .ack_merge_result(submission.receipt_id, notice.notice_id)
            .expect("ack failed result");

        let error = engine
            .finalize_receipt(submission.receipt_id)
            .expect_err("cannot finalize a failed receipt");
        assert!(matches!(
            error,
            TileMergeError::IllegalStateTransition {
                from: ReceiptState::Failed,
                to: ReceiptState::Finalized,
                ..
            }
        ));
    }

    #[test]
    fn upstream_processing_blocks_new_submission_until_business_drain() {
        let store = SharedFakeTileStore::default();
        store.seed_key(
            TileKey(500),
            TileAddress {
                atlas_layer: 0,
                tile_index: 9,
            },
        );
        let mut engine = TileMergeEngine::new(store);
        let submission = engine
            .submit_merge_plan(request_with_single_op(None))
            .expect("submit merge plan");

        let notice = completion_notice(&submission, MergeExecutionResult::Succeeded);
        engine
            .on_renderer_merge_completion(notice.clone())
            .expect("enqueue completion notice");
        let _ = engine.poll_submission_results();

        let blocked = engine
            .submit_merge_plan(request_with_single_op(None))
            .expect_err("submit must be blocked in upstream phase");
        assert_eq!(blocked, TileMergeError::ReentrantDownstreamSubmission);

        engine
            .ack_merge_result(submission.receipt_id, notice.notice_id)
            .expect("ack completion notice before unlock");
        let _ = engine.drain_business_results();
        let second = engine.submit_merge_plan(request_with_single_op_with_token(None, 901));
        assert!(second.is_ok());
    }

    #[test]
    fn duplicate_completion_notice_replay_is_rejected() {
        let store = SharedFakeTileStore::default();
        store.seed_key(
            TileKey(500),
            TileAddress {
                atlas_layer: 0,
                tile_index: 9,
            },
        );
        let mut engine = TileMergeEngine::new(store);
        let submission = engine
            .submit_merge_plan(request_with_single_op(None))
            .expect("submit merge plan");
        let notice = completion_notice(&submission, MergeExecutionResult::Succeeded);

        engine
            .on_renderer_merge_completion(notice.clone())
            .expect("enqueue first notice");
        let replay = engine.on_renderer_merge_completion(notice);
        assert!(matches!(
            replay,
            Err(TileMergeError::DuplicateCompletionNotice {
                receipt_id,
                notice_id: _
            }) if receipt_id == submission.receipt_id
        ));
    }

    #[test]
    fn finalize_marks_drop_keys_retained_without_releasing_them() {
        let store = SharedFakeTileStore::default();
        let old_layer_key = TileKey(600);
        store.seed_key(
            TileKey(500),
            TileAddress {
                atlas_layer: 0,
                tile_index: 9,
            },
        );
        store.seed_key(
            old_layer_key,
            TileAddress {
                atlas_layer: 0,
                tile_index: 10,
            },
        );
        let mut engine = TileMergeEngine::new(store.clone());
        let submission = engine
            .submit_merge_plan(request_with_single_op(Some(old_layer_key)))
            .expect("submit merge plan");
        assert!(store.has_key(old_layer_key));

        let notice = completion_notice(&submission, MergeExecutionResult::Succeeded);
        engine
            .on_renderer_merge_completion(notice.clone())
            .expect("enqueue completion notice");
        let _ = engine.poll_submission_results();
        engine
            .ack_merge_result(submission.receipt_id, notice.notice_id)
            .expect("ack completion");

        assert!(store.has_key(old_layer_key));
        engine
            .finalize_receipt(submission.receipt_id)
            .expect("finalize receipt");
        assert!(store.has_key(old_layer_key));
        let retain_calls = store.retain_calls();
        assert_eq!(retain_calls.len(), 1);
        assert_eq!(
            retain_calls[0].0,
            submission.renderer_submit_payload.receipt.stroke_session_id
        );
        assert_eq!(retain_calls[0].1, vec![old_layer_key]);
        assert_eq!(
            engine
                .query_receipt_state(submission.receipt_id)
                .expect("query finalized state"),
            ReceiptState::Finalized
        );
    }

    #[test]
    fn abort_releases_new_keys_and_preserves_old_keys() {
        let store = SharedFakeTileStore::default();
        let old_layer_key = TileKey(600);
        store.seed_key(
            TileKey(500),
            TileAddress {
                atlas_layer: 0,
                tile_index: 9,
            },
        );
        store.seed_key(
            old_layer_key,
            TileAddress {
                atlas_layer: 0,
                tile_index: 10,
            },
        );
        let mut engine = TileMergeEngine::new(store.clone());
        let submission = engine
            .submit_merge_plan(request_with_single_op(Some(old_layer_key)))
            .expect("submit merge plan");
        let new_key = submission.new_key_mappings[0].new_key;
        assert!(store.has_key(new_key));

        let notice = completion_notice(
            &submission,
            MergeExecutionResult::Failed {
                message: "merge failed".to_owned(),
            },
        );
        engine
            .on_renderer_merge_completion(notice.clone())
            .expect("enqueue completion notice");
        let _ = engine.poll_submission_results();
        engine
            .ack_merge_result(submission.receipt_id, notice.notice_id)
            .expect("ack failure");
        engine
            .abort_receipt(submission.receipt_id)
            .expect("abort receipt");

        assert!(!store.has_key(new_key));
        assert!(store.has_key(old_layer_key));
        assert_eq!(
            engine
                .query_receipt_state(submission.receipt_id)
                .expect("query aborted state"),
            ReceiptState::Aborted
        );
    }

    #[test]
    fn ack_rejects_mismatched_receipt_notice_pair() {
        let store = SharedFakeTileStore::default();
        store.seed_key(
            TileKey(500),
            TileAddress {
                atlas_layer: 0,
                tile_index: 9,
            },
        );
        let mut engine = TileMergeEngine::new(store);
        let submission = engine
            .submit_merge_plan(request_with_single_op(None))
            .expect("submit merge plan");
        let notice = completion_notice(&submission, MergeExecutionResult::Succeeded);
        engine
            .on_renderer_merge_completion(notice.clone())
            .expect("enqueue completion notice");
        let _ = engine.poll_submission_results();

        let mismatch = engine
            .ack_merge_result(
                StrokeExecutionReceiptId(submission.receipt_id.0 + 1),
                notice.notice_id,
            )
            .expect_err("mismatched receipt id must fail");
        assert!(matches!(
            mismatch,
            TileMergeError::NoticeReceiptMismatch {
                expected_receipt_id,
                notice_receipt_id,
                ..
            } if expected_receipt_id == submission.receipt_id && notice_receipt_id == StrokeExecutionReceiptId(submission.receipt_id.0 + 1)
        ));
    }

    #[test]
    fn drain_business_results_does_not_unlock_when_notice_is_unacked() {
        let store = SharedFakeTileStore::default();
        store.seed_key(
            TileKey(500),
            TileAddress {
                atlas_layer: 0,
                tile_index: 9,
            },
        );
        let mut engine = TileMergeEngine::new(store);
        let submission = engine
            .submit_merge_plan(request_with_single_op(None))
            .expect("submit merge plan");

        let notice = completion_notice(&submission, MergeExecutionResult::Succeeded);
        engine
            .on_renderer_merge_completion(notice)
            .expect("enqueue completion notice");
        let _ = engine.poll_submission_results();

        let drained = engine.drain_business_results();
        assert!(drained.is_empty());
        let blocked = engine
            .submit_merge_plan(request_with_single_op(None))
            .expect_err("unacked notice must keep upstream phase locked");
        assert_eq!(blocked, TileMergeError::ReentrantDownstreamSubmission);
    }

    #[test]
    fn duplicate_notice_id_across_receipts_is_rejected() {
        let store = SharedFakeTileStore::default();
        store.seed_key(
            TileKey(500),
            TileAddress {
                atlas_layer: 0,
                tile_index: 9,
            },
        );
        let mut engine = TileMergeEngine::new(store);

        let first_submission = engine
            .submit_merge_plan(request_with_single_op(None))
            .expect("submit first plan");
        let second_submission = engine
            .submit_merge_plan(request_with_single_op_with_token(None, 901))
            .expect("submit second plan");

        let duplicated_notice_id = TileMergeCompletionNoticeId {
            renderer_submission_id: render_protocol::RendererSubmissionId(77),
            frame_id: 9,
            op_trace_id: Some(1),
        };
        let first_notice = TileMergeCompletionNotice::new(
            first_submission.receipt_id,
            render_protocol::MergeAuditMeta {
                frame_id: duplicated_notice_id.frame_id,
                renderer_submission_id: duplicated_notice_id.renderer_submission_id,
                op_trace_id: duplicated_notice_id.op_trace_id,
            },
            MergeExecutionResult::Succeeded,
        );
        let second_notice = TileMergeCompletionNotice::new(
            second_submission.receipt_id,
            render_protocol::MergeAuditMeta {
                frame_id: duplicated_notice_id.frame_id,
                renderer_submission_id: duplicated_notice_id.renderer_submission_id,
                op_trace_id: duplicated_notice_id.op_trace_id,
            },
            MergeExecutionResult::Succeeded,
        );

        engine
            .on_renderer_merge_completion(first_notice)
            .expect("first notice accepted");
        let duplicated = engine.on_renderer_merge_completion(second_notice);
        assert!(matches!(
            duplicated,
            Err(TileMergeError::NoticeReceiptMismatch {
                expected_receipt_id,
                notice_receipt_id,
                notice_id
            })
            if expected_receipt_id == first_submission.receipt_id
                && notice_receipt_id == second_submission.receipt_id
                && notice_id == duplicated_notice_id
        ));
    }

    #[test]
    fn renderer_completion_signal_adapter_enqueues_notice() {
        let store = SharedFakeTileStore::default();
        store.seed_key(
            TileKey(500),
            TileAddress {
                atlas_layer: 0,
                tile_index: 9,
            },
        );
        let mut engine = TileMergeEngine::new(store);
        let submission = engine
            .submit_merge_plan(request_with_single_op(None))
            .expect("submit merge plan");

        engine
            .on_renderer_completion_signal(
                submission.receipt_id,
                render_protocol::MergeAuditMeta {
                    frame_id: 8,
                    renderer_submission_id: render_protocol::RendererSubmissionId(99),
                    op_trace_id: Some(7),
                },
                MergeExecutionResult::Succeeded,
            )
            .expect("adapted completion signal accepted");
        let notices = engine.poll_submission_results();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].receipt_id, submission.receipt_id);
    }

    #[test]
    fn submit_rejects_duplicate_tx_token_for_same_stroke_session() {
        let store = SharedFakeTileStore::default();
        store.seed_key(
            TileKey(500),
            TileAddress {
                atlas_layer: 0,
                tile_index: 9,
            },
        );
        let mut engine = TileMergeEngine::new(store);
        let request = request_with_single_op(None);

        let first = engine
            .submit_merge_plan(request.clone())
            .expect("first submit");
        let duplicate = engine
            .submit_merge_plan(request)
            .expect_err("duplicate tx token must fail fast");
        assert!(matches!(
            duplicate,
            TileMergeError::DuplicateTxToken {
                stroke_session_id,
                tx_token,
                existing_receipt_id
            } if stroke_session_id == 100 && tx_token == 900 && existing_receipt_id == first.receipt_id
        ));
    }

    #[test]
    fn submit_rejects_shared_previous_key_across_multiple_tile_ops() {
        let store = SharedFakeTileStore::default();
        let shared_previous_key = TileKey(600);
        store.seed_key(
            TileKey(500),
            TileAddress {
                atlas_layer: 0,
                tile_index: 9,
            },
        );
        store.seed_key(
            TileKey(501),
            TileAddress {
                atlas_layer: 0,
                tile_index: 11,
            },
        );
        store.seed_key(
            shared_previous_key,
            TileAddress {
                atlas_layer: 0,
                tile_index: 10,
            },
        );

        let mut engine = TileMergeEngine::new(store);
        let error = engine
            .submit_merge_plan(request_with_two_ops(
                Some(shared_previous_key),
                Some(shared_previous_key),
            ))
            .expect_err("shared previous key must be rejected");
        assert!(matches!(
            error,
            TileMergeError::SharedPreviousKeyNotAllowed {
                stroke_session_id,
                layer_id,
                previous_key,
                first_tile_x,
                first_tile_y,
                duplicate_tile_x,
                duplicate_tile_y,
            } if stroke_session_id == 100
                && layer_id == 22
                && previous_key == shared_previous_key
                && first_tile_x == 0
                && first_tile_y == 0
                && duplicate_tile_x == 1
                && duplicate_tile_y == 0
        ));
    }

    #[test]
    fn new_key_mapping_preserves_tile_coordinates() {
        let store = SharedFakeTileStore::default();
        store.seed_key(
            TileKey(500),
            TileAddress {
                atlas_layer: 0,
                tile_index: 9,
            },
        );
        store.seed_key(
            TileKey(501),
            TileAddress {
                atlas_layer: 0,
                tile_index: 11,
            },
        );

        let mut engine = TileMergeEngine::new(store);
        let submission = engine
            .submit_merge_plan(request_with_two_ops(None, None))
            .expect("submit merge plan");
        assert_eq!(submission.new_key_mappings.len(), 2);
        assert_eq!(submission.new_key_mappings[0].tile_x, 0);
        assert_eq!(submission.new_key_mappings[0].tile_y, 0);
        assert_eq!(submission.new_key_mappings[1].tile_x, 1);
        assert_eq!(submission.new_key_mappings[1].tile_y, 0);
    }

    #[test]
    fn finalize_precheck_prevents_partial_drop_key_release() {
        let store = SharedFakeTileStore::default();
        let first_old_layer_key = TileKey(600);
        let second_old_layer_key = TileKey(601);
        store.seed_key(
            TileKey(500),
            TileAddress {
                atlas_layer: 0,
                tile_index: 9,
            },
        );
        store.seed_key(
            TileKey(501),
            TileAddress {
                atlas_layer: 0,
                tile_index: 11,
            },
        );
        store.seed_key(
            first_old_layer_key,
            TileAddress {
                atlas_layer: 0,
                tile_index: 10,
            },
        );
        store.seed_key(
            second_old_layer_key,
            TileAddress {
                atlas_layer: 0,
                tile_index: 12,
            },
        );

        let mut engine = TileMergeEngine::new(store.clone());
        let submission = engine
            .submit_merge_plan(request_with_two_ops(
                Some(first_old_layer_key),
                Some(second_old_layer_key),
            ))
            .expect("submit merge plan");
        let notice = completion_notice(&submission, MergeExecutionResult::Succeeded);

        engine
            .on_renderer_merge_completion(notice.clone())
            .expect("enqueue completion notice");
        let _ = engine.poll_submission_results();
        engine
            .ack_merge_result(submission.receipt_id, notice.notice_id)
            .expect("ack completion notice");

        store.remove_key_for_test(second_old_layer_key);
        let finalize_error = engine
            .finalize_receipt(submission.receipt_id)
            .expect_err("missing drop key must fail precheck");
        assert!(matches!(
            finalize_error,
            TileMergeError::UnknownTileKey {
                key,
                stage,
                ..
            } if key == second_old_layer_key && stage == "finalize.precheck_drop_key"
        ));
        assert!(store.has_key(first_old_layer_key));
        assert_eq!(
            engine
                .query_receipt_state(submission.receipt_id)
                .expect("state remains succeeded"),
            ReceiptState::Succeeded
        );
    }
}
