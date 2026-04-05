use std::collections::HashSet;

use glaphica_core::{ImageTileKey, NodeId};

#[derive(Debug, Default)]
pub struct ImageDirtyTracker {
    dirty: HashSet<ImageTileKey>,
}

impl ImageDirtyTracker {
    pub fn mark(&mut self, image_tile: ImageTileKey) {
        self.dirty.insert(image_tile);
    }

    pub fn mark_node_tile(&mut self, node_id: NodeId, tile_index: usize) {
        self.mark(ImageTileKey::from_node_tile(node_id, tile_index));
    }

    pub fn clear(&mut self) {
        self.dirty.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = ImageTileKey> + '_ {
        self.dirty.iter().copied()
    }

    pub fn is_empty(&self) -> bool {
        self.dirty.is_empty()
    }
}
