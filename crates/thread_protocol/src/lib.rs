pub use glaphica_core::{AtlasLayout, BrushId, EpochId, InputDeviceKind, MappedCursor, TileKey};

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
    pub epoch: EpochId,
    pub time_ns: u64,
    pub device: InputDeviceKind,
    pub cursor: MappedCursor,
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

mod gpu_command;
pub use gpu_command::{ClearOp, CopyOp, DrawOp, GpuCmdMsg, RefImage};

mod gpu_feedback;
pub use gpu_feedback::{
    CompleteWaterline, ExecutedBatchWaterline, GpuFeedbackFrame, GpuFeedbackMergeState, MergeItem,
    SubmitWaterline,
};

#[cfg(test)]
mod tests {
    use super::{
        BrushId, ClearOp, CompleteWaterline, CopyOp, DrawOp, ExecutedBatchWaterline, GpuCmdMsg,
        GpuFeedbackFrame, GpuFeedbackMergeState, InputControlEvent, InputControlOp, MergeItem,
        RefImage, SubmitWaterline, TileKey,
    };

    use glaphica_core::PresentFrameId;

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
    fn gpu_cmd_draw_op_carries_tile_key_input_and_brush_id() {
        let cmd = GpuCmdMsg::DrawOp(DrawOp {
            tile_key: TileKey::from_parts(2, 3, 4),
            ref_image: Some(RefImage {
                tile_key: TileKey::from_parts(8, 9, 10),
            }),
            input: vec![1.0, 0.5, 9.0],
            brush_id: BrushId(7),
        });

        match cmd {
            GpuCmdMsg::DrawOp(draw_op) => {
                assert_eq!(draw_op.tile_key, TileKey::from_parts(2, 3, 4));
                assert_eq!(
                    draw_op.ref_image,
                    Some(RefImage {
                        tile_key: TileKey::from_parts(8, 9, 10)
                    })
                );
                assert_eq!(draw_op.input, vec![1.0, 0.5, 9.0]);
                assert_eq!(draw_op.brush_id, BrushId(7));
            }
            GpuCmdMsg::CopyOp(_) => panic!("expected draw op"),
            GpuCmdMsg::ClearOp(_) => panic!("expected draw op"),
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
        }
    }
}
