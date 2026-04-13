mod image;
mod layout;
mod stored_image;

use atlas::TileKey;

pub use crate::image::{
    GlaImage, GlaImageCreateError, GlaImageTileAccessError, GlaImageTileRecBounds,
};
pub use crate::layout::{GlaImageLayout, GlaImageLayoutError};
pub use crate::stored_image::{GlaStoredImage, GlaStoredImageError};

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
    use atlas::TileKey;

    use crate::{GlaImageLayout, ImageId, ImageTileBinding, ImageTileSlot};

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
            tile_key: TileKey::EMPTY,
        };

        assert_eq!(binding.image_tile.image_id, ImageId(3));
        assert_eq!(binding.image_tile.tile_index, 9);
        assert_eq!(binding.tile_key, TileKey::EMPTY);
    }

    #[test]
    fn gla_image_layout_reports_total_tiles() {
        let layout = GlaImageLayout::new(63, 125);

        assert_eq!(layout.tile_x(), 2);
        assert_eq!(layout.tile_y(), 3);
        assert_eq!(layout.total_tiles(), 6);
    }
}
