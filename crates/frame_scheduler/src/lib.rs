#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSchedulerConfig {
    pub max_brush_commands_per_frame: u32,
    pub min_brush_commands_per_frame: u32,
}

impl Default for FrameSchedulerConfig {
    fn default() -> Self {
        Self {
            max_brush_commands_per_frame: 128,
            min_brush_commands_per_frame: 8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSchedulerInput {
    pub frame_sequence_id: u64,
    pub brush_hot_path_active: bool,
    pub pending_brush_commands: u32,
    pub previous_frame_gpu_micros: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerUpdateReason {
    BrushHotPathActivated,
    BrushHotPathTick,
    BrushHotPathDeactivated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSchedulerDecision {
    pub frame_sequence_id: u64,
    pub scheduler_active: bool,
    pub brush_commands_to_render: Option<u32>,
    pub update_reason: Option<SchedulerUpdateReason>,
}

#[derive(Debug, Clone)]
pub struct FrameScheduler {
    config: FrameSchedulerConfig,
    scheduler_active: bool,
}

impl FrameScheduler {
    pub fn new(config: FrameSchedulerConfig) -> Self {
        Self {
            config,
            scheduler_active: false,
        }
    }

    pub fn config(&self) -> FrameSchedulerConfig {
        self.config
    }

    pub fn is_active(&self) -> bool {
        self.scheduler_active
    }

    pub fn schedule_frame(&mut self, input: FrameSchedulerInput) -> FrameSchedulerDecision {
        if input.brush_hot_path_active {
            let was_inactive = !self.scheduler_active;
            self.scheduler_active = true;
            let brush_commands_to_render =
                self.brush_quota_for_pending(input.pending_brush_commands);
            return FrameSchedulerDecision {
                frame_sequence_id: input.frame_sequence_id,
                scheduler_active: self.scheduler_active,
                brush_commands_to_render: Some(brush_commands_to_render),
                update_reason: Some(if was_inactive {
                    SchedulerUpdateReason::BrushHotPathActivated
                } else {
                    SchedulerUpdateReason::BrushHotPathTick
                }),
            };
        }

        if self.scheduler_active {
            self.scheduler_active = false;
            return FrameSchedulerDecision {
                frame_sequence_id: input.frame_sequence_id,
                scheduler_active: self.scheduler_active,
                brush_commands_to_render: Some(0),
                update_reason: Some(SchedulerUpdateReason::BrushHotPathDeactivated),
            };
        }

        FrameSchedulerDecision {
            frame_sequence_id: input.frame_sequence_id,
            scheduler_active: self.scheduler_active,
            brush_commands_to_render: None,
            update_reason: None,
        }
    }

    fn brush_quota_for_pending(&self, pending_brush_commands: u32) -> u32 {
        if pending_brush_commands == 0 {
            return 0;
        }
        let floor = self.config.min_brush_commands_per_frame;
        let ceiling = self.config.max_brush_commands_per_frame;
        if floor > ceiling {
            panic!(
                "invalid frame scheduler config: min_brush_commands_per_frame ({floor}) exceeds max_brush_commands_per_frame ({ceiling})"
            );
        }
        pending_brush_commands.clamp(floor, ceiling)
    }
}

impl Default for FrameScheduler {
    fn default() -> Self {
        Self::new(FrameSchedulerConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activates_and_emits_quota_on_first_brush_hot_frame() {
        let mut scheduler = FrameScheduler::new(FrameSchedulerConfig {
            max_brush_commands_per_frame: 16,
            min_brush_commands_per_frame: 4,
        });

        let decision = scheduler.schedule_frame(FrameSchedulerInput {
            frame_sequence_id: 42,
            brush_hot_path_active: true,
            pending_brush_commands: 200,
            previous_frame_gpu_micros: None,
        });

        assert!(decision.scheduler_active);
        assert_eq!(decision.brush_commands_to_render, Some(16));
        assert_eq!(
            decision.update_reason,
            Some(SchedulerUpdateReason::BrushHotPathActivated)
        );
    }

    #[test]
    fn emits_quota_on_each_hot_path_frame() {
        let mut scheduler = FrameScheduler::default();

        let _first = scheduler.schedule_frame(FrameSchedulerInput {
            frame_sequence_id: 1,
            brush_hot_path_active: true,
            pending_brush_commands: 20,
            previous_frame_gpu_micros: None,
        });
        let second = scheduler.schedule_frame(FrameSchedulerInput {
            frame_sequence_id: 2,
            brush_hot_path_active: true,
            pending_brush_commands: 3,
            previous_frame_gpu_micros: None,
        });

        assert!(second.scheduler_active);
        assert_eq!(second.brush_commands_to_render, Some(8));
        assert_eq!(
            second.update_reason,
            Some(SchedulerUpdateReason::BrushHotPathTick)
        );
    }

    #[test]
    fn deactivates_and_emits_zero_quota() {
        let mut scheduler = FrameScheduler::default();

        let _ = scheduler.schedule_frame(FrameSchedulerInput {
            frame_sequence_id: 1,
            brush_hot_path_active: true,
            pending_brush_commands: 5,
            previous_frame_gpu_micros: None,
        });
        let decision = scheduler.schedule_frame(FrameSchedulerInput {
            frame_sequence_id: 2,
            brush_hot_path_active: false,
            pending_brush_commands: 0,
            previous_frame_gpu_micros: None,
        });

        assert!(!decision.scheduler_active);
        assert_eq!(decision.brush_commands_to_render, Some(0));
        assert_eq!(
            decision.update_reason,
            Some(SchedulerUpdateReason::BrushHotPathDeactivated)
        );
    }

    #[test]
    fn never_activates_without_brush_hot_path() {
        let mut scheduler = FrameScheduler::default();

        let decision = scheduler.schedule_frame(FrameSchedulerInput {
            frame_sequence_id: 7,
            brush_hot_path_active: false,
            pending_brush_commands: 0,
            previous_frame_gpu_micros: None,
        });

        assert!(!decision.scheduler_active);
        assert_eq!(decision.brush_commands_to_render, None);
        assert_eq!(decision.update_reason, None);
    }

    #[test]
    fn uses_zero_quota_for_empty_pending_commands() {
        let mut scheduler = FrameScheduler::default();

        let decision = scheduler.schedule_frame(FrameSchedulerInput {
            frame_sequence_id: 77,
            brush_hot_path_active: true,
            pending_brush_commands: 0,
            previous_frame_gpu_micros: None,
        });

        assert_eq!(decision.brush_commands_to_render, Some(0));
    }
}
