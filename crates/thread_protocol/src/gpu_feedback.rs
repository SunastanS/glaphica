use glaphica_core::PresentFrameId;
use std::collections::HashMap;
use std::hash::Hash;
use std::marker::PhantomData;
use std::{cmp::Ordering, fmt};

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
