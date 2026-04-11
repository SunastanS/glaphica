#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct TileKey(u64);

impl TileKey {
    pub const EMPTY: Self = Self(u64::MAX);

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const fn new(raw: u64) -> Self {
        Self(raw)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ImageId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageTileSlot {
    pub image_id: ImageId,
    pub tile_index: usize,
}

impl ImageTileSlot {
    pub const fn new(image_id: ImageId, tile_index: usize) -> Self {
        Self {
            image_id,
            tile_index,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageTileBinding {
    pub image_tile: ImageTileSlot,
    pub tile_key: TileKey,
}

#[cfg(test)]
mod tests {
    use super::{ImageId, ImageTileBinding, ImageTileSlot, TileKey};

    #[test]
    fn image_tile_slot_keeps_image_and_tile_index() {
        let slot = ImageTileSlot::new(ImageId(7), 3);

        assert_eq!(slot.image_id, ImageId(7));
        assert_eq!(slot.tile_index, 3);
    }

    #[test]
    fn image_tile_binding_keeps_slot_and_tile_key_together() {
        let binding = ImageTileBinding {
            image_tile: ImageTileSlot::new(ImageId(3), 9),
            tile_key: TileKey::new(0x1234_5678_9ABC_DEF0),
        };

        assert_eq!(binding.image_tile.image_id, ImageId(3));
        assert_eq!(binding.image_tile.tile_index, 9);
        assert_eq!(binding.tile_key, TileKey::new(0x1234_5678_9ABC_DEF0));
    }

    #[test]
    fn crate_can_construct_tile_key() {
        let key = TileKey::new(0x1234_5678_9ABC_DEF0);

        assert_eq!(key, TileKey(0x1234_5678_9ABC_DEF0));
    }

    #[test]
    fn empty_is_all_ones() {
        assert_eq!(TileKey::EMPTY, TileKey(u64::MAX));
    }
}
