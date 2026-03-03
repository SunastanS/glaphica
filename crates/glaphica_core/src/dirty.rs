use std::collections::HashSet;

use crate::TileKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageDirtyKey {
    pub node_id: NodeId,
    pub tile_index: usize,
}

#[derive(Debug, Default)]
pub struct ImageDirtyTracker {
    dirty: HashSet<ImageDirtyKey>,
}

impl ImageDirtyTracker {
    pub fn mark(&mut self, node_id: NodeId, tile_index: usize) {
        self.dirty.insert(ImageDirtyKey {
            node_id,
            tile_index,
        });
    }

    pub fn clear(&mut self) {
        self.dirty.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = ImageDirtyKey> + '_ {
        self.dirty.iter().copied()
    }

    pub fn is_empty(&self) -> bool {
        self.dirty.is_empty()
    }
}

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
