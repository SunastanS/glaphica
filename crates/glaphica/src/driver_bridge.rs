use std::time::Instant;

use driver::{
    DriverEngine, DriverEventError, FrameDispatchSignal, FramedDabChunk,
    NoSmoothingUniformResampling, NoSmoothingUniformResamplingConfig, PointerDeviceKind,
    PointerEventPhase, RawPointerInput,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputFrameStats {
    pub total_events: u32,
    pub down_events: u32,
    pub move_events: u32,
    pub up_events: u32,
    pub cancel_events: u32,
    pub hover_events: u32,
    pub handle_time_micros_total: u64,
}

impl InputFrameStats {
    fn observe_event(&mut self, phase: PointerEventPhase) {
        self.total_events = self
            .total_events
            .checked_add(1)
            .expect("input event count overflow");
        match phase {
            PointerEventPhase::Down => {
                self.down_events = self
                    .down_events
                    .checked_add(1)
                    .expect("down event count overflow");
            }
            PointerEventPhase::Move => {
                self.move_events = self
                    .move_events
                    .checked_add(1)
                    .expect("move event count overflow");
            }
            PointerEventPhase::Up => {
                self.up_events = self
                    .up_events
                    .checked_add(1)
                    .expect("up event count overflow");
            }
            PointerEventPhase::Cancel => {
                self.cancel_events = self
                    .cancel_events
                    .checked_add(1)
                    .expect("cancel event count overflow");
            }
            PointerEventPhase::Hover => {
                self.hover_events = self
                    .hover_events
                    .checked_add(1)
                    .expect("hover event count overflow");
            }
        }
    }

    fn observe_handle_duration(&mut self, elapsed: std::time::Duration) {
        let elapsed_micros =
            u64::try_from(elapsed.as_micros()).expect("event handle micros overflow");
        self.handle_time_micros_total = self
            .handle_time_micros_total
            .checked_add(elapsed_micros)
            .expect("input handle time overflow");
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputFrameStats {
    pub chunk_count: u32,
    pub dab_count: u32,
    pub discontinuity_chunk_count: u32,
    pub dropped_chunk_count_before_total: u32,
}

impl OutputFrameStats {
    fn from_chunks(chunks: &[FramedDabChunk]) -> Self {
        let mut stats = Self::default();
        for framed_chunk in chunks {
            let chunk = &framed_chunk.chunk;
            stats.chunk_count = stats
                .chunk_count
                .checked_add(1)
                .expect("chunk count overflow");
            stats.dab_count = stats
                .dab_count
                .checked_add(u32::try_from(chunk.dab_count()).expect("dab count overflow"))
                .expect("dab count overflow");
            if chunk.discontinuity_before {
                stats.discontinuity_chunk_count = stats
                    .discontinuity_chunk_count
                    .checked_add(1)
                    .expect("discontinuity count overflow");
            }
            stats.dropped_chunk_count_before_total = stats
                .dropped_chunk_count_before_total
                .checked_add(u32::from(chunk.dropped_chunk_count_before))
                .expect("dropped chunk count overflow");
        }
        stats
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameDrainStats {
    pub frame_sequence_id: u64,
    pub input: InputFrameStats,
    pub output: OutputFrameStats,
}

impl FrameDrainStats {
    pub fn has_activity(&self) -> bool {
        self.input.total_events > 0 || self.output.chunk_count > 0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameDrainResult {
    pub stats: FrameDrainStats,
    pub chunks: Vec<FramedDabChunk>,
}

pub struct DriverUiBridge {
    engine: DriverEngine<NoSmoothingUniformResampling>,
    pointer_id: u64,
    clock_start: Instant,
    pending_input_stats: InputFrameStats,
}

impl DriverUiBridge {
    pub fn new(queue_capacity: usize, spacing_pixels: f32) -> Result<Self, DriverEventError> {
        let engine = DriverEngine::new(
            queue_capacity,
            NoSmoothingUniformResampling::new,
            NoSmoothingUniformResamplingConfig { spacing_pixels },
        )?;
        Ok(Self {
            engine,
            pointer_id: 1,
            clock_start: Instant::now(),
            pending_input_stats: InputFrameStats::default(),
        })
    }

    pub fn ingest_mouse_event(
        &mut self,
        phase: PointerEventPhase,
        screen_x: f32,
        screen_y: f32,
    ) -> Result<(), DriverEventError> {
        let timestamp_micros = self
            .clock_start
            .elapsed()
            .as_micros()
            .try_into()
            .expect("timestamp micros overflow");
        let input = RawPointerInput {
            pointer_id: self.pointer_id,
            device_kind: PointerDeviceKind::Mouse,
            phase,
            timestamp_micros,
            screen_x,
            screen_y,
            pressure: Some(1.0),
            tilt_x_degrees: None,
            tilt_y_degrees: None,
            twist_degrees: None,
        };
        self.ingest_pointer_event(input)
    }

    pub fn ingest_pointer_event(&mut self, input: RawPointerInput) -> Result<(), DriverEventError> {
        self.pending_input_stats.observe_event(input.phase);
        let handle_start = Instant::now();
        let result = self.engine.handle_pointer_event(input);
        self.pending_input_stats
            .observe_handle_duration(handle_start.elapsed());
        result
    }

    pub fn drain_frame(&mut self, frame_sequence_id: u64) -> FrameDrainResult {
        let chunks = self
            .engine
            .dispatch_frame(FrameDispatchSignal { frame_sequence_id });
        let input = std::mem::take(&mut self.pending_input_stats);
        let output = OutputFrameStats::from_chunks(&chunks);
        FrameDrainResult {
            stats: FrameDrainStats {
                frame_sequence_id,
                input,
                output,
            },
            chunks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_input(
        phase: PointerEventPhase,
        timestamp_micros: u64,
        x: f32,
        y: f32,
    ) -> RawPointerInput {
        RawPointerInput {
            pointer_id: 1,
            device_kind: PointerDeviceKind::Mouse,
            phase,
            timestamp_micros,
            screen_x: x,
            screen_y: y,
            pressure: Some(1.0),
            tilt_x_degrees: None,
            tilt_y_degrees: None,
            twist_degrees: None,
        }
    }

    #[test]
    fn drain_frame_reports_input_phase_counts_and_resets() {
        let mut bridge = DriverUiBridge::new(8, 2.0).expect("create bridge");

        bridge
            .ingest_pointer_event(test_input(PointerEventPhase::Down, 1, 0.0, 0.0))
            .expect("down");
        bridge
            .ingest_pointer_event(test_input(PointerEventPhase::Move, 2, 3.0, 0.0))
            .expect("move");
        bridge
            .ingest_pointer_event(test_input(PointerEventPhase::Up, 3, 4.0, 0.0))
            .expect("up");

        let first = bridge.drain_frame(10);
        assert_eq!(first.stats.frame_sequence_id, 10);
        assert_eq!(first.stats.input.total_events, 3);
        assert_eq!(first.stats.input.down_events, 1);
        assert_eq!(first.stats.input.move_events, 1);
        assert_eq!(first.stats.input.up_events, 1);
        assert!(first.stats.output.chunk_count > 0);

        let second = bridge.drain_frame(11);
        assert_eq!(second.stats.input.total_events, 0);
        assert_eq!(second.stats.output.chunk_count, 0);
    }

    #[test]
    fn drain_frame_reports_discontinuity_and_drop_counts() {
        let mut bridge = DriverUiBridge::new(1, 1.0).expect("create bridge");

        bridge
            .ingest_pointer_event(test_input(PointerEventPhase::Down, 1, 0.0, 0.0))
            .expect("down");
        bridge
            .ingest_pointer_event(test_input(PointerEventPhase::Move, 2, 20.0, 0.0))
            .expect("move");
        bridge
            .ingest_pointer_event(test_input(PointerEventPhase::Up, 3, 21.0, 0.0))
            .expect("up");

        let drained = bridge.drain_frame(20);
        assert_eq!(drained.stats.frame_sequence_id, 20);
        assert_eq!(drained.stats.output.chunk_count, 1);
        assert!(drained.stats.output.discontinuity_chunk_count >= 1);
        assert!(drained.stats.output.dropped_chunk_count_before_total >= 1);
    }
}
