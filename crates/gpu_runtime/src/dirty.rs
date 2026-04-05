use std::collections::HashSet;

use glaphica_core::TileKey;

#[derive(Debug, Default)]
pub struct TileDirtyTracker {
    dirty: HashSet<TileKey>,
}

impl TileDirtyTracker {
    pub fn mark(&mut self, tile_key: TileKey) {
        if tile_key != TileKey::EMPTY {
            self.dirty.insert(tile_key);
        }
    }

    pub fn clear(&mut self) {
        self.dirty.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = TileKey> + '_ {
        self.dirty.iter().copied()
    }

    pub fn is_empty(&self) -> bool {
        self.dirty.is_empty()
    }
}
