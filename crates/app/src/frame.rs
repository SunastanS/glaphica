use std::time::Duration;

use crate::{BrushInput, EditorRenderUpdate, MainBrushInputConsumer};

pub struct AppFrameScheduler {
    pending_brush_inputs: Vec<BrushInput>,
    scheduled_tile_indices: Vec<usize>,
    redraw_requested: bool,
}

impl AppFrameScheduler {
    pub fn new() -> Self {
        Self {
            pending_brush_inputs: Vec::new(),
            scheduled_tile_indices: Vec::new(),
            redraw_requested: false,
        }
    }

    pub fn request_redraw(&mut self) {
        self.redraw_requested = true;
    }

    pub fn has_requested_redraw(&self) -> bool {
        self.redraw_requested
    }

    pub fn reset_redraw_request(&mut self) {
        self.redraw_requested = false;
    }

    pub fn clear_pending_brush_inputs(&mut self) {
        self.pending_brush_inputs.clear();
    }

    pub fn drain_brush_inputs(
        &mut self,
        consumer: &MainBrushInputConsumer,
        max_inputs: usize,
        wait_timeout: Duration,
    ) -> usize {
        self.pending_brush_inputs.clear();
        consumer.drain_batch_with_wait(&mut self.pending_brush_inputs, max_inputs, wait_timeout);
        self.pending_brush_inputs.len()
    }

    pub fn pending_brush_inputs(&self) -> &[BrushInput] {
        &self.pending_brush_inputs
    }

    pub fn finish_brush_inputs(&mut self) {
        self.pending_brush_inputs.clear();
    }

    pub fn schedule_render_update(&mut self, update: &EditorRenderUpdate) {
        self.schedule_tile_indices(update.tile_indices());
    }

    pub fn schedule_tile_indices(&mut self, tile_indices: &[usize]) {
        self.scheduled_tile_indices.extend_from_slice(tile_indices);
        self.redraw_requested = true;
    }

    pub fn take_scheduled_tile_indices(&mut self) -> Vec<usize> {
        if self.scheduled_tile_indices.is_empty() {
            return Vec::new();
        }
        self.scheduled_tile_indices.sort_unstable();
        self.scheduled_tile_indices.dedup();
        std::mem::take(&mut self.scheduled_tile_indices)
    }
}

impl Default for AppFrameScheduler {
    fn default() -> Self {
        Self::new()
    }
}
