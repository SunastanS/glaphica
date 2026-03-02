pub use glaphica_core::{AtlasLayout, PipelineId, ShaderId, TileKey};

use std::collections::HashMap;
use std::hash::Hash;
use std::marker::PhantomData;
use std::{cmp::Ordering, fmt};

/// This crate defines the bottom communication protocol of app thread and engine thread
/// Can be dependent by any crates
/// Should only depend on foundational crates

/// Input transport design:
/// - Ring buffer: lossy high-frequency samples (ok to drop/overwrite).
/// - Control events that define semantic boundaries (stroke begin/end, tool change, layer change)
///   MUST be delivered reliably (bounded queue) and MUST NOT be stored only in the overwrite ring.
///   Dropping boundary events causes undefined stroke state.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputRingSample {
    /// `epoch` groups samples that share the same semantic state (tool/params/target).
    /// Back-end must treat epoch boundaries as "can only change at safe points".
    pub epoch: u32,
    pub cursor_x: f32,
    pub cursor_y: f32,
    pub pressure: f32,
    pub tilt: f32,
    pub twist: f32,
}

pub trait InputControlOp {
    type Target;

    fn apply(&self, target: &mut Self::Target);
    fn undo(&self, target: &mut Self::Target);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputControlEvent<Control>
where
    Control: InputControlOp,
{
    Control(Control),
}

impl<Control> InputControlEvent<Control>
where
    Control: InputControlOp,
{
    pub fn apply(&self, target: &mut Control::Target) {
        match self {
            Self::Control(control) => control.apply(target),
        }
    }

    pub fn undo(&self, target: &mut Control::Target) {
        match self {
            Self::Control(control) => control.undo(target),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PresentFrameId(pub u64);

#[derive(Debug, PartialEq, Eq)]
pub enum SubmitWaterlineTag {}

#[derive(Debug, PartialEq, Eq)]
pub enum ExecutedBatchWaterlineTag {}

#[derive(Debug, PartialEq, Eq)]
pub enum CompleteWaterlineTag {}

#[repr(transparent)]
pub struct Waterline<Tag> {
    raw: u64,
    _marker: PhantomData<Tag>,
}

impl<Tag> Copy for Waterline<Tag> {}

impl<Tag> Clone for Waterline<Tag> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Tag> PartialEq for Waterline<Tag> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<Tag> Eq for Waterline<Tag> {}

impl<Tag> PartialOrd for Waterline<Tag> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<Tag> Ord for Waterline<Tag> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.raw.cmp(&other.raw)
    }
}

impl<Tag> fmt::Debug for Waterline<Tag> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.raw.fmt(f)
    }
}

impl<Tag> Waterline<Tag> {
    pub const fn new(raw: u64) -> Self {
        Self {
            raw,
            _marker: PhantomData,
        }
    }

    pub const fn raw(self) -> u64 {
        self.raw
    }
}

pub type SubmitWaterline = Waterline<SubmitWaterlineTag>;
pub type ExecutedBatchWaterline = Waterline<ExecutedBatchWaterlineTag>;
pub type CompleteWaterline = Waterline<CompleteWaterlineTag>;

#[derive(Debug, Clone, PartialEq)]
pub struct DrawOp {
    pub tile_key: TileKey,
    pub input: Vec<f32>,
    pub pipeline_id: PipelineId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyOp {
    pub src_tile_key: TileKey,
    pub dst_tile_key: TileKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClearOp {
    pub tile_key: TileKey,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GpuCmdMsg {
    Notify,
    DrawOp(DrawOp),
    CopyOp(CopyOp),
    ClearOp(ClearOp),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuFeedbackFrame<Receipt, Error> {
    pub present_frame_id: PresentFrameId,
    pub submit_waterline: SubmitWaterline,
    pub executed_batch_waterline: ExecutedBatchWaterline,
    pub complete_waterline: CompleteWaterline,
    /// `receipts` / `errors` are non-overwritable deltas.
    /// They must not be modeled as a single waterline because they are not contiguous,
    /// and loss would break correctness (resource allocation results, failure reasons, etc.).
    pub receipts: Vec<Receipt>,
    pub errors: Vec<Error>,
}

/// Minimal contract for centralized vector merge:
/// - each module only defines how to compute identity key;
/// - protocol layer performs O(n) dedup and optional duplicate reconciliation
///   using caller-owned reusable hash indexes.
pub trait MergeItem: Sized {
    type MergeKey: Eq + Hash;

    /// Key must be stable for one logical entity across frames.
    fn merge_key(&self) -> Self::MergeKey;

    /// Duplicate reconciliation policy.
    /// If duplicates are possible for this item type, implementations must provide this method.
    /// The default is fail-fast to avoid silent data corruption.
    fn merge_duplicate(existing: &mut Self, incoming: Self) {
        let _ = existing;
        let _ = incoming;
        panic!("merge_duplicate is not implemented for this item type");
    }
}

#[derive(Debug)]
pub struct MergeVecIndex<Key> {
    index_by_key: HashMap<Key, usize>,
}

impl<Key> Default for MergeVecIndex<Key> {
    fn default() -> Self {
        Self {
            index_by_key: HashMap::new(),
        }
    }
}

impl<Key> MergeVecIndex<Key> {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            index_by_key: HashMap::with_capacity(capacity),
        }
    }

    pub fn clear(&mut self) {
        self.index_by_key.clear();
    }
}

#[derive(Debug, Default)]
pub struct GpuFeedbackMergeState<ReceiptKey, ErrorKey> {
    pub receipt_index: MergeVecIndex<ReceiptKey>,
    pub error_index: MergeVecIndex<ErrorKey>,
}

pub fn merge_vec<Item>(
    current: &mut Vec<Item>,
    incoming: Vec<Item>,
    merge_index: &mut MergeVecIndex<Item::MergeKey>,
) where
    Item: MergeItem,
{
    merge_index.clear();
    for (index, item) in current.iter().enumerate() {
        let duplicated_index = merge_index.index_by_key.insert(item.merge_key(), index);
        if duplicated_index.is_some() {
            panic!("current vector contains duplicated merge key before merge");
        }
    }

    for incoming_item in incoming {
        let key = incoming_item.merge_key();
        match merge_index.index_by_key.get(&key).copied() {
            Some(existing_index) => {
                Item::merge_duplicate(&mut current[existing_index], incoming_item);
            }
            None => {
                let new_index = current.len();
                current.push(incoming_item);
                merge_index.index_by_key.insert(key, new_index);
            }
        }
    }
}

impl<Receipt, Error> GpuFeedbackFrame<Receipt, Error>
where
    Receipt: MergeItem,
    Error: MergeItem,
{
    pub fn merge_mailbox(
        mut current: Self,
        newer: Self,
        merge_state: &mut GpuFeedbackMergeState<Receipt::MergeKey, Error::MergeKey>,
    ) -> Self {
        current.present_frame_id = current.present_frame_id.max(newer.present_frame_id);
        current.submit_waterline = current.submit_waterline.max(newer.submit_waterline);
        current.executed_batch_waterline = current
            .executed_batch_waterline
            .max(newer.executed_batch_waterline);
        current.complete_waterline = current.complete_waterline.max(newer.complete_waterline);
        merge_vec(
            &mut current.receipts,
            newer.receipts,
            &mut merge_state.receipt_index,
        );
        merge_vec(
            &mut current.errors,
            newer.errors,
            &mut merge_state.error_index,
        );
        current
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ClearOp, CompleteWaterline, CopyOp, DrawOp, ExecutedBatchWaterline, GpuCmdMsg,
        GpuFeedbackFrame, GpuFeedbackMergeState, InputControlEvent, InputControlOp, MergeItem,
        PipelineId, PresentFrameId, SubmitWaterline, TileKey,
    };

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestReceipt {
        key: u64,
        payload_version: u64,
    }

    impl MergeItem for TestReceipt {
        type MergeKey = u64;

        fn merge_key(&self) -> Self::MergeKey {
            self.key
        }

        fn merge_duplicate(existing: &mut Self, incoming: Self) {
            if incoming.payload_version > existing.payload_version {
                *existing = incoming;
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestError {
        key: u64,
    }

    impl MergeItem for TestError {
        type MergeKey = u64;

        fn merge_key(&self) -> Self::MergeKey {
            self.key
        }

        fn merge_duplicate(existing: &mut Self, incoming: Self) {
            let _ = incoming;
            let _ = existing;
        }
    }

    #[test]
    fn mailbox_merge_is_idempotent_and_uses_max_waterlines() {
        let current = GpuFeedbackFrame {
            present_frame_id: PresentFrameId(10),
            submit_waterline: SubmitWaterline::new(2),
            executed_batch_waterline: ExecutedBatchWaterline::new(3),
            complete_waterline: CompleteWaterline::new(4),
            receipts: vec![TestReceipt {
                key: 1,
                payload_version: 10,
            }],
            errors: vec![TestError { key: 2 }],
        };
        let newer = GpuFeedbackFrame {
            present_frame_id: PresentFrameId(9),
            submit_waterline: SubmitWaterline::new(20),
            executed_batch_waterline: ExecutedBatchWaterline::new(30),
            complete_waterline: CompleteWaterline::new(40),
            receipts: vec![
                TestReceipt {
                    key: 1,
                    payload_version: 11,
                },
                TestReceipt {
                    key: 3,
                    payload_version: 1,
                },
            ],
            errors: vec![TestError { key: 2 }, TestError { key: 4 }],
        };

        let mut merge_state = GpuFeedbackMergeState::default();
        let once = GpuFeedbackFrame::merge_mailbox(current, newer.clone(), &mut merge_state);
        let twice = GpuFeedbackFrame::merge_mailbox(once.clone(), newer, &mut merge_state);
        assert_eq!(once.present_frame_id, PresentFrameId(10));
        assert_eq!(once.submit_waterline, SubmitWaterline::new(20));
        assert_eq!(
            once.executed_batch_waterline,
            ExecutedBatchWaterline::new(30)
        );
        assert_eq!(once.complete_waterline, CompleteWaterline::new(40));
        assert_eq!(once.receipts.len(), 2);
        assert_eq!(once.errors.len(), 2);
        assert_eq!(once.receipts[0].payload_version, 11);
        assert_eq!(once, twice);
    }

    #[test]
    #[should_panic(expected = "current vector contains duplicated merge key before merge")]
    fn mailbox_merge_panics_when_current_contains_duplicated_keys() {
        let current = GpuFeedbackFrame {
            present_frame_id: PresentFrameId(1),
            submit_waterline: SubmitWaterline::new(1),
            executed_batch_waterline: ExecutedBatchWaterline::new(1),
            complete_waterline: CompleteWaterline::new(1),
            receipts: vec![
                TestReceipt {
                    key: 1,
                    payload_version: 1,
                },
                TestReceipt {
                    key: 1,
                    payload_version: 2,
                },
            ],
            errors: vec![TestError { key: 10 }],
        };
        let newer = GpuFeedbackFrame {
            present_frame_id: PresentFrameId(2),
            submit_waterline: SubmitWaterline::new(2),
            executed_batch_waterline: ExecutedBatchWaterline::new(2),
            complete_waterline: CompleteWaterline::new(2),
            receipts: vec![TestReceipt {
                key: 2,
                payload_version: 1,
            }],
            errors: vec![TestError { key: 20 }],
        };

        let mut merge_state = GpuFeedbackMergeState::default();
        let _ = GpuFeedbackFrame::merge_mailbox(current, newer, &mut merge_state);
    }

    #[test]
    fn mailbox_merge_merges_duplicated_incoming_keys_with_item_policy() {
        let current = GpuFeedbackFrame {
            present_frame_id: PresentFrameId(1),
            submit_waterline: SubmitWaterline::new(1),
            executed_batch_waterline: ExecutedBatchWaterline::new(1),
            complete_waterline: CompleteWaterline::new(1),
            receipts: vec![TestReceipt {
                key: 7,
                payload_version: 5,
            }],
            errors: vec![TestError { key: 100 }],
        };
        let newer = GpuFeedbackFrame {
            present_frame_id: PresentFrameId(2),
            submit_waterline: SubmitWaterline::new(2),
            executed_batch_waterline: ExecutedBatchWaterline::new(2),
            complete_waterline: CompleteWaterline::new(2),
            receipts: vec![
                TestReceipt {
                    key: 7,
                    payload_version: 8,
                },
                TestReceipt {
                    key: 7,
                    payload_version: 6,
                },
                TestReceipt {
                    key: 9,
                    payload_version: 1,
                },
            ],
            errors: vec![TestError { key: 200 }],
        };

        let mut merge_state = GpuFeedbackMergeState::default();
        let merged = GpuFeedbackFrame::merge_mailbox(current, newer, &mut merge_state);
        assert_eq!(merged.receipts.len(), 2);
        assert_eq!(merged.receipts[0].key, 7);
        assert_eq!(merged.receipts[0].payload_version, 8);
        assert_eq!(merged.receipts[1].key, 9);
        assert_eq!(merged.receipts[1].payload_version, 1);
    }

    #[test]
    fn waterline_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SubmitWaterline>();
        assert_send_sync::<ExecutedBatchWaterline>();
        assert_send_sync::<CompleteWaterline>();
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestControlOp(u8);

    impl InputControlOp for TestControlOp {
        type Target = u8;

        fn apply(&self, target: &mut Self::Target) {
            *target = target.saturating_add(self.0);
        }

        fn undo(&self, target: &mut Self::Target) {
            *target = target.saturating_sub(self.0);
        }
    }

    #[test]
    fn input_control_event_delegates_apply_and_undo() {
        let event = InputControlEvent::Control(TestControlOp(3));
        let mut state = 10;
        event.apply(&mut state);
        assert_eq!(state, 13);
        event.undo(&mut state);
        assert_eq!(state, 10);
    }

    #[test]
    fn gpu_cmd_draw_op_carries_tile_key_input_and_pipeline_id() {
        let cmd = GpuCmdMsg::DrawOp(DrawOp {
            tile_key: TileKey::from_parts(2, 3, 4),
            input: vec![1.0, 0.5, 9.0],
            pipeline_id: PipelineId(7),
        });

        match cmd {
            GpuCmdMsg::DrawOp(draw_op) => {
                assert_eq!(draw_op.tile_key, TileKey::from_parts(2, 3, 4));
                assert_eq!(draw_op.input, vec![1.0, 0.5, 9.0]);
                assert_eq!(draw_op.pipeline_id, PipelineId(7));
            }
            GpuCmdMsg::CopyOp(_) => panic!("expected draw op"),
            GpuCmdMsg::ClearOp(_) => panic!("expected draw op"),
            GpuCmdMsg::Notify => panic!("expected draw op"),
        }
    }

    #[test]
    fn gpu_cmd_copy_op_carries_source_and_destination_keys() {
        let cmd = GpuCmdMsg::CopyOp(CopyOp {
            src_tile_key: TileKey::from_parts(1, 2, 3),
            dst_tile_key: TileKey::from_parts(4, 5, 6),
        });

        match cmd {
            GpuCmdMsg::CopyOp(copy_op) => {
                assert_eq!(copy_op.src_tile_key, TileKey::from_parts(1, 2, 3));
                assert_eq!(copy_op.dst_tile_key, TileKey::from_parts(4, 5, 6));
            }
            GpuCmdMsg::DrawOp(_) => panic!("expected copy op"),
            GpuCmdMsg::ClearOp(_) => panic!("expected copy op"),
            GpuCmdMsg::Notify => panic!("expected copy op"),
        }
    }

    #[test]
    fn gpu_cmd_clear_op_carries_target_key() {
        let cmd = GpuCmdMsg::ClearOp(ClearOp {
            tile_key: TileKey::from_parts(9, 8, 7),
        });

        match cmd {
            GpuCmdMsg::ClearOp(clear_op) => {
                assert_eq!(clear_op.tile_key, TileKey::from_parts(9, 8, 7));
            }
            GpuCmdMsg::DrawOp(_) => panic!("expected clear op"),
            GpuCmdMsg::CopyOp(_) => panic!("expected clear op"),
            GpuCmdMsg::Notify => panic!("expected clear op"),
        }
    }
}
