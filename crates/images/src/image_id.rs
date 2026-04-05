use std::sync::atomic::{AtomicU64, Ordering};

use glaphica_core::ImageId;

#[derive(Debug, Default)]
pub struct ImageIdAllocator {
    next_runtime_id: AtomicU64,
}

impl ImageIdAllocator {
    pub const fn new() -> Self {
        Self {
            next_runtime_id: AtomicU64::new(0),
        }
    }

    pub fn allocate(&self) -> ImageId {
        let id = self.next_runtime_id.fetch_add(1, Ordering::Relaxed);
        debug_assert_eq!(id & (1 << 63), 0);
        ImageId(id)
    }
}

#[cfg(test)]
mod tests {
    use super::ImageIdAllocator;

    #[test]
    fn image_id_allocator_stays_outside_node_namespace() {
        let allocator = ImageIdAllocator::new();
        let image_id = allocator.allocate();
        assert_eq!(image_id.node_id(), None);
    }
}
