use atlas::{Atlas, AtlasError, AtlasLayout, AtlasTextureStore, KeyBinding, TilePos};
use gla_color::GlaFormat;
use gla_core::{Pool, PoolError};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::num::NonZeroU64;

/// Owning tile resource token.
///
/// Moving a `Tile` transfers ownership of the resource identity. The packed
/// value is non-zero so `Option<Tile>` remains pointer-sized for cache slots.
#[derive(Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Tile(NonZeroU64);

impl Tile {
    const INDEX_BITS: u32 = 32;
    const INDEX_MASK: u64 = (1 << Self::INDEX_BITS) - 1;
    const GENERATION_SHIFT: u32 = Self::INDEX_BITS;

    #[inline]
    fn new(index: u32, generation: u32) -> Self {
        assert_ne!(generation, 0, "tile generation 0 is reserved");
        debug_assert!(index as u64 <= Self::INDEX_MASK);
        let raw = ((generation as u64) << Self::GENERATION_SHIFT) | index as u64;
        Self(NonZeroU64::new(raw).expect("non-zero tile generation makes tile id non-zero"))
    }

    #[inline]
    fn index(&self) -> u32 {
        (self.0.get() & Self::INDEX_MASK) as u32
    }

    #[inline]
    fn generation(&self) -> u32 {
        (self.0.get() >> Self::GENERATION_SHIFT) as u32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileReadRef {
    Zero,
    Physical(TilePos),
}

pub struct Tiles {
    atlases: Vec<Atlas>, // index == atlas id
    key_pool: Pool,
    bindings: Vec<KeyBinding>,
}

#[derive(Debug)]
pub enum NewAtlasError<E> {
    TooManyAtlases,
    Texture(E),
}

impl<E: Display> Display for NewAtlasError<E> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyAtlases => f.write_str("too many atlases"),
            Self::Texture(error) => write!(f, "atlas texture creation failed: {error}"),
        }
    }
}

impl<E> Error for NewAtlasError<E> where E: Error + 'static {}

#[derive(Debug)]
pub enum TilesError {
    AtlasOutOfTiles { atlas_id: u8 },
    KeyPoolFull,
    InvalidAtlasId { atlas_id: u8 },
    MissingAtlasForFormat { format: GlaFormat },
    InvalidTile { index: u32, generation: u32 },
    TileGenerationMismatch { index: u32, generation: u32 },
    TileBindingMismatch { index: u32, generation: u32 },
}

impl Display for TilesError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AtlasOutOfTiles { atlas_id } => write!(f, "atlas {atlas_id} out of tiles"),
            Self::KeyPoolFull => f.write_str("key pool is full"),
            Self::InvalidAtlasId { atlas_id } => write!(f, "invalid atlas id {atlas_id}"),
            Self::MissingAtlasForFormat { format } => {
                write!(f, "missing atlas for format {format:?}")
            }
            Self::InvalidTile { index, generation } => {
                write!(f, "invalid tile index {index} generation {generation}")
            }
            Self::TileGenerationMismatch { index, generation } => {
                write!(
                    f,
                    "tile generation mismatch for index {index} generation {generation}"
                )
            }
            Self::TileBindingMismatch { index, generation } => {
                write!(
                    f,
                    "tile binding mismatch for index {index} generation {generation}"
                )
            }
        }
    }
}

impl Error for TilesError {}

impl Tiles {
    pub fn new() -> Self {
        Self {
            atlases: Vec::new(),
            key_pool: Pool::new(u32::MAX),
            bindings: Vec::new(),
        }
    }

    pub fn new_atlas<S>(
        &mut self,
        layout: AtlasLayout,
        format: GlaFormat,
        textures: &mut S,
    ) -> Result<u8, NewAtlasError<S::Error>>
    where
        S: AtlasTextureStore,
    {
        let atlas_id =
            u8::try_from(self.atlases.len()).map_err(|_| NewAtlasError::TooManyAtlases)?;
        textures
            .create_atlas_texture(atlas_id, layout, format)
            .map_err(NewAtlasError::Texture)?;
        self.atlases.push(Atlas::new(atlas_id, layout, format));
        Ok(atlas_id)
    }

    pub fn atlas(&self, atlas_id: u8) -> Option<&Atlas> {
        self.atlases.get(atlas_id as usize)
    }

    pub fn atlases(&self) -> &[Atlas] {
        &self.atlases
    }

    pub fn atlas_for_format(&self, format: GlaFormat) -> Option<u8> {
        self.atlases
            .iter()
            .find(|atlas| atlas.format == format)
            .map(|atlas| atlas.id)
    }

    pub fn reserve_for_format(&mut self, format: GlaFormat) -> Result<Tile, TilesError> {
        let atlas_id = self
            .atlas_for_format(format)
            .ok_or(TilesError::MissingAtlasForFormat { format })?;
        self.reserve(atlas_id)
    }

    pub fn reserve_batch_for_format(
        &mut self,
        format: GlaFormat,
        count: u32,
    ) -> Result<Vec<Tile>, TilesError> {
        let atlas_id = self
            .atlas_for_format(format)
            .ok_or(TilesError::MissingAtlasForFormat { format })?;
        self.reserve_batch(atlas_id, count)
    }

    pub fn reserve(&mut self, atlas_id: u8) -> Result<Tile, TilesError> {
        self.atlases
            .get(atlas_id as usize)
            .ok_or(TilesError::InvalidAtlasId { atlas_id })?;

        let (index, generation) = self.key_pool.alloc()?;
        let tile = Tile::new(index, generation);
        self.bind_tile(&tile, KeyBinding::empty(atlas_id));
        Ok(tile)
    }

    pub fn reserve_batch(&mut self, atlas_id: u8, count: u32) -> Result<Vec<Tile>, TilesError> {
        self.atlases
            .get(atlas_id as usize)
            .ok_or(TilesError::InvalidAtlasId { atlas_id })?;
        if self.key_pool.remaining() < count {
            return Err(TilesError::KeyPoolFull);
        }

        let mut tiles = Vec::with_capacity(count as usize);
        for _ in 0..count {
            tiles.push(self.reserve(atlas_id)?);
        }
        Ok(tiles)
    }

    pub fn read_ref(&self, tile: &Tile) -> Result<TileReadRef, TilesError> {
        self.ensure_valid(tile)?;
        let binding = self.bindings[tile.index() as usize];
        if binding.is_empty() {
            Ok(TileReadRef::Zero)
        } else {
            Ok(TileReadRef::Physical(binding.position()))
        }
    }

    pub fn write_pos(&mut self, tile: &mut Tile) -> Result<TilePos, TilesError> {
        self.write_pos_with_zero_init(tile, |_| {})
    }

    pub fn write_pos_with_zero_init(
        &mut self,
        tile: &mut Tile,
        init_zero: impl FnOnce(TilePos),
    ) -> Result<TilePos, TilesError> {
        self.ensure_valid(tile)?;
        let binding = self.bindings[tile.index() as usize];
        let position = binding.position();
        if !position.is_empty() {
            return Ok(position);
        }

        let atlas_id = position.atlas_id();
        let atlas = self
            .atlases
            .get_mut(atlas_id as usize)
            .ok_or(TilesError::InvalidAtlasId { atlas_id })?;
        if atlas.remaining() == 0 {
            return Err(TilesError::AtlasOutOfTiles { atlas_id });
        }

        let binding = atlas.alloc()?;
        let position = binding.position();
        self.bind_tile(tile, binding);
        init_zero(position);
        Ok(position)
    }

    pub fn release(&mut self, tile: Tile) {
        self.ensure_valid(&tile)
            .expect("released tile must be a valid owned tile");
        let idx = tile.index() as usize;
        let binding = self.bindings[idx];
        if !binding.is_empty() {
            let atlas_id = binding.position().atlas_id();
            let atlas = self
                .atlases
                .get_mut(atlas_id as usize)
                .expect("validated tile binding must reference an atlas");
            assert!(
                atlas.check(binding),
                "validated tile binding must match atlas allocation"
            );
            atlas.free(binding.position());
        }
        self.key_pool.free(tile.index());
    }

    pub fn release_optional(&mut self, tile: Option<Tile>) {
        if let Some(tile) = tile {
            self.release(tile);
        }
    }

    fn ensure_valid(&self, tile: &Tile) -> Result<(), TilesError> {
        let index = tile.index();
        let generation = tile.generation();
        self.key_pool
            .check(index, generation)
            .then_some(())
            .ok_or(TilesError::TileGenerationMismatch { index, generation })?;
        let binding = self
            .bindings
            .get(index as usize)
            .ok_or(TilesError::InvalidTile { index, generation })?;
        let atlas = self
            .atlases
            .get(binding.position().atlas_id() as usize)
            .ok_or(TilesError::InvalidAtlasId {
                atlas_id: binding.position().atlas_id(),
            })?;
        (binding.is_empty() || atlas.check(*binding))
            .then_some(())
            .ok_or(TilesError::TileBindingMismatch { index, generation })?;
        Ok(())
    }

    fn bind_tile(&mut self, tile: &Tile, binding: KeyBinding) {
        let index = tile.index() as usize;
        if self.bindings.len() <= index {
            self.bindings
                .resize(index + 1, KeyBinding::empty(binding.position().atlas_id()));
        }
        self.bindings[index] = binding;
    }
}

impl Default for Tiles {
    fn default() -> Self {
        Self::new()
    }
}

impl From<AtlasError> for TilesError {
    fn from(error: AtlasError) -> Self {
        match error {
            AtlasError::OutOfTiles { atlas_id } => TilesError::AtlasOutOfTiles { atlas_id },
            AtlasError::IdMismatch { .. } => {
                unreachable!("Tiles invariant violated: atlas_id mismatch")
            }
        }
    }
}

impl From<PoolError> for TilesError {
    fn from(error: PoolError) -> Self {
        match error {
            PoolError::Full => TilesError::KeyPoolFull,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Tile, TileReadRef, Tiles, TilesError};
    use atlas::{AtlasLayout, NoAtlasTextures};
    use gla_color::{ChannelCount, ChannelType, GlaFormat};
    use std::mem::size_of;

    fn format() -> GlaFormat {
        GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::U8,
        }
    }

    fn new_test_atlas(tiles: &mut Tiles) -> u8 {
        let mut textures = NoAtlasTextures;
        tiles
            .new_atlas(AtlasLayout::TINY8, format(), &mut textures)
            .unwrap()
    }

    #[test]
    fn option_tile_uses_non_zero_niche() {
        assert_eq!(size_of::<Option<Tile>>(), size_of::<Tile>());
        assert_eq!(size_of::<Tile>(), size_of::<u64>());
    }

    #[test]
    fn reserve_creates_valid_zero_content_tile_without_atlas_slot() {
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);

        let tile = tiles.reserve(atlas_id).unwrap();

        assert_eq!(tile.index(), 0);
        assert_eq!(tile.generation(), 1);
        assert_eq!(tiles.atlas(atlas_id).unwrap().remaining(), 256);
        assert_eq!(tiles.read_ref(&tile).unwrap(), TileReadRef::Zero);
    }

    #[test]
    fn reserve_for_format_uses_matching_atlas() {
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);

        let tile = tiles.reserve_for_format(format()).unwrap();

        assert_eq!(tiles.atlas_for_format(format()), Some(atlas_id));
        assert_eq!(tiles.read_ref(&tile).unwrap(), TileReadRef::Zero);
    }

    #[test]
    fn reserve_for_format_rejects_missing_atlas() {
        let mut tiles = Tiles::new();

        let err = tiles.reserve_for_format(format()).unwrap_err();

        assert!(matches!(
            err,
            TilesError::MissingAtlasForFormat { format: missing } if missing == format()
        ));
    }

    #[test]
    fn write_pos_materializes_empty_tile_once() {
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let mut tile = tiles.reserve(atlas_id).unwrap();

        let first = tiles.write_pos(&mut tile).unwrap();
        let second = tiles.write_pos(&mut tile).unwrap();

        assert_eq!(first, second);
        assert_eq!(tiles.atlas(atlas_id).unwrap().remaining(), 255);
        assert_eq!(tiles.read_ref(&tile).unwrap(), TileReadRef::Physical(first));
    }

    #[test]
    fn write_pos_with_zero_init_initializes_only_on_first_materialization() {
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let mut tile = tiles.reserve(atlas_id).unwrap();
        let mut initialized = Vec::new();

        let first = tiles
            .write_pos_with_zero_init(&mut tile, |pos| initialized.push(pos))
            .unwrap();
        let second = tiles
            .write_pos_with_zero_init(&mut tile, |pos| initialized.push(pos))
            .unwrap();

        assert_eq!(first, second);
        assert_eq!(initialized, vec![first]);
        assert_eq!(tiles.read_ref(&tile).unwrap(), TileReadRef::Physical(first));
    }

    #[test]
    fn release_empty_tile_reuses_identity_with_next_generation() {
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let tile = tiles.reserve(atlas_id).unwrap();

        tiles.release(tile);
        let next = tiles.reserve(atlas_id).unwrap();

        assert_eq!(next.index(), 0);
        assert_eq!(next.generation(), 2);
        assert_eq!(tiles.read_ref(&next).unwrap(), TileReadRef::Zero);
    }

    #[test]
    fn release_physical_tile_frees_atlas_slot() {
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let mut tile = tiles.reserve(atlas_id).unwrap();

        tiles.write_pos(&mut tile).unwrap();
        assert_eq!(tiles.atlas(atlas_id).unwrap().remaining(), 255);

        tiles.release(tile);
        assert_eq!(tiles.atlas(atlas_id).unwrap().remaining(), 256);
    }

    #[test]
    fn reserve_batch_is_all_or_nothing_when_key_pool_is_full() {
        let mut tiles = Tiles {
            atlases: Vec::new(),
            key_pool: gla_core::Pool::new(1),
            bindings: Vec::new(),
        };
        let atlas_id = new_test_atlas(&mut tiles);

        let err = tiles.reserve_batch(atlas_id, 2).unwrap_err();

        assert!(matches!(err, TilesError::KeyPoolFull));
        assert_eq!(tiles.reserve(atlas_id).unwrap().index(), 0);
    }

    #[test]
    fn release_optional_none_is_noop() {
        let mut tiles = Tiles::new();

        tiles.release_optional(None);
    }

    #[test]
    #[should_panic(expected = "released tile must be a valid owned tile")]
    fn release_panics_on_stale_generation() {
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let tile = tiles.reserve(atlas_id).unwrap();
        let stale = Tile::new(tile.index(), tile.generation());

        tiles.release(tile);
        tiles.release(stale);
    }
}
