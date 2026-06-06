use atlas::{Atlas, AtlasError, AtlasLayout, KeyBinding, TilePos};
use gla_color::GlaFormat;
use gla_core::{Pool, PoolError};

/// Key (u32):
/// - [0,  32) key index
/// - [32, 64) key generation

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct TileKey(u64);

impl TileKey {
    const INDEX_BITS: u32 = 32;
    const INDEX_MASK: u64 = (1 << Self::INDEX_BITS) - 1;
    const GENERATION_SHIFT: u32 = Self::INDEX_BITS;

    #[inline]
    pub fn new(index: u32, generation: u32) -> Self {
        debug_assert!(index as u64 <= Self::INDEX_MASK);
        Self(((generation as u64) << Self::GENERATION_SHIFT) | index as u64)
    }

    #[inline]
    pub fn index(self) -> u32 {
        (self.0 & Self::INDEX_MASK) as u32
    }

    #[inline]
    pub fn generation(self) -> u32 {
        (self.0 >> Self::GENERATION_SHIFT) as u32
    }
}

pub struct Tiles {
    atlases: Vec<Atlas>, // index == atlas id
    key_pool: Pool,
    bindings: Vec<KeyBinding>,
}

use std::fmt::{Display, Formatter};

#[derive(Debug)]
pub enum TilesError {
    AtlasOutOfTiles { atlas_id: u8 },
    KeyPoolFull,
    InvalidAtlasId { atlas_id: u8 },
    InvalidKey { key: TileKey },
    KeyGenMisMatch { key: TileKey },
    TileGenMisMatch { key: TileKey },
}

impl Display for TilesError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AtlasOutOfTiles { atlas_id } => write!(f, "atlas {atlas_id} out of tiles"),
            Self::KeyPoolFull => f.write_str("key pool is full"),
            Self::InvalidAtlasId { atlas_id } => write!(f, "invalid atlas id {atlas_id}"),
            Self::InvalidKey { key } => write!(f, "invalid key {key:?}"),
            Self::KeyGenMisMatch { key } => write!(f, "key generation mismatch for {key:?}"),
            Self::TileGenMisMatch { key } => write!(f, "tile generation mismatch for {key:?}"),
        }
    }
}

impl Tiles {
    pub fn new() -> Self {
        Self {
            atlases: Vec::new(),
            key_pool: Pool::new(u32::MAX),
            bindings: Vec::new(),
        }
    }

    pub fn new_atlas(&mut self, layout: AtlasLayout, format: GlaFormat) -> u8 {
        let atlas_id = self.atlases.len() as u8;
        self.atlases.push(Atlas::new(atlas_id, layout, format));
        atlas_id
    }

    pub fn position(&self, key: TileKey) -> Result<TilePos, TilesError> {
        self.ensure_valid(key)?;
        let position = self.bindings[key.index() as usize].position();
        Ok(position)
    }

    fn bind_key(&mut self, key: TileKey, binding: KeyBinding) {
        let index = key.index() as usize;
        if self.bindings.len() <= index {
            self.bindings
                .resize(index + 1, KeyBinding::empty(binding.position().atlas_id()));
        }
        self.bindings[index] = binding;
    }

    fn alloc_empty_from(&mut self, atlas_id: u8) -> Result<TileKey, TilesError> {
        self.atlases
            .get(atlas_id as usize)
            .ok_or(TilesError::InvalidAtlasId { atlas_id })?;

        let (index, generation) = self.key_pool.alloc()?;
        let key = TileKey::new(index, generation);
        self.bind_key(key, KeyBinding::empty(atlas_id));
        Ok(key)
    }

    fn materialize_empty(&mut self, key: TileKey) -> Result<TilePos, TilesError> {
        self.ensure_valid(key)?;
        let position = self.bindings[key.index() as usize].position();
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
        self.bind_key(key, binding);
        Ok(binding.position())
    }

    pub fn alloc_from(&mut self, atlas_id: u8) -> Result<TileKey, TilesError> {
        let atlas = self
            .atlases
            .get_mut(atlas_id as usize)
            .ok_or(TilesError::InvalidAtlasId { atlas_id })?;
        if atlas.remaining() == 0 {
            return Err(TilesError::AtlasOutOfTiles { atlas_id });
        }
        let (index, generation) = self.key_pool.alloc()?;
        let binding = atlas.alloc()?;
        let key = TileKey::new(index, generation);
        self.bind_key(key, binding);
        Ok(key)
    }

    pub fn alloc_batch_from(
        &mut self,
        atlas_id: u8,
        count: u32,
    ) -> Result<Vec<TileKey>, TilesError> {
        self.atlases
            .get(atlas_id as usize)
            .ok_or(TilesError::InvalidAtlasId { atlas_id })?;
        if self.key_pool.remaining() < count {
            return Err(TilesError::KeyPoolFull);
        }

        let mut keys = Vec::with_capacity(count as usize);
        for _ in 0..count {
            keys.push(self.alloc_empty_from(atlas_id)?);
        }
        Ok(keys)
    }

    pub fn ensure_valid(&self, key: TileKey) -> Result<(), TilesError> {
        self.key_pool
            .check(key.index(), key.generation())
            .then_some(())
            .ok_or(TilesError::KeyGenMisMatch { key })?;
        let binding = self
            .bindings
            .get(key.index() as usize)
            .ok_or(TilesError::InvalidKey { key })?;
        let atlas = self
            .atlases
            .get(binding.position().atlas_id() as usize)
            .ok_or(TilesError::InvalidAtlasId {
                atlas_id: binding.position().atlas_id(),
            })?;
        (binding.is_empty() || atlas.check(*binding))
            .then_some(())
            .ok_or(TilesError::TileGenMisMatch { key })?;
        Ok(())
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

pub struct TilesSession<'a> {
    tiles: &'a mut Tiles,
    pub allocated: Vec<TileKey>,
    pub discarded: Vec<TileKey>,
}

pub struct TilesSessionRecord {
    pub allocated: Box<[TileKey]>,
    pub discarded: Box<[TileKey]>,
}

impl<'a> TilesSession<'a> {
    pub fn new(tiles: &'a mut Tiles) -> Self {
        Self {
            tiles,
            allocated: Vec::new(),
            discarded: Vec::new(),
        }
    }

    pub fn alloc_from(&mut self, atlas_id: u8) -> Result<TileKey, TilesError> {
        let key = self.tiles.alloc_from(atlas_id)?;
        self.allocated.push(key);
        Ok(key)
    }

    pub fn alloc_batch_from(
        &mut self,
        atlas_id: u8,
        count: u32,
    ) -> Result<Vec<TileKey>, TilesError> {
        let keys = self.tiles.alloc_batch_from(atlas_id, count)?;
        self.allocated.extend(keys.clone());
        Ok(keys)
    }

    pub fn acquire_for_read(&self, key: TileKey) -> Result<TilePos, TilesError> {
        self.tiles.position(key)
    }

    pub fn acquire_for_write(&mut self, key: TileKey) -> Result<TilePos, TilesError> {
        let position = self.tiles.position(key)?;
        if !position.is_empty() {
            return Ok(position);
        }

        let position = self.tiles.materialize_empty(key)?;
        Ok(position)
    }

    pub fn discard(&mut self, key: TileKey) {
        self.discarded.push(key);
    }

    pub fn discard_batch(&mut self, keys: Vec<TileKey>) {
        self.discarded.extend(keys);
    }

    pub fn record(&self) -> TilesSessionRecord {
        TilesSessionRecord {
            allocated: self.allocated.clone().into_boxed_slice(),
            discarded: self.discarded.clone().into_boxed_slice(),
        }
    }
}
