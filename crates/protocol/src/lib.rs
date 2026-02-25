use std::collections::HashMap;
use std::hash::Hash;

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
    pub receipts: Vec<Receipt>,
    pub errors: Vec<Error>,
}

/// Vector merge abstraction consumed by mailbox merge.
/// `MergeItem` provides the default implementation used by feature modules.
pub trait MergeVec: Sized {
    fn merge_vec(current: &mut Vec<Self>, incoming: Vec<Self>);
}

/// Minimal contract for centralized vector merge:
/// - each module only defines how to compute identity key;
/// - protocol layer performs O(n) dedup and optional duplicate reconciliation.
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

impl<Item> MergeVec for Item
where
    Item: MergeItem,
{
    fn merge_vec(current: &mut Vec<Self>, incoming: Vec<Self>) {
        let mut index_by_key = HashMap::with_capacity(current.len() + incoming.len());
        for (index, item) in current.iter().enumerate() {
            let duplicated_index = index_by_key.insert(item.merge_key(), index);
            if duplicated_index.is_some() {
                panic!("current vector contains duplicated merge key before merge");
            }
        }

        for incoming_item in incoming {
            let key = incoming_item.merge_key();
            match index_by_key.get(&key).copied() {
                Some(existing_index) => {
                    Item::merge_duplicate(&mut current[existing_index], incoming_item);
                }
                None => {
                    let new_index = current.len();
                    current.push(incoming_item);
                    index_by_key.insert(key, new_index);
                }
            }
        }
    }
}

impl<Receipt, Error> GpuFeedbackFrame<Receipt, Error>
where
    Receipt: MergeVec,
    Error: MergeVec,
{
    pub fn merge_mailbox(mut current: Self, newer: Self) -> Self {
        current.present_frame_id = current.present_frame_id.max(newer.present_frame_id);
        current.submit_waterline = current.submit_waterline.max(newer.submit_waterline);
        current.executed_batch_waterline = current
            .executed_batch_waterline
            .max(newer.executed_batch_waterline);
        current.complete_waterline = current.complete_waterline.max(newer.complete_waterline);
        Receipt::merge_vec(&mut current.receipts, newer.receipts);
        Error::merge_vec(&mut current.errors, newer.errors);
        current
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CompleteWaterline, ExecutedBatchWaterline, GpuFeedbackFrame, MergeItem, PresentFrameId,
        SubmitWaterline,
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
            submit_waterline: SubmitWaterline(2),
            executed_batch_waterline: ExecutedBatchWaterline(3),
            complete_waterline: CompleteWaterline(4),
            receipts: vec![TestReceipt {
                key: 1,
                payload_version: 10,
            }],
            errors: vec![TestError { key: 2 }],
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
            ],
            errors: vec![TestError { key: 2 }, TestError { key: 4 }],
        };

        let once = GpuFeedbackFrame::merge_mailbox(current, newer.clone());
        let twice = GpuFeedbackFrame::merge_mailbox(once.clone(), newer);
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
            ],
            errors: vec![TestError { key: 10 }],
        };
        let newer = GpuFeedbackFrame {
            present_frame_id: PresentFrameId(2),
            submit_waterline: SubmitWaterline(2),
            executed_batch_waterline: ExecutedBatchWaterline(2),
            complete_waterline: CompleteWaterline(2),
            receipts: vec![TestReceipt {
                key: 2,
                payload_version: 1,
            }],
            errors: vec![TestError { key: 20 }],
        };

        let _ = GpuFeedbackFrame::merge_mailbox(current, newer);
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
            }],
            errors: vec![TestError { key: 100 }],
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
            ],
            errors: vec![TestError { key: 200 }],
        };

        let merged = GpuFeedbackFrame::merge_mailbox(current, newer);
        assert_eq!(merged.receipts.len(), 2);
        assert_eq!(merged.receipts[0].key, 7);
        assert_eq!(merged.receipts[0].payload_version, 8);
        assert_eq!(merged.receipts[1].key, 9);
        assert_eq!(merged.receipts[1].payload_version, 1);
    }
}
