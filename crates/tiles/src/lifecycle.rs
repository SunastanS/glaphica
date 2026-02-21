use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileReleaseBatch<Owner, Tile> {
    pub owner: Owner,
    pub tiles: Vec<Tile>,
}

pub trait TileLifecycleManager<Owner, Tile> {
    fn begin_shutdown(&mut self);
    fn drain_releasable(&mut self, now: Instant) -> Vec<TileReleaseBatch<Owner, Tile>>;
    fn force_release_all(&mut self) -> Vec<TileReleaseBatch<Owner, Tile>>;
    fn is_drained(&self) -> bool;
}

#[derive(Debug)]
struct RetainedBatch<Owner, Tile> {
    owner: Owner,
    tiles: Vec<Tile>,
    release_at: Instant,
}

#[derive(Debug)]
pub struct BufferTileLifecycle<Owner, Tile> {
    shutdown_started: bool,
    allocated_by_owner: HashMap<Owner, HashSet<Tile>>,
    pending_by_owner: HashMap<Owner, Vec<Tile>>,
    retained: Vec<RetainedBatch<Owner, Tile>>,
}

impl<Owner, Tile> Default for BufferTileLifecycle<Owner, Tile>
where
    Owner: Eq + Hash + Copy,
    Tile: Eq + Hash + Copy,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<Owner, Tile> BufferTileLifecycle<Owner, Tile>
where
    Owner: Eq + Hash + Copy,
    Tile: Eq + Hash + Copy,
{
    pub fn new() -> Self {
        Self {
            shutdown_started: false,
            allocated_by_owner: HashMap::new(),
            pending_by_owner: HashMap::new(),
            retained: Vec::new(),
        }
    }

    pub fn is_shutdown_started(&self) -> bool {
        self.shutdown_started
    }

    pub fn record_allocated(&mut self, owner: Owner, tile: Tile) -> bool {
        self.assert_accepting_new_requests("record_allocated");
        self.allocated_by_owner
            .entry(owner)
            .or_default()
            .insert(tile)
    }

    pub fn record_allocated_batch(
        &mut self,
        owner: Owner,
        tiles: impl IntoIterator<Item = Tile>,
    ) -> Vec<Tile> {
        self.assert_accepting_new_requests("record_allocated_batch");
        let allocated = self.allocated_by_owner.entry(owner).or_default();
        let mut new_tiles = Vec::new();
        for tile in tiles {
            if allocated.insert(tile) {
                new_tiles.push(tile);
            }
        }
        new_tiles
    }

    pub fn move_allocated_to_pending(&mut self, owner: Owner) {
        let Some(allocated) = self.allocated_by_owner.remove(&owner) else {
            return;
        };
        let previous = self
            .pending_by_owner
            .insert(owner, allocated.into_iter().collect());
        if previous.is_some() {
            panic!("pending tiles duplicated for owner");
        }
    }

    pub fn retain_pending_until(&mut self, owner: Owner, release_at: Instant) {
        self.assert_accepting_new_requests("retain_pending_until");
        let Some(pending) = self.pending_by_owner.remove(&owner) else {
            panic!("cannot retain owner without pending tiles");
        };
        self.retained.push(RetainedBatch {
            owner,
            tiles: pending,
            release_at,
        });
    }

    fn assert_accepting_new_requests(&self, stage: &'static str) {
        if self.shutdown_started {
            panic!(
                "tile lifecycle rejects new allocate/retain request after shutdown at stage {stage}"
            );
        }
    }
}

impl<Owner, Tile> TileLifecycleManager<Owner, Tile> for BufferTileLifecycle<Owner, Tile>
where
    Owner: Eq + Hash + Copy,
    Tile: Eq + Hash + Copy,
{
    fn begin_shutdown(&mut self) {
        self.shutdown_started = true;
    }

    fn drain_releasable(&mut self, now: Instant) -> Vec<TileReleaseBatch<Owner, Tile>> {
        let mut releasable = self
            .pending_by_owner
            .drain()
            .map(|(owner, tiles)| TileReleaseBatch { owner, tiles })
            .collect::<Vec<_>>();

        let mut retained_remaining = Vec::with_capacity(self.retained.len());
        for batch in self.retained.drain(..) {
            if batch.release_at > now {
                retained_remaining.push(batch);
                continue;
            }
            releasable.push(TileReleaseBatch {
                owner: batch.owner,
                tiles: batch.tiles,
            });
        }
        self.retained = retained_remaining;
        releasable
    }

    fn force_release_all(&mut self) -> Vec<TileReleaseBatch<Owner, Tile>> {
        let mut release_by_owner = HashMap::<Owner, HashSet<Tile>>::new();

        for (owner, allocated) in self.allocated_by_owner.drain() {
            release_by_owner.entry(owner).or_default().extend(allocated);
        }
        for (owner, pending) in self.pending_by_owner.drain() {
            release_by_owner.entry(owner).or_default().extend(pending);
        }
        for batch in self.retained.drain(..) {
            release_by_owner
                .entry(batch.owner)
                .or_default()
                .extend(batch.tiles);
        }

        release_by_owner
            .into_iter()
            .map(|(owner, tiles)| TileReleaseBatch {
                owner,
                tiles: tiles.into_iter().collect(),
            })
            .collect()
    }

    fn is_drained(&self) -> bool {
        self.allocated_by_owner.is_empty()
            && self.pending_by_owner.is_empty()
            && self.retained.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{BufferTileLifecycle, TileLifecycleManager};
    use std::time::{Duration, Instant};

    #[test]
    fn drain_releasable_includes_pending_and_due_retained_only() {
        let mut lifecycle = BufferTileLifecycle::<u64, i32>::new();
        let now = Instant::now();

        lifecycle.record_allocated_batch(11, [1, 2, 3]);
        lifecycle.move_allocated_to_pending(11);

        lifecycle.record_allocated_batch(22, [7, 8]);
        lifecycle.move_allocated_to_pending(22);
        lifecycle.retain_pending_until(22, now - Duration::from_millis(1));

        lifecycle.record_allocated_batch(33, [9]);
        lifecycle.move_allocated_to_pending(33);
        lifecycle.retain_pending_until(33, now + Duration::from_millis(500));

        let mut drained = lifecycle.drain_releasable(now);
        drained.sort_by_key(|batch| batch.owner);

        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].owner, 11);
        assert_eq!(drained[0].tiles.len(), 3);
        assert_eq!(drained[1].owner, 22);
        assert_eq!(drained[1].tiles.len(), 2);
        assert!(!lifecycle.is_drained());
    }

    #[test]
    fn force_release_all_is_idempotent() {
        let mut lifecycle = BufferTileLifecycle::<u64, i32>::new();
        let now = Instant::now();

        lifecycle.record_allocated_batch(11, [1, 2]);
        lifecycle.move_allocated_to_pending(11);
        lifecycle.record_allocated_batch(22, [5]);
        lifecycle.move_allocated_to_pending(22);
        lifecycle.retain_pending_until(22, now + Duration::from_secs(1));
        lifecycle.record_allocated(44, 9);

        let first = lifecycle.force_release_all();
        let second = lifecycle.force_release_all();

        assert!(!first.is_empty());
        assert!(second.is_empty());
        assert!(lifecycle.is_drained());
    }

    #[test]
    #[should_panic(expected = "rejects new allocate/retain request after shutdown")]
    fn begin_shutdown_rejects_new_allocate_request() {
        let mut lifecycle = BufferTileLifecycle::<u64, i32>::new();
        lifecycle.begin_shutdown();
        let _ = lifecycle.record_allocated(1, 10);
    }

    #[test]
    #[should_panic(expected = "rejects new allocate/retain request after shutdown")]
    fn begin_shutdown_rejects_new_retain_request() {
        let mut lifecycle = BufferTileLifecycle::<u64, i32>::new();
        let now = Instant::now();
        lifecycle.record_allocated(1, 10);
        lifecycle.move_allocated_to_pending(1);
        lifecycle.begin_shutdown();
        lifecycle.retain_pending_until(1, now);
    }
}
