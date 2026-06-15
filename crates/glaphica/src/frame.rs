#[derive(Debug, Default)]
pub(crate) struct AppFrameScheduler {
    redraw_requested: bool,
}

impl AppFrameScheduler {
    pub(crate) fn new() -> Self {
        Self {
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
}
