use atlas::{Atlas, AtlasError, AtlasFormat, AtlasLayout, KeyBinding, Position};
use gla_core::{Pool, PoolError};
use tile_commands::TileOpRecorder;

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

    pub fn new_atlas(&mut self, layout: AtlasLayout, format: AtlasFormat) -> u8 {
        let atlas_id = self.atlases.len() as u8;
        self.atlases.push(Atlas::new(atlas_id, layout, format));
        atlas_id
    }

    pub fn position(&self, key: TileKey) -> Result<Position, TilesError> {
        self.ensure_valid(key)?;
        let position = self.bindings[key.index() as usize].position();
        Ok(position)
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
        self.bindings[key.index() as usize] = binding;
        Ok(key)
    }

    pub fn alloc_batch_from(
        &mut self,
        atlas_id: u8,
        count: u32,
    ) -> Result<Vec<TileKey>, TilesError> {
        let atlas = &mut self.atlases[atlas_id as usize];
        if atlas.remaining() < count {
            return Err(TilesError::AtlasOutOfTiles { atlas_id });
        }
        let mut keys = Vec::new();
        for _ in 0..count {
            keys.push(self.alloc_from(atlas_id)?); // n redandent check for remaining tiles, but fine
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

    /// WARNING: BLACK MAGIC
    /// see [`Tiles::copy_on_write`]
    ///
    /// This is a dangerous operation for modifying bindings directly without protection of generations.
    /// It has to be because on the logic tile side, editing a tile is Copy on Write,
    /// but at resource level, we backup historical tiles in different atlases.
    /// So once editing a tile, we need to
    /// - alloc a new tile in backup atlas
    /// - copy the original tile to the new tile
    /// - binding a new key to original tile
    /// - binding the old key to the new tile at backup atlas
    ///
    /// During this process, the generation of keys do not increase
    /// because their coordinate data did not change, only the position changed,
    /// so if you try to render the original key, you can still get it's coordinate data (but in backup atlas).
    ///
    /// Theoretically, the generation of both positions should increase,
    /// but in practice, preservation is fine if we keep the operating order
    pub fn swap_binding(&mut self, lhs: TileKey, rhs: TileKey) -> Result<(), TilesError> {
        self.ensure_valid(lhs)?;
        self.ensure_valid(rhs)?;
        let lhs_binding = self.bindings[lhs.index() as usize];
        let rhs_binding = self.bindings[rhs.index() as usize];
        self.bindings[lhs.index() as usize] = rhs_binding;
        self.bindings[rhs.index() as usize] = lhs_binding;
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

    /// this should be considered as getting a mut ref of a tile
    /// after called, you can use TileOpRecorder to modify the content of a tile
    pub fn copy_on_write(
        &mut self,
        key: TileKey,
        backup_atlas_id: u8,
        recorder: &mut TileOpRecorder,
    ) -> Result<TileKey, TilesError> {
        self.tiles.ensure_valid(key)?;
        if self.allocated.contains(&key) {
            Ok(key)
        } else {
            let new_key = self.tiles.alloc_from(backup_atlas_id)?;
            self.allocated.push(new_key);
            recorder.copy_tile(self.tiles.position(key)?, self.tiles.position(new_key)?);
            self.tiles.swap_binding(key, new_key)?;
            Ok(new_key)
        }
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
