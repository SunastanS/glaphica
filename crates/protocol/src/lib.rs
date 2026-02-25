/// This crate defines the buttom communication protocol of app thread and engine thread
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

/// To avoid the protocol layer being "oversmart", we left the merge action to be implemented by upper layers
/// This function should auto remove the repeated elements
/// Avoid native `iter().any()` dedup, which is O(n^2)
pub trait MergeVec: Sized {
    fn merge_vec(current: &mut Vec<Self>, incoming: Vec<Self>);
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
        CompleteWaterline, ExecutedBatchWaterline, GpuFeedbackFrame, MergeVec, PresentFrameId,
        SubmitWaterline,
    };

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestReceipt {
        key: u64,
    }

    impl MergeVec for TestReceipt {
        fn merge_vec(current: &mut Vec<Self>, incoming: Vec<Self>) {
            for item in incoming {
                if !current.iter().any(|existing| existing.key == item.key) {
                    current.push(item);
                }
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestError {
        key: u64,
    }

    impl MergeVec for TestError {
        fn merge_vec(current: &mut Vec<Self>, incoming: Vec<Self>) {
            for item in incoming {
                // WARN: Do not use this method in production code for bad performance.
                if !current.iter().any(|existing| existing.key == item.key) {
                    current.push(item);
                }
            }
        }
    }

    #[test]
    fn mailbox_merge_is_idempotent_and_uses_max_waterlines() {
        let current = GpuFeedbackFrame {
            present_frame_id: PresentFrameId(10),
            submit_waterline: SubmitWaterline(2),
            executed_batch_waterline: ExecutedBatchWaterline(3),
            complete_waterline: CompleteWaterline(4),
            receipts: vec![TestReceipt { key: 1 }],
            errors: vec![TestError { key: 2 }],
        };
        let newer = GpuFeedbackFrame {
            present_frame_id: PresentFrameId(9),
            submit_waterline: SubmitWaterline(20),
            executed_batch_waterline: ExecutedBatchWaterline(30),
            complete_waterline: CompleteWaterline(40),
            receipts: vec![TestReceipt { key: 1 }, TestReceipt { key: 3 }],
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
        assert_eq!(once, twice);
    }
}
