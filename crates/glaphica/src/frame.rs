#[derive(Debug, Default)]
pub(crate) struct AppFrameScheduler {
    scheduled_tile_indices: Vec<u32>,
    redraw_requested: bool,
}

impl AppFrameScheduler {
    pub(crate) fn new() -> Self {
        Self {
            scheduled_tile_indices: Vec::new(),
            redraw_requested: false,
        }
    }

    pub(crate) fn request_redraw(&mut self) {
        self.redraw_requested = true;
    }

    pub(crate) fn has_requested_redraw(&self) -> bool {
        self.redraw_requested
    }

    pub(crate) fn reset_redraw_request(&mut self) {
        self.redraw_requested = false;
    }

    pub(crate) fn schedule_tile_indices(&mut self, tile_indices: &[u32]) {
        self.scheduled_tile_indices.extend_from_slice(tile_indices);
        self.redraw_requested = true;
    }

    pub(crate) fn take_scheduled_tile_indices(&mut self) -> Vec<u32> {
        if self.scheduled_tile_indices.is_empty() {
            return Vec::new();
        }
        self.scheduled_tile_indices.sort_unstable();
        self.scheduled_tile_indices.dedup();
        std::mem::take(&mut self.scheduled_tile_indices)
    }
}

#[cfg(test)]
mod tests {
    use super::AppFrameScheduler;

    #[test]
    fn redraw_request_latches_until_reset() {
        let mut scheduler = AppFrameScheduler::new();

        assert!(!scheduler.has_requested_redraw());
        scheduler.request_redraw();
        assert!(scheduler.has_requested_redraw());
        scheduler.reset_redraw_request();
        assert!(!scheduler.has_requested_redraw());
    }

    #[test]
    fn scheduled_tile_indices_are_deduplicated_and_request_redraw() {
        let mut scheduler = AppFrameScheduler::new();

        scheduler.schedule_tile_indices(&[3, 1, 3, 2]);

        assert!(scheduler.has_requested_redraw());
        assert_eq!(scheduler.take_scheduled_tile_indices(), vec![1, 2, 3]);
        assert!(scheduler.take_scheduled_tile_indices().is_empty());
    }
}
