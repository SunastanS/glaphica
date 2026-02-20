use render_protocol::{
    MergeAuditMeta, MergeExecutionResult, RendererSubmissionId, StrokeExecutionReceipt,
    StrokeExecutionReceiptId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileMergeCompletionNoticeId {
    pub renderer_submission_id: RendererSubmissionId,
    pub frame_id: u64,
    pub op_trace_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileMergeCompletionNotice {
    pub notice_id: TileMergeCompletionNoticeId,
    pub receipt_id: StrokeExecutionReceiptId,
    pub audit_meta: MergeAuditMeta,
    pub result: MergeExecutionResult,
}

impl TileMergeCompletionNotice {
    pub fn new(
        receipt_id: StrokeExecutionReceiptId,
        audit_meta: MergeAuditMeta,
        result: MergeExecutionResult,
    ) -> Self {
        Self {
            notice_id: TileMergeCompletionNoticeId {
                renderer_submission_id: audit_meta.renderer_submission_id,
                frame_id: audit_meta.frame_id,
                op_trace_id: audit_meta.op_trace_id,
            },
            receipt_id,
            audit_meta,
            result,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileMergeAckFailure {
    pub receipt_id: StrokeExecutionReceiptId,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TileMergeBatchAck {
    pub total: usize,
    pub succeeded: usize,
    pub failed: Vec<TileMergeAckFailure>,
}

impl TileMergeBatchAck {
    pub fn record_ack_success(&mut self) {
        self.total = self.total.checked_add(1).expect("merge ack total overflow");
        self.succeeded = self
            .succeeded
            .checked_add(1)
            .expect("merge ack success count overflow");
    }

    pub fn record_ack_failure(&mut self, receipt_id: StrokeExecutionReceiptId, message: String) {
        self.total = self.total.checked_add(1).expect("merge ack total overflow");
        self.failed.push(TileMergeAckFailure {
            receipt_id,
            message,
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileMergeTerminalUpdate {
    pub receipt: StrokeExecutionReceipt,
    pub result: MergeExecutionResult,
}

pub trait TileMergeCompletionCallback {
    type Error;

    fn on_renderer_merge_completion(
        &mut self,
        notice: TileMergeCompletionNotice,
    ) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use render_protocol::{MergeExecutionResult, RendererSubmissionId, StrokeExecutionReceiptId};

    #[test]
    fn batch_ack_tracks_success_and_failure_without_frame_field() {
        let mut batch = TileMergeBatchAck::default();
        batch.record_ack_success();
        batch.record_ack_failure(StrokeExecutionReceiptId(22), "ack rejected".to_owned());

        assert_eq!(batch.total, 2);
        assert_eq!(batch.succeeded, 1);
        assert_eq!(batch.failed.len(), 1);
        assert_eq!(batch.failed[0].receipt_id, StrokeExecutionReceiptId(22));
        assert_eq!(batch.failed[0].message, "ack rejected");
    }

    #[test]
    fn completion_notice_keeps_audit_and_result_payload() {
        let notice = TileMergeCompletionNotice::new(
            StrokeExecutionReceiptId(5),
            MergeAuditMeta {
                frame_id: 10,
                renderer_submission_id: RendererSubmissionId(7),
                op_trace_id: Some(99),
            },
            MergeExecutionResult::Succeeded,
        );

        assert_eq!(notice.receipt_id, StrokeExecutionReceiptId(5));
        assert_eq!(notice.notice_id.frame_id, 10);
        assert_eq!(notice.audit_meta.frame_id, 10);
        assert_eq!(
            notice.audit_meta.renderer_submission_id,
            RendererSubmissionId(7)
        );
        assert_eq!(notice.audit_meta.op_trace_id, Some(99));
        assert_eq!(notice.result, MergeExecutionResult::Succeeded);
    }

    struct CaptureCallback {
        notices: Vec<TileMergeCompletionNotice>,
    }

    impl TileMergeCompletionCallback for CaptureCallback {
        type Error = &'static str;

        fn on_renderer_merge_completion(
            &mut self,
            notice: TileMergeCompletionNotice,
        ) -> Result<(), Self::Error> {
            self.notices.push(notice);
            Ok(())
        }
    }

    #[test]
    fn callback_trait_accepts_renderer_driven_completion() {
        let mut callback = CaptureCallback {
            notices: Vec::new(),
        };
        let notice = TileMergeCompletionNotice::new(
            StrokeExecutionReceiptId(42),
            MergeAuditMeta {
                frame_id: 8,
                renderer_submission_id: RendererSubmissionId(11),
                op_trace_id: None,
            },
            MergeExecutionResult::Failed {
                message: "gpu lost".to_owned(),
            },
        );

        callback
            .on_renderer_merge_completion(notice.clone())
            .expect("callback should accept notice");

        assert_eq!(callback.notices.len(), 1);
        assert_eq!(callback.notices[0], notice);
    }
}
