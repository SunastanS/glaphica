#[derive(Debug, Default)]
pub(crate) struct AppFrameScheduler {
    scheduled_tile_indices: Vec<u32>,
    full_update_requested: bool,
    redraw_requested: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ScreenUpdateRequest {
    None,
    Full,
    Tiles(Vec<u32>),
}

impl AppFrameScheduler {
    pub(crate) fn new() -> Self {
        Self {
            scheduled_tile_indices: Vec::new(),
            full_update_requested: false,
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

    pub(crate) fn schedule_full_update(&mut self) {
        self.full_update_requested = true;
        self.scheduled_tile_indices.clear();
        self.redraw_requested = true;
    }

    pub(crate) fn take_screen_update_request(&mut self) -> ScreenUpdateRequest {
        if self.full_update_requested {
            self.full_update_requested = false;
            self.scheduled_tile_indices.clear();
            return ScreenUpdateRequest::Full;
        }
        if self.scheduled_tile_indices.is_empty() {
            return ScreenUpdateRequest::None;
        }
        self.scheduled_tile_indices.sort_unstable();
        self.scheduled_tile_indices.dedup();
        ScreenUpdateRequest::Tiles(std::mem::take(&mut self.scheduled_tile_indices))
    }
}

#[cfg(test)]
mod tests {
    use super::{AppFrameScheduler, ScreenUpdateRequest};

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
        assert_eq!(
            scheduler.take_screen_update_request(),
            ScreenUpdateRequest::Tiles(vec![1, 2, 3])
        );
        assert_eq!(
            scheduler.take_screen_update_request(),
            ScreenUpdateRequest::None
        );
    }

    #[test]
    fn full_update_takes_precedence_over_tile_indices() {
        let mut scheduler = AppFrameScheduler::new();

        scheduler.schedule_tile_indices(&[1, 2]);
        scheduler.schedule_full_update();

        assert_eq!(
            scheduler.take_screen_update_request(),
            ScreenUpdateRequest::Full
        );
        assert_eq!(
            scheduler.take_screen_update_request(),
            ScreenUpdateRequest::None
        );
    }
}
