use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::{EpochId, PresentFrameId, StrokeId};

pub struct PresentFrameIdAllocator {
    counter: AtomicU64,
}

impl PresentFrameIdAllocator {
    pub const fn new() -> Self {
        Self {
            counter: AtomicU64::new(0),
        }
    }

    pub fn allocate(&self) -> PresentFrameId {
        PresentFrameId(self.counter.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for PresentFrameIdAllocator {
    fn default() -> Self {
        Self::new()
    }
}

pub struct StrokeIdAllocator {
    counter: AtomicU64,
}

impl StrokeIdAllocator {
    pub const fn new() -> Self {
        Self {
            counter: AtomicU64::new(0),
        }
    }

    pub fn allocate(&self) -> StrokeId {
        StrokeId(self.counter.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for StrokeIdAllocator {
    fn default() -> Self {
        Self::new()
    }
}

pub struct EpochIdAllocator {
    counter: AtomicU32,
}

impl EpochIdAllocator {
    pub const fn new() -> Self {
        Self {
            counter: AtomicU32::new(0),
        }
    }

    pub fn allocate(&self) -> EpochId {
        EpochId(self.counter.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for EpochIdAllocator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn present_frame_id_allocator_increments() {
        let allocator = PresentFrameIdAllocator::new();
        assert_eq!(allocator.allocate(), PresentFrameId(0));
        assert_eq!(allocator.allocate(), PresentFrameId(1));
        assert_eq!(allocator.allocate(), PresentFrameId(2));
    }

    #[test]
    fn stroke_id_allocator_increments() {
        let allocator = StrokeIdAllocator::new();
        assert_eq!(allocator.allocate(), StrokeId(0));
        assert_eq!(allocator.allocate(), StrokeId(1));
        assert_eq!(allocator.allocate(), StrokeId(2));
    }

    #[test]
    fn epoch_id_allocator_increments() {
        let allocator = EpochIdAllocator::new();
        assert_eq!(allocator.allocate(), EpochId(0));
        assert_eq!(allocator.allocate(), EpochId(1));
        assert_eq!(allocator.allocate(), EpochId(2));
    }

    #[test]
    fn allocators_are_thread_safe() {
        use std::sync::Arc;
        use std::thread;

        let allocator = Arc::new(StrokeIdAllocator::new());
        let mut handles = vec![];

        for _ in 0..4 {
            let allocator_clone = Arc::clone(&allocator);
            handles.push(thread::spawn(move || {
                let mut ids = vec![];
                for _ in 0..100 {
                    ids.push(allocator_clone.allocate());
                }
                ids
            }));
        }

        let mut all_ids: Vec<u64> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .map(|id| id.0)
            .collect();

        all_ids.sort();
        all_ids.dedup();

        assert_eq!(all_ids.len(), 400);
        assert_eq!(all_ids.first(), Some(&0));
        assert_eq!(all_ids.last(), Some(&399));
    }
}
