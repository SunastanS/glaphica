use std::collections::HashSet;
use std::hash::Hash;
use std::cmp::Ordering;

pub trait DedupKey {
    type Key: Eq + Hash;

    fn dedup_key(&self) -> Self::Key;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSignal {
    Notify,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputRingSample {
    pub epoch: u32,
    pub cursor_x: f32,
    pub cursor_y: f32,
    pub pressure: f32,
    pub tilt: f32,
    pub twist: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuCmdMsg<Command> {
    Command(Command),
}

#[derive(Debug, Clone)]
pub struct PrioritizedInputSignal<Control> {
    pub priority: u8,
    pub sequence: u64,
    pub control: Control,
}

impl<Control> PrioritizedInputSignal<Control> {
    pub fn new(priority: u8, sequence: u64, control: Control) -> Self {
        Self {
            priority,
            sequence,
            control,
        }
    }
}

impl<Control> PartialEq for PrioritizedInputSignal<Control> {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.sequence == other.sequence
    }
}

impl<Control> Eq for PrioritizedInputSignal<Control> {}

impl<Control> PartialOrd for PrioritizedInputSignal<Control> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<Control> Ord for PrioritizedInputSignal<Control> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuFeedbackFrame<Receipt, Error> {
    pub present_frame_id: u64,
    pub submit_waterline: u64,
    pub executed_batch_waterline: u64,
    pub complete_waterline: u64,
    pub receipts: Vec<Receipt>,
    pub errors: Vec<Error>,
}

impl<Receipt, Error> GpuFeedbackFrame<Receipt, Error>
where
    Receipt: DedupKey,
    Error: DedupKey,
{
    pub fn merge_mailbox(mut current: Self, newer: Self) -> Self {
        current.present_frame_id = current.present_frame_id.max(newer.present_frame_id);
        current.submit_waterline = current.submit_waterline.max(newer.submit_waterline);
        current.executed_batch_waterline = current
            .executed_batch_waterline
            .max(newer.executed_batch_waterline);
        current.complete_waterline = current.complete_waterline.max(newer.complete_waterline);
        merge_unique_by_key(&mut current.receipts, newer.receipts);
        merge_unique_by_key(&mut current.errors, newer.errors);
        current
    }
}

fn merge_unique_by_key<T>(current: &mut Vec<T>, incoming: Vec<T>)
where
    T: DedupKey,
{
    let mut existing_keys: HashSet<T::Key> = current.iter().map(T::dedup_key).collect();
    for item in incoming {
        let item_key = item.dedup_key();
        if existing_keys.insert(item_key) {
            current.push(item);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DedupKey, GpuFeedbackFrame};

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestReceipt {
        key: u64,
    }

    impl DedupKey for TestReceipt {
        type Key = u64;

        fn dedup_key(&self) -> Self::Key {
            self.key
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestError {
        key: u64,
    }

    impl DedupKey for TestError {
        type Key = u64;

        fn dedup_key(&self) -> Self::Key {
            self.key
        }
    }

    #[test]
    fn mailbox_merge_is_idempotent_and_uses_max_waterlines() {
        let current = GpuFeedbackFrame {
            present_frame_id: 10,
            submit_waterline: 2,
            executed_batch_waterline: 3,
            complete_waterline: 4,
            receipts: vec![TestReceipt { key: 1 }],
            errors: vec![TestError { key: 2 }],
        };
        let newer = GpuFeedbackFrame {
            present_frame_id: 9,
            submit_waterline: 20,
            executed_batch_waterline: 30,
            complete_waterline: 40,
            receipts: vec![TestReceipt { key: 1 }, TestReceipt { key: 3 }],
            errors: vec![TestError { key: 2 }, TestError { key: 4 }],
        };

        let once = GpuFeedbackFrame::merge_mailbox(current, newer.clone());
        let twice = GpuFeedbackFrame::merge_mailbox(once.clone(), newer);
        assert_eq!(once.present_frame_id, 10);
        assert_eq!(once.submit_waterline, 20);
        assert_eq!(once.executed_batch_waterline, 30);
        assert_eq!(once.complete_waterline, 40);
        assert_eq!(once.receipts.len(), 2);
        assert_eq!(once.errors.len(), 2);
        assert_eq!(once, twice);
    }
}
