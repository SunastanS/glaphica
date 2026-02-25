use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;
use smallvec::SmallVec;

/// This crate defines the bottom communication protocol of app thread and engine thread
/// Can be dependent by any crates
/// Should not depend on other crates

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputControlEvent {
    Notify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PresentFrameId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SubmitWaterline(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExecutedBatchWaterline(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CompleteWaterline(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuCmdMsg<Command> {
    Command(Command),
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
    /// Callers should prefer waterline-based feedback whenever possible and only rely on
    /// receipts/errors for non-contiguous events that cannot be represented by waterlines.
    pub receipts: SmallVec<[Receipt; 4]>,
    pub errors: SmallVec<[Error; 4]>,
}

/// Minimal contract for centralized vector merge:
/// - each module only defines how to compute identity key;
/// - protocol layer performs O(n) dedup and optional duplicate reconciliation
///   using caller-owned reusable hash indexes.
pub trait MergeItem: Sized {
    type MergeKey: Eq + Hash + Debug;

    /// Key must be stable for one logical entity across frames.
    fn merge_key(&self) -> Self::MergeKey;

    /// Duplicate reconciliation policy.
    /// The default implementation replaces the existing item with the incoming one.
    /// Override this method if you need custom merging logic.
    fn merge_duplicate(existing: &mut Self, incoming: Self) {
        *existing = incoming;
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

#[derive(Debug)]
pub struct GpuFeedbackMergeState<Receipt, Error> 
where
    Receipt: MergeItem,
    Error: MergeItem,
{
    pub receipt_index: MergeVecIndex<Receipt::MergeKey>,
    pub error_index: MergeVecIndex<Error::MergeKey>,
}

impl<Receipt, Error> Default for GpuFeedbackMergeState<Receipt, Error> 
where
    Receipt: MergeItem,
    Error: MergeItem,
{
    fn default() -> Self {
        Self {
            receipt_index: MergeVecIndex::default(),
            error_index: MergeVecIndex::default(),
        }
    }
}

pub fn merge_vec<Item>(
    current: &mut SmallVec<[Item; 4]>,
    incoming: SmallVec<[Item; 4]>,
    merge_index: &mut MergeVecIndex<Item::MergeKey>,
)
where
    Item: MergeItem,
{
    merge_index.clear();
    for (index, item) in current.iter().enumerate() {
        match merge_index.index_by_key.entry(item.merge_key()) {
            Entry::Vacant(slot) => {
                slot.insert(index);
            }
            Entry::Occupied(existing) => {
                panic!(
                    "current vector contains duplicated merge key before merge: key={:?}, first_index={}, duplicated_index={}",
                    existing.key(),
                    existing.get(),
                    index,
                );
            }
        }
    }

    current.reserve(incoming.len());
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
        merge_state: &mut GpuFeedbackMergeState<Receipt, Error>,
    ) -> Self {
        current.present_frame_id = current.present_frame_id.max(newer.present_frame_id);
        current.submit_waterline = current.submit_waterline.max(newer.submit_waterline);
        current.executed_batch_waterline = current
            .executed_batch_waterline
            .max(newer.executed_batch_waterline);
        current.complete_waterline = current.complete_waterline.max(newer.complete_waterline);
        merge_vec(&mut current.receipts, newer.receipts, &mut merge_state.receipt_index);
        merge_vec(&mut current.errors, newer.errors, &mut merge_state.error_index);
        current
    }
}

pub mod merge_test_support {
    use crate::MergeItem;

    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct TestReceipt {
        pub key: u64,
        pub payload_version: u64,
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

    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct TestError {
        pub key: u64,
    }

    impl MergeItem for TestError {
        type MergeKey = u64;

        fn merge_key(&self) -> Self::MergeKey {
            self.key
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CompleteWaterline, ExecutedBatchWaterline, GpuFeedbackFrame, GpuFeedbackMergeState,
        PresentFrameId, SubmitWaterline, merge_test_support::{TestError, TestReceipt},
    };

    #[test]
    fn mailbox_merge_is_absorptive_and_uses_max_waterlines() {
        let current = GpuFeedbackFrame {
            present_frame_id: PresentFrameId(10),
            submit_waterline: SubmitWaterline(2),
            executed_batch_waterline: ExecutedBatchWaterline(3),
            complete_waterline: CompleteWaterline(4),
            receipts: vec![TestReceipt {
                key: 1,
                payload_version: 10,
            }]
            .into(),
            errors: vec![TestError { key: 2 }].into(),
        };
        let newer = GpuFeedbackFrame {
            present_frame_id: PresentFrameId(9),
            submit_waterline: SubmitWaterline(20),
            executed_batch_waterline: ExecutedBatchWaterline(30),
            complete_waterline: CompleteWaterline(40),
            receipts: vec![
                TestReceipt {
                    key: 1,
                    payload_version: 11,
                },
                TestReceipt {
                    key: 3,
                    payload_version: 1,
                },
            ]
            .into(),
            errors: vec![TestError { key: 2 }, TestError { key: 4 }].into(),
        };

        let mut merge_state = GpuFeedbackMergeState::<TestReceipt, TestError>::default();
        let once = GpuFeedbackFrame::merge_mailbox(current, newer.clone(), &mut merge_state);
        let twice = GpuFeedbackFrame::merge_mailbox(once.clone(), newer, &mut merge_state);
        assert_eq!(once.present_frame_id, PresentFrameId(10));
        assert_eq!(once.submit_waterline, SubmitWaterline(20));
        assert_eq!(once.executed_batch_waterline, ExecutedBatchWaterline(30));
        assert_eq!(once.complete_waterline, CompleteWaterline(40));
        assert_eq!(once.receipts.len(), 2);
        assert_eq!(once.errors.len(), 2);
        assert_eq!(once.receipts[0].payload_version, 11);
        assert_eq!(once, twice);
    }

    #[test]
    fn mailbox_merge_is_idempotent_for_identical_frames() {
        let frame = GpuFeedbackFrame {
            present_frame_id: PresentFrameId(42),
            submit_waterline: SubmitWaterline(100),
            executed_batch_waterline: ExecutedBatchWaterline(101),
            complete_waterline: CompleteWaterline(102),
            receipts: vec![
                TestReceipt {
                    key: 1,
                    payload_version: 7,
                },
                TestReceipt {
                    key: 3,
                    payload_version: 2,
                },
            ]
            .into(),
            errors: vec![TestError { key: 9 }, TestError { key: 10 }].into(),
        };

        let mut merge_state = GpuFeedbackMergeState::<TestReceipt, TestError>::default();
        let merged = GpuFeedbackFrame::merge_mailbox(frame.clone(), frame.clone(), &mut merge_state);
        assert_eq!(merged, frame);
    }

    #[test]
    #[should_panic(expected = "current vector contains duplicated merge key before merge")]
    fn mailbox_merge_panics_when_current_contains_duplicated_keys() {
        let current = GpuFeedbackFrame {
            present_frame_id: PresentFrameId(1),
            submit_waterline: SubmitWaterline(1),
            executed_batch_waterline: ExecutedBatchWaterline(1),
            complete_waterline: CompleteWaterline(1),
            receipts: vec![
                TestReceipt {
                    key: 1,
                    payload_version: 1,
                },
                TestReceipt {
                    key: 1,
                    payload_version: 2,
                },
            ]
            .into(),
            errors: vec![TestError { key: 10 }].into(),
        };
        let newer = GpuFeedbackFrame {
            present_frame_id: PresentFrameId(2),
            submit_waterline: SubmitWaterline(2),
            executed_batch_waterline: ExecutedBatchWaterline(2),
            complete_waterline: CompleteWaterline(2),
            receipts: vec![TestReceipt {
                key: 2,
                payload_version: 1,
            }]
            .into(),
            errors: vec![TestError { key: 20 }].into(),
        };

        let mut merge_state = GpuFeedbackMergeState::<TestReceipt, TestError>::default();
        let _ = GpuFeedbackFrame::merge_mailbox(current, newer, &mut merge_state);
    }

    #[test]
    fn mailbox_merge_merges_duplicated_incoming_keys_with_item_policy() {
        let current = GpuFeedbackFrame {
            present_frame_id: PresentFrameId(1),
            submit_waterline: SubmitWaterline(1),
            executed_batch_waterline: ExecutedBatchWaterline(1),
            complete_waterline: CompleteWaterline(1),
            receipts: vec![TestReceipt {
                key: 7,
                payload_version: 5,
            }]
            .into(),
            errors: vec![TestError { key: 100 }].into(),
        };
        let newer = GpuFeedbackFrame {
            present_frame_id: PresentFrameId(2),
            submit_waterline: SubmitWaterline(2),
            executed_batch_waterline: ExecutedBatchWaterline(2),
            complete_waterline: CompleteWaterline(2),
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
            ]
            .into(),
            errors: vec![TestError { key: 200 }].into(),
        };

        let mut merge_state = GpuFeedbackMergeState::<TestReceipt, TestError>::default();
        let merged = GpuFeedbackFrame::merge_mailbox(current, newer, &mut merge_state);
        assert_eq!(merged.receipts.len(), 2);
        assert_eq!(merged.receipts[0].key, 7);
        assert_eq!(merged.receipts[0].payload_version, 8);
        assert_eq!(merged.receipts[1].key, 9);
        assert_eq!(merged.receipts[1].payload_version, 1);
    }
}
