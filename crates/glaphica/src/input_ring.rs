use std::cell::Cell;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug)]
struct RingState<T> {
    queue: VecDeque<T>,
    pushed: u64,
    dropped: u64,
}

#[derive(Debug)]
struct SharedOverwriteRing<T> {
    capacity: usize,
    state: Mutex<RingState<T>>,
    notify: Condvar,
}

pub struct OverwriteRingProducer<T> {
    shared: Arc<SharedOverwriteRing<T>>,
    _spsc_marker: Cell<()>,
    _not_clone: PhantomData<*const ()>,
}

pub struct OverwriteRingConsumer<T> {
    shared: Arc<SharedOverwriteRing<T>>,
    _spsc_marker: Cell<()>,
    _not_clone: PhantomData<*const ()>,
}

unsafe impl<T: Send> Send for OverwriteRingProducer<T> {}
unsafe impl<T: Send> Send for OverwriteRingConsumer<T> {}

impl<T> SharedOverwriteRing<T> {
    fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "ring capacity must be greater than zero");
        Self {
            capacity,
            state: Mutex::new(RingState {
                queue: VecDeque::with_capacity(capacity),
                pushed: 0,
                dropped: 0,
            }),
            notify: Condvar::new(),
        }
    }
}

impl<T> OverwriteRingProducer<T> {
    pub fn push(&self, value: T) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("overwrite ring state should not be poisoned");
        if state.queue.len() >= self.shared.capacity {
            state.queue.pop_front();
            state.dropped = state.dropped.saturating_add(1);
        }
        state.queue.push_back(value);
        state.pushed = state.pushed.saturating_add(1);
        drop(state);
        self.shared.notify.notify_one();
    }

    pub fn pushed_items(&self) -> u64 {
        self.shared
            .state
            .lock()
            .expect("overwrite ring state should not be poisoned")
            .pushed
    }

    pub fn dropped_items(&self) -> u64 {
        self.shared
            .state
            .lock()
            .expect("overwrite ring state should not be poisoned")
            .dropped
    }

    pub fn clear(&self) {
        self.shared
            .state
            .lock()
            .expect("overwrite ring state should not be poisoned")
            .queue
            .clear();
    }
}

impl<T> OverwriteRingConsumer<T> {
    pub fn drain_batch_with_wait(
        &self,
        output: &mut Vec<T>,
        max_items: usize,
        wait_timeout: Duration,
    ) {
        if max_items == 0 {
            return;
        }

        output.reserve(max_items);
        let mut state = self
            .shared
            .state
            .lock()
            .expect("overwrite ring state should not be poisoned");
        if state.queue.is_empty() && !wait_timeout.is_zero() {
            let deadline = Instant::now() + wait_timeout;
            while state.queue.is_empty() {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                let remaining = deadline.saturating_duration_since(now);
                let (next_state, timeout) = self
                    .shared
                    .notify
                    .wait_timeout(state, remaining)
                    .expect("overwrite ring wait should not be poisoned");
                state = next_state;
                if timeout.timed_out() {
                    break;
                }
            }
        }

        for _ in 0..max_items {
            let Some(value) = state.queue.pop_front() else {
                break;
            };
            output.push(value);
        }
    }

    pub fn pushed_items(&self) -> u64 {
        self.shared
            .state
            .lock()
            .expect("overwrite ring state should not be poisoned")
            .pushed
    }

    pub fn dropped_items(&self) -> u64 {
        self.shared
            .state
            .lock()
            .expect("overwrite ring state should not be poisoned")
            .dropped
    }

    pub fn clear(&self) {
        self.shared
            .state
            .lock()
            .expect("overwrite ring state should not be poisoned")
            .queue
            .clear();
    }
}

pub fn create_overwrite_ring<T>(
    capacity: usize,
) -> (OverwriteRingProducer<T>, OverwriteRingConsumer<T>) {
    let shared = Arc::new(SharedOverwriteRing::new(capacity));
    (
        OverwriteRingProducer {
            shared: shared.clone(),
            _spsc_marker: Cell::new(()),
            _not_clone: PhantomData,
        },
        OverwriteRingConsumer {
            shared,
            _spsc_marker: Cell::new(()),
            _not_clone: PhantomData,
        },
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::create_overwrite_ring;

    #[test]
    fn overwrite_ring_drains_newest_values_when_full() {
        let (producer, consumer) = create_overwrite_ring(2);
        let mut output = Vec::new();

        producer.push(1);
        producer.push(2);
        producer.push(3);

        consumer.drain_batch_with_wait(&mut output, 8, Duration::ZERO);

        assert_eq!(output, vec![2, 3]);
        assert_eq!(producer.pushed_items(), 3);
        assert_eq!(producer.dropped_items(), 1);
        assert_eq!(consumer.pushed_items(), 3);
        assert_eq!(consumer.dropped_items(), 1);
    }

    #[test]
    fn overwrite_ring_clear_drops_pending_values_without_resetting_counters() {
        let (producer, consumer) = create_overwrite_ring(2);
        let mut output = Vec::new();

        producer.push(1);
        producer.push(2);
        consumer.clear();
        producer.push(3);
        consumer.drain_batch_with_wait(&mut output, 8, Duration::ZERO);

        assert_eq!(output, vec![3]);
        assert_eq!(producer.pushed_items(), 3);
        assert_eq!(producer.dropped_items(), 0);
    }
}
