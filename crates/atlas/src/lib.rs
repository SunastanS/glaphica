use gla_color::GlaFormat;
use gla_core::{ATLAS_TILE_SIZE, Pool};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileAddress {
    pub layer: usize,
    pub tile_x: usize,
    pub tile_y: usize,
}

impl TileAddress {
    pub const fn offset_x(self) -> usize {
        self.tile_x * ATLAS_TILE_SIZE as usize
    }

    pub const fn offset_y(self) -> usize {
        self.tile_y * ATLAS_TILE_SIZE as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TilePos {
    pub atlas_id: u8,
    pub layer: u32,
    pub tile_x: u32,
    pub tile_y: u32,
}

impl TilePos {
    #[inline]
    pub const fn new(atlas_id: u8, layer: u32, tile_x: u32, tile_y: u32) -> Self {
        Self {
            atlas_id,
            layer,
            tile_x,
            tile_y,
        }
    }

    #[inline]
    pub fn from_address(atlas_id: u8, address: TileAddress) -> Self {
        Self {
            atlas_id,
            layer: address.layer as u32,
            tile_x: address.tile_x as u32,
            tile_y: address.tile_y as u32,
        }
    }

    #[inline]
    pub fn atlas_id(self) -> u8 {
        self.atlas_id
    }

    #[inline]
    pub fn address(self) -> TileAddress {
        TileAddress {
            layer: self.layer as usize,
            tile_x: self.tile_x as usize,
            tile_y: self.tile_y as usize,
        }
    }

    #[inline]
    pub const fn offset_x(self) -> u32 {
        self.tile_x * ATLAS_TILE_SIZE
    }

    #[inline]
    pub const fn offset_y(self) -> u32 {
        self.tile_y * ATLAS_TILE_SIZE
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyBinding {
    atlas_id: u8,
    position: Option<TilePos>,
    tile_generation: u32,
}

impl KeyBinding {
    #[inline]
    pub(crate) fn new(position: TilePos, tile_gen: u32) -> Self {
        Self {
            atlas_id: position.atlas_id(),
            position: Some(position),
            tile_generation: tile_gen,
        }
    }

    #[inline]
    pub fn position(self) -> Option<TilePos> {
        self.position
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.position.is_none()
    }

    pub fn empty(atlas_id: u8) -> Self {
        Self {
            atlas_id,
            position: None,
            tile_generation: 0,
        }
    }

    #[inline]
    pub fn atlas_id(self) -> u8 {
        self.atlas_id
    }

    #[inline]
    pub fn tile_generation(self) -> u32 {
        self.tile_generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AtlasLayout {
    pub tiles_per_edge: usize,
    pub layer_num: usize,
}

impl AtlasLayout {
    // Names encode total slot exponent: TINY8 has 2^8 slots, not edge length 8.
    pub const TINY8: AtlasLayout = AtlasLayout {
        tiles_per_edge: 16,
        layer_num: 1,
    };
    pub const SMALL11: AtlasLayout = AtlasLayout {
        tiles_per_edge: 32,
        layer_num: 2,
    };
    pub const MEDIUM14: AtlasLayout = AtlasLayout {
        tiles_per_edge: 64,
        layer_num: 4,
    };
    pub const LARGE17: AtlasLayout = AtlasLayout {
        tiles_per_edge: 128,
        layer_num: 8,
    };
    pub const HUGE20: AtlasLayout = AtlasLayout {
        tiles_per_edge: 256,
        layer_num: 16,
    };

    pub const fn total_slots(self) -> usize {
        self.tiles_per_edge * self.tiles_per_edge * self.layer_num
    }

    pub const fn tiles_per_edge(self) -> usize {
        self.tiles_per_edge
    }

    pub const fn layer_num(self) -> usize {
        self.layer_num
    }

    pub fn index_to_address(self, index: usize) -> Result<TileAddress, AtlasLayoutError> {
        if index >= self.total_slots() {
            return Err(AtlasLayoutError::OutOfBounds);
        }

        let tiles_per_edge = self.tiles_per_edge;
        let slots_per_layer = tiles_per_edge * tiles_per_edge;
        let layer = index / slots_per_layer;
        let layer_slot = index % slots_per_layer;
        Ok(TileAddress {
            layer,
            tile_x: layer_slot % tiles_per_edge,
            tile_y: layer_slot / tiles_per_edge,
        })
    }

    pub fn address_to_index(self, address: TileAddress) -> Result<usize, AtlasLayoutError> {
        if address.layer >= self.layer_num
            || address.tile_y >= self.tiles_per_edge
            || address.tile_x >= self.tiles_per_edge
        {
            return Err(AtlasLayoutError::OutOfBounds);
        }

        let tiles_per_edge = self.tiles_per_edge;
        let index = address
            .layer
            .checked_mul(tiles_per_edge)
            .and_then(|v| v.checked_add(address.tile_y))
            .and_then(|v| v.checked_mul(tiles_per_edge))
            .and_then(|v| v.checked_add(address.tile_x))
            .ok_or(AtlasLayoutError::OutOfBounds)?;
        if index >= self.total_slots() {
            return Err(AtlasLayoutError::OutOfBounds);
        }
        Ok(index)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasLayoutError {
    OutOfBounds,
}

pub trait AtlasTextureStore {
    type Error;

    fn create_atlas_texture(
        &mut self,
        atlas_id: u8,
        layout: AtlasLayout,
        format: GlaFormat,
    ) -> Result<(), Self::Error>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoAtlasTextures;

impl AtlasTextureStore for NoAtlasTextures {
    type Error = std::convert::Infallible;

    fn create_atlas_texture(
        &mut self,
        _atlas_id: u8,
        _layout: AtlasLayout,
        _format: GlaFormat,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub struct Atlas {
    pub id: u8,
    pub layout: AtlasLayout,
    pub format: GlaFormat,
    tile_pool: Pool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasError {
    OutOfTiles { atlas_id: u8 },
    IdMismatch { atlas_id: u8 },
}

impl Atlas {
    pub fn new(id: u8, layout: AtlasLayout, format: GlaFormat) -> Self {
        Self {
            id,
            layout,
            format,
            tile_pool: Pool::new(layout.total_slots() as u32),
        }
    }

    pub fn alloc(&mut self) -> Result<KeyBinding, AtlasError> {
        let (index, tile_gen) = self
            .tile_pool
            .alloc()
            .map_err(|_| AtlasError::OutOfTiles { atlas_id: self.id })?;
        let address = self
            .layout
            .index_to_address(index as usize)
            .expect("allocated atlas slot must be inside atlas layout");
        Ok(KeyBinding::new(
            TilePos::from_address(self.id, address),
            tile_gen,
        ))
    }

    pub fn remaining(&self) -> u32 {
        self.tile_pool.remaining()
    }

    pub fn check(&self, binding: KeyBinding) -> bool {
        if binding.atlas_id() != self.id {
            return false;
        }
        let Some(position) = binding.position() else {
            return true;
        };
        let Ok(index) = self.layout.address_to_index(position.address()) else {
            return false;
        };
        self.tile_pool
            .check(index as u32, binding.tile_generation())
    }

    pub fn free(&mut self, position: TilePos) {
        assert_eq!(
            position.atlas_id(),
            self.id,
            "tile position atlas id does not match atlas"
        );
        let index = self
            .layout
            .address_to_index(position.address())
            .expect("freed tile position must be inside atlas layout");
        self.tile_pool.free(index as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::{Atlas, AtlasLayout, AtlasLayoutError, TileAddress, TilePos};
    use gla_color::{ChannelCount, ChannelType, GlaFormat};

    fn format() -> GlaFormat {
        GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::U8,
        }
    }

    #[test]
    fn allocated_binding_uses_one_based_generation() {
        let mut atlas = Atlas::new(3, AtlasLayout::TINY8, format());
        let binding = atlas.alloc().unwrap();

        assert_eq!(binding.tile_generation(), 1);
        assert!(atlas.check(binding));
    }

    #[test]
    #[should_panic(expected = "tile position atlas id does not match atlas")]
    fn free_panics_on_atlas_id_mismatch() {
        let mut atlas = Atlas::new(3, AtlasLayout::TINY8, format());

        atlas.free(TilePos::new(4, 0, 0, 0));
    }

    #[test]
    fn address_to_index_rejects_component_out_of_bounds() {
        let layout = AtlasLayout::TINY8;
        let edge = layout.tiles_per_edge();

        assert_eq!(
            layout.address_to_index(TileAddress {
                layer: 0,
                tile_x: edge,
                tile_y: 0,
            }),
            Err(AtlasLayoutError::OutOfBounds)
        );

        assert_eq!(
            layout.address_to_index(TileAddress {
                layer: 0,
                tile_x: 0,
                tile_y: edge,
            }),
            Err(AtlasLayoutError::OutOfBounds)
        );

        assert_eq!(
            layout.address_to_index(TileAddress {
                layer: layout.layer_num(),
                tile_x: 0,
                tile_y: 0,
            }),
            Err(AtlasLayoutError::OutOfBounds)
        );
    }
}
