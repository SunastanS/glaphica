use crate::TileKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageId(pub u64);

impl ImageId {
    const NODE_NAMESPACE_BIT: u64 = 1 << 63;

    pub const fn from_node_id(node_id: NodeId) -> Self {
        Self(Self::NODE_NAMESPACE_BIT | node_id.0)
    }

    pub const fn node_id(self) -> Option<NodeId> {
        if (self.0 & Self::NODE_NAMESPACE_BIT) == 0 {
            return None;
        }
        Some(NodeId(self.0 & !Self::NODE_NAMESPACE_BIT))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageTileKey {
    pub image_id: ImageId,
    pub tile_index: usize,
}

impl ImageTileKey {
    pub const fn new(image_id: ImageId, tile_index: usize) -> Self {
        Self {
            image_id,
            tile_index,
        }
    }

    pub const fn from_node_tile(node_id: NodeId, tile_index: usize) -> Self {
        Self::new(ImageId::from_node_id(node_id), tile_index)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageTileBinding {
    pub image_tile: ImageTileKey,
    pub tile_key: TileKey,
}

#[cfg(test)]
mod tests {
    use super::{ImageId, ImageTileBinding, ImageTileKey, NodeId};
    use crate::TileKey;

    #[test]
    fn image_id_round_trips_node_ids() {
        let image_id = ImageId::from_node_id(NodeId(7));
        assert_eq!(image_id.node_id(), Some(NodeId(7)));
    }

    #[test]
    fn image_tile_binding_keeps_logical_and_physical_keys_together() {
        let binding = ImageTileBinding {
            image_tile: ImageTileKey::from_node_tile(NodeId(3), 9),
            tile_key: TileKey::from_parts(1, 2, 3),
        };

        assert_eq!(binding.image_tile.image_id.node_id(), Some(NodeId(3)));
        assert_eq!(binding.image_tile.tile_index, 9);
        assert_eq!(binding.tile_key, TileKey::from_parts(1, 2, 3));
    }
}
