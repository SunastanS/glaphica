use std::collections::{HashMap, HashSet};
use std::fmt;

use bitvec::prelude::{BitVec, Lsb0};
use render_protocol::BufferTileCoordinate;

pub const TILE_SIZE: u32 = 128;
pub const TILE_GUTTER: u32 = 1;
pub const TILE_STRIDE: u32 = TILE_SIZE + TILE_GUTTER * 2;
pub const DEFAULT_MAX_LAYERS: u32 = 4;
pub const TILES_PER_ROW: u32 = 32;
pub const ATLAS_SIZE: u32 = TILES_PER_ROW * TILE_STRIDE;
pub const TILES_PER_ATLAS: u32 = TILES_PER_ROW * TILES_PER_ROW;
pub const ATLAS_OCCUPANCY_WORDS: usize = (TILES_PER_ATLAS as usize).div_ceil(64);

const INDEX_SHARDS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileKey(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileSetId(u64);

#[derive(Debug)]
pub struct TileSetHandle {
    id: TileSetId,
    owner_tag: u64,
    keys: Vec<TileKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileAddress {
    pub atlas_layer: u32,
    pub tile_index: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileAtlasLayout {
    pub tiles_per_row: u32,
    pub tiles_per_column: u32,
    pub atlas_width: u32,
    pub atlas_height: u32,
}

impl TileAddress {
    fn tile_coords_in(self, layout: TileAtlasLayout) -> (u32, u32) {
        assert!(layout.tiles_per_row > 0, "tiles_per_row must be at least 1");
        assert!(
            layout.tiles_per_column > 0,
            "tiles_per_column must be at least 1"
        );
        let tile_index = self.tile_index as u32;
        let tile_x = tile_index % layout.tiles_per_row;
        let tile_y = tile_index / layout.tiles_per_row;
        assert!(
            tile_y < layout.tiles_per_column,
            "tile_index {} is out of bounds for atlas tile grid {}x{}",
            self.tile_index,
            layout.tiles_per_row,
            layout.tiles_per_column
        );
        (tile_x, tile_y)
    }

    pub fn tile_x(self) -> u32 {
        self.tile_x_in(TileAtlasLayout {
            tiles_per_row: TILES_PER_ROW,
            tiles_per_column: TILES_PER_ROW,
            atlas_width: ATLAS_SIZE,
            atlas_height: ATLAS_SIZE,
        })
    }

    pub fn tile_y(self) -> u32 {
        self.tile_y_in(TileAtlasLayout {
            tiles_per_row: TILES_PER_ROW,
            tiles_per_column: TILES_PER_ROW,
            atlas_width: ATLAS_SIZE,
            atlas_height: ATLAS_SIZE,
        })
    }

    pub fn tile_x_in(self, layout: TileAtlasLayout) -> u32 {
        let (tile_x, _) = self.tile_coords_in(layout);
        tile_x
    }

    pub fn tile_y_in(self, layout: TileAtlasLayout) -> u32 {
        let (_, tile_y) = self.tile_coords_in(layout);
        tile_y
    }

    pub fn atlas_uv_origin_in(self, layout: TileAtlasLayout) -> (f32, f32) {
        assert!(layout.atlas_width > 0, "atlas_width must be at least 1");
        assert!(layout.atlas_height > 0, "atlas_height must be at least 1");
        let (origin_x, origin_y) = self.atlas_content_origin_pixels_in(layout);
        (
            origin_x as f32 / layout.atlas_width as f32,
            origin_y as f32 / layout.atlas_height as f32,
        )
    }

    pub fn atlas_slot_origin_pixels_in(self, layout: TileAtlasLayout) -> (u32, u32) {
        let (tile_x, tile_y) = self.tile_coords_in(layout);
        (
            tile_x
                .checked_mul(TILE_STRIDE)
                .expect("atlas slot origin x overflow"),
            tile_y
                .checked_mul(TILE_STRIDE)
                .expect("atlas slot origin y overflow"),
        )
    }

    pub fn atlas_content_origin_pixels_in(self, layout: TileAtlasLayout) -> (u32, u32) {
        let (slot_x, slot_y) = self.atlas_slot_origin_pixels_in(layout);
        (
            slot_x
                .checked_add(TILE_GUTTER)
                .expect("atlas content origin x overflow"),
            slot_y
                .checked_add(TILE_GUTTER)
                .expect("atlas content origin y overflow"),
        )
    }
}

impl TileSetHandle {
    pub fn id(&self) -> TileSetId {
        self.id
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn iter_keys(&self) -> impl Iterator<Item = TileKey> + '_ {
        self.keys.iter().copied()
    }

    pub(crate) fn new(id: TileSetId, owner_tag: u64, keys: Vec<TileKey>) -> Self {
        Self {
            id,
            owner_tag,
            keys,
        }
    }

    pub(crate) fn keys(&self) -> &[TileKey] {
        &self.keys
    }

    pub(crate) fn into_keys(self) -> Vec<TileKey> {
        self.keys
    }

    pub(crate) fn owner_tag(&self) -> u64 {
        self.owner_tag
    }
}

#[cfg(feature = "test-helpers")]
pub fn test_tile_key(raw: u64) -> TileKey {
    TileKey(raw)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileAllocError {
    KeySpaceExhausted,
    AtlasFull,
    QueueDisconnected,
}

impl fmt::Display for TileAllocError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TileAllocError::KeySpaceExhausted => write!(formatter, "tile key space exhausted"),
            TileAllocError::AtlasFull => write!(formatter, "tile atlas has no free slots"),
            TileAllocError::QueueDisconnected => {
                write!(formatter, "tile operation queue disconnected")
            }
        }
    }
}

impl std::error::Error for TileAllocError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileSetError {
    Alloc(TileAllocError),
    KeySpaceExhausted,
    UnknownTileKey,
    DuplicateTileKey,
    SetNotOwnedByStore,
    RollbackReleaseFailed,
}

impl From<TileAllocError> for TileSetError {
    fn from(value: TileAllocError) -> Self {
        Self::Alloc(value)
    }
}

impl fmt::Display for TileSetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TileSetError::Alloc(error) => write!(formatter, "tile set allocation failed: {error}"),
            TileSetError::KeySpaceExhausted => write!(formatter, "tile set id space exhausted"),
            TileSetError::UnknownTileKey => write!(formatter, "tile set contains unknown tile key"),
            TileSetError::DuplicateTileKey => {
                write!(formatter, "tile set cannot contain duplicate tile keys")
            }
            TileSetError::SetNotOwnedByStore => {
                write!(
                    formatter,
                    "tile set handle does not belong to this atlas store"
                )
            }
            TileSetError::RollbackReleaseFailed => {
                write!(
                    formatter,
                    "tile set rollback failed to release reserved tile key"
                )
            }
        }
    }
}

impl std::error::Error for TileSetError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileIngestError {
    SizeMismatch,
    MissingCopyDstUsage,
    SizeOverflow,
    StrideTooSmall,
    BufferLengthMismatch,
    BufferTooShort,
    Alloc(TileAllocError),
}

impl From<TileAllocError> for TileIngestError {
    fn from(value: TileAllocError) -> Self {
        Self::Alloc(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageIngestError {
    SizeOverflow,
    StrideTooSmall,
    BufferTooShort,
    RectOutOfBounds,
    NonTileAligned,
    Virtual(VirtualImageError),
    Tile(TileIngestError),
}

impl From<VirtualImageError> for ImageIngestError {
    fn from(value: VirtualImageError) -> Self {
        Self::Virtual(value)
    }
}

impl From<TileIngestError> for ImageIngestError {
    fn from(value: TileIngestError) -> Self {
        Self::Tile(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileAtlasCreateError {
    MissingCopyDstUsage,
    MissingTextureBindingUsage,
    MissingStorageBindingUsage,
    MaxLayersZero,
    AtlasTileGridZero,
    AtlasTileGridNotSquare,
    AtlasTileGridTooLarge,
    MaxLayersExceedsDeviceLimit,
    AtlasSizeExceedsDeviceLimit,
    UnsupportedPayloadFormat,
    UnsupportedFormatUsage,
    StorageBindingUnsupportedForFormat,
}

impl fmt::Display for TileAtlasCreateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TileAtlasCreateError::MissingCopyDstUsage => {
                write!(formatter, "tile atlas usage must include COPY_DST")
            }
            TileAtlasCreateError::MissingTextureBindingUsage => {
                write!(formatter, "tile atlas usage must include TEXTURE_BINDING")
            }
            TileAtlasCreateError::MissingStorageBindingUsage => {
                write!(formatter, "tile atlas usage must include STORAGE_BINDING")
            }
            TileAtlasCreateError::MaxLayersZero => {
                write!(formatter, "tile atlas max_layers must be at least 1")
            }
            TileAtlasCreateError::AtlasTileGridZero => {
                write!(
                    formatter,
                    "tile atlas tiles_per_row/tiles_per_column must be at least 1"
                )
            }
            TileAtlasCreateError::AtlasTileGridNotSquare => {
                write!(
                    formatter,
                    "tile atlas tiles_per_row must match tiles_per_column"
                )
            }
            TileAtlasCreateError::AtlasTileGridTooLarge => {
                write!(
                    formatter,
                    "tile atlas tile grid exceeds supported tile index range"
                )
            }
            TileAtlasCreateError::MaxLayersExceedsDeviceLimit => {
                write!(formatter, "tile atlas max_layers exceeds device limit")
            }
            TileAtlasCreateError::AtlasSizeExceedsDeviceLimit => {
                write!(formatter, "tile atlas size exceeds device limit")
            }
            TileAtlasCreateError::UnsupportedPayloadFormat => {
                write!(
                    formatter,
                    "tile atlas payload kind is incompatible with texture format"
                )
            }
            TileAtlasCreateError::UnsupportedFormatUsage => {
                write!(
                    formatter,
                    "tile atlas texture format does not support requested usage"
                )
            }
            TileAtlasCreateError::StorageBindingUnsupportedForFormat => {
                write!(
                    formatter,
                    "tile atlas texture format does not support STORAGE_BINDING"
                )
            }
        }
    }
}

impl std::error::Error for TileAtlasCreateError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileGpuDrainError {
    MissingCopyDstUsage,
    UploadLengthMismatch,
}

impl fmt::Display for TileGpuDrainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TileGpuDrainError::MissingCopyDstUsage => {
                write!(formatter, "tile atlas usage must include COPY_DST")
            }
            TileGpuDrainError::UploadLengthMismatch => {
                write!(formatter, "tile upload bytes length mismatch")
            }
        }
    }
}

impl std::error::Error for TileGpuDrainError {}

#[derive(Debug, Clone, Default)]
pub struct VirtualImage<K> {
    size_x: u32,
    size_y: u32,
    tiles_per_row: u32,
    tiles_per_column: u32,
    tiles: Box<[Option<K>]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtualImageError {
    TileCountOverflow,
    TileBytesLengthMismatch,
    TileIndexOutOfBounds,
    OutputByteCountOverflow,
    VersionOverflow,
}

impl<K> VirtualImage<K> {
    fn tile_index(&self, tile_x: u32, tile_y: u32) -> Result<usize, VirtualImageError> {
        if tile_x >= self.tiles_per_row || tile_y >= self.tiles_per_column {
            return Err(VirtualImageError::TileIndexOutOfBounds);
        }

        let row = (tile_y as usize)
            .checked_mul(self.tiles_per_row as usize)
            .ok_or(VirtualImageError::TileIndexOutOfBounds)?;
        row.checked_add(tile_x as usize)
            .ok_or(VirtualImageError::TileIndexOutOfBounds)
    }

    pub fn new(size_x: u32, size_y: u32) -> Result<Self, VirtualImageError> {
        let tiles_per_row = size_x.div_ceil(TILE_SIZE);
        let tiles_per_column = size_y.div_ceil(TILE_SIZE);
        let tile_count = (tiles_per_row as usize)
            .checked_mul(tiles_per_column as usize)
            .ok_or(VirtualImageError::TileCountOverflow)?;

        Ok(Self {
            size_x,
            size_y,
            tiles_per_row,
            tiles_per_column,
            tiles: {
                let mut tiles = Vec::new();
                tiles.resize_with(tile_count, || None);
                tiles.into_boxed_slice()
            },
        })
    }

    pub fn size_x(&self) -> u32 {
        self.size_x
    }

    pub fn size_y(&self) -> u32 {
        self.size_y
    }

    pub fn tiles_per_row(&self) -> u32 {
        self.tiles_per_row
    }

    pub fn tiles_per_column(&self) -> u32 {
        self.tiles_per_column
    }

    pub fn get_tile(&self, tile_x: u32, tile_y: u32) -> Result<Option<&K>, VirtualImageError> {
        let index = self.tile_index(tile_x, tile_y)?;
        Ok(self.tiles.get(index).and_then(|value| value.as_ref()))
    }

    pub(crate) fn set_tile(
        &mut self,
        tile_x: u32,
        tile_y: u32,
        key: K,
    ) -> Result<(), VirtualImageError> {
        let index = self.tile_index(tile_x, tile_y)?;
        let slot = self
            .tiles
            .get_mut(index)
            .ok_or(VirtualImageError::TileIndexOutOfBounds)?;
        *slot = Some(key);
        Ok(())
    }

    pub fn iter_tiles(&self) -> impl Iterator<Item = (u32, u32, &K)> + '_ {
        self.tiles
            .iter()
            .enumerate()
            .filter_map(move |(index, slot)| {
                let key = slot.as_ref()?;
                let tiles_per_row = self.tiles_per_row as usize;
                let tile_x = (index % tiles_per_row) as u32;
                let tile_y = (index / tiles_per_row) as u32;
                Some((tile_x, tile_y, key))
            })
    }

    pub fn export_rgba8(
        &self,
        mut load_tile: impl FnMut(&K) -> Option<Vec<u8>>,
    ) -> Result<Vec<u8>, VirtualImageError> {
        let size_x_usize: usize = self
            .size_x
            .try_into()
            .map_err(|_| VirtualImageError::OutputByteCountOverflow)?;
        let size_y_usize: usize = self
            .size_y
            .try_into()
            .map_err(|_| VirtualImageError::OutputByteCountOverflow)?;
        let out_len = size_x_usize
            .checked_mul(size_y_usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(VirtualImageError::OutputByteCountOverflow)?;
        let mut out = vec![0u8; out_len];
        let tile_row_bytes = (TILE_SIZE as usize) * 4;
        let expected_tile_len = (TILE_SIZE as usize) * tile_row_bytes;

        for tile_y in 0..self.tiles_per_column {
            for tile_x in 0..self.tiles_per_row {
                let index = (tile_y as usize) * (self.tiles_per_row as usize) + (tile_x as usize);
                let Some(key) = self.tiles.get(index).and_then(|value| value.as_ref()) else {
                    continue;
                };
                let Some(tile) = load_tile(key) else {
                    continue;
                };
                if tile.len() != expected_tile_len {
                    return Err(VirtualImageError::TileBytesLengthMismatch);
                }

                let dst_x0 = (tile_x * TILE_SIZE) as usize;
                let dst_y0 = (tile_y * TILE_SIZE) as usize;
                for row in 0..(TILE_SIZE as usize) {
                    let dst_y = dst_y0 + row;
                    if dst_y >= self.size_y as usize {
                        break;
                    }
                    let copy_pixels =
                        (TILE_SIZE as usize).min((self.size_x as usize).saturating_sub(dst_x0));
                    let copy_bytes = copy_pixels * 4;
                    let src = &tile[(row * tile_row_bytes)..(row * tile_row_bytes + copy_bytes)];
                    let dst_offset = (dst_y * (self.size_x as usize) + dst_x0) * 4;
                    out[dst_offset..(dst_offset + copy_bytes)].copy_from_slice(src);
                }
            }
        }

        Ok(out)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileDirtyBitset {
    tiles_per_row: u32,
    tiles_per_column: u32,
    bits: BitVec<usize, Lsb0>,
    dirty_count: usize,
}

impl TileDirtyBitset {
    pub fn new(tiles_per_row: u32, tiles_per_column: u32) -> Result<Self, VirtualImageError> {
        let tile_count = (tiles_per_row as usize)
            .checked_mul(tiles_per_column as usize)
            .ok_or(VirtualImageError::TileCountOverflow)?;
        Ok(Self {
            tiles_per_row,
            tiles_per_column,
            bits: BitVec::repeat(false, tile_count),
            dirty_count: 0,
        })
    }

    pub fn tiles_per_row(&self) -> u32 {
        self.tiles_per_row
    }

    pub fn tiles_per_column(&self) -> u32 {
        self.tiles_per_column
    }

    pub fn is_empty(&self) -> bool {
        self.dirty_count == 0
    }

    pub fn is_full(&self) -> bool {
        self.dirty_count == self.bits.len() && !self.bits.is_empty()
    }

    pub fn set(&mut self, tile_x: u32, tile_y: u32) -> Result<(), VirtualImageError> {
        let index = self.tile_index(tile_x, tile_y)?;
        let Some(mut slot) = self.bits.get_mut(index) else {
            return Err(VirtualImageError::TileIndexOutOfBounds);
        };
        if !*slot {
            *slot = true;
            self.dirty_count = self
                .dirty_count
                .checked_add(1)
                .ok_or(VirtualImageError::TileCountOverflow)?;
        }
        Ok(())
    }

    pub fn merge_from(&mut self, other: &TileDirtyBitset) -> Result<(), VirtualImageError> {
        if self.tiles_per_row != other.tiles_per_row
            || self.tiles_per_column != other.tiles_per_column
            || self.bits.len() != other.bits.len()
        {
            return Err(VirtualImageError::TileIndexOutOfBounds);
        }
        for (index, other_bit) in other.bits.iter().by_vals().enumerate() {
            if other_bit {
                let Some(mut slot) = self.bits.get_mut(index) else {
                    return Err(VirtualImageError::TileIndexOutOfBounds);
                };
                if !*slot {
                    *slot = true;
                    self.dirty_count = self
                        .dirty_count
                        .checked_add(1)
                        .ok_or(VirtualImageError::TileCountOverflow)?;
                }
            }
        }
        Ok(())
    }

    pub fn iter_dirty_tiles(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        let tiles_per_row = self.tiles_per_row as usize;
        self.bits
            .iter()
            .by_vals()
            .enumerate()
            .filter_map(move |(index, is_dirty)| {
                if is_dirty {
                    let tile_x = (index % tiles_per_row) as u32;
                    let tile_y = (index / tiles_per_row) as u32;
                    Some((tile_x, tile_y))
                } else {
                    None
                }
            })
    }

    fn tile_index(&self, tile_x: u32, tile_y: u32) -> Result<usize, VirtualImageError> {
        if tile_x >= self.tiles_per_row || tile_y >= self.tiles_per_column {
            return Err(VirtualImageError::TileIndexOutOfBounds);
        }
        let row = (tile_y as usize)
            .checked_mul(self.tiles_per_row as usize)
            .ok_or(VirtualImageError::TileIndexOutOfBounds)?;
        row.checked_add(tile_x as usize)
            .ok_or(VirtualImageError::TileIndexOutOfBounds)
    }
}

#[derive(Debug, Clone)]
pub struct TileImage {
    image: VirtualImage<TileKey>,
    versions: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileDirtyQuery {
    pub latest_version: u64,
    pub dirty_tiles: TileDirtyBitset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirtySinceResult {
    UpToDate,
    HistoryTruncated,
    HasChanges(TileDirtyQuery),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TileImageApplyError {
    TileOutOfBounds {
        tile_x: u32,
        tile_y: u32,
    },
    DuplicateTileCoordinate {
        tile_x: u32,
        tile_y: u32,
    },
    PreviousKeyMismatch {
        tile_x: u32,
        tile_y: u32,
        expected: Option<TileKey>,
        actual: Option<TileKey>,
    },
}

impl fmt::Display for TileImageApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TileImageApplyError::TileOutOfBounds { tile_x, tile_y } => {
                write!(
                    formatter,
                    "tile coordinate out of bounds at ({tile_x}, {tile_y})"
                )
            }
            TileImageApplyError::DuplicateTileCoordinate { tile_x, tile_y } => {
                write!(
                    formatter,
                    "duplicate tile coordinate in apply batch at ({tile_x}, {tile_y})"
                )
            }
            TileImageApplyError::PreviousKeyMismatch {
                tile_x,
                tile_y,
                expected,
                actual,
            } => {
                write!(
                    formatter,
                    "tile key mismatch at ({tile_x}, {tile_y}): expected {:?}, got {:?}",
                    expected, actual
                )
            }
        }
    }
}

impl std::error::Error for TileImageApplyError {}

impl TileImage {
    pub fn new(size_x: u32, size_y: u32) -> Result<Self, VirtualImageError> {
        let tiles_per_row = size_x.div_ceil(TILE_SIZE);
        let tiles_per_column = size_y.div_ceil(TILE_SIZE);
        let tile_count = (tiles_per_row as usize)
            .checked_mul(tiles_per_column as usize)
            .ok_or(VirtualImageError::TileCountOverflow)?;
        Ok(Self {
            image: VirtualImage::new(size_x, size_y)?,
            versions: vec![0; tile_count],
        })
    }

    pub fn size_x(&self) -> u32 {
        self.image.size_x()
    }

    pub fn size_y(&self) -> u32 {
        self.image.size_y()
    }

    pub fn tiles_per_row(&self) -> u32 {
        self.image.tiles_per_row()
    }

    pub fn tiles_per_column(&self) -> u32 {
        self.image.tiles_per_column()
    }

    pub fn get_tile(&self, tile_x: u32, tile_y: u32) -> Result<Option<TileKey>, VirtualImageError> {
        Ok(self.image.get_tile(tile_x, tile_y)?.copied())
    }

    pub fn get_tile_version(&self, tile_x: u32, tile_y: u32) -> Result<u32, VirtualImageError> {
        let index = self.tile_index(tile_x, tile_y)?;
        Ok(*self
            .versions
            .get(index)
            .ok_or(VirtualImageError::TileIndexOutOfBounds)?)
    }

    pub fn set_tile(
        &mut self,
        tile_x: u32,
        tile_y: u32,
        key: TileKey,
    ) -> Result<(), VirtualImageError> {
        self.set_tile_recording(tile_x, tile_y, key)?;
        Ok(())
    }

    pub fn iter_tiles(&self) -> impl Iterator<Item = (u32, u32, TileKey)> + '_ {
        self.image
            .iter_tiles()
            .map(|(tile_x, tile_y, key)| (tile_x, tile_y, *key))
    }

    pub fn export_rgba8(
        &self,
        mut load_tile: impl FnMut(TileKey) -> Option<Vec<u8>>,
    ) -> Result<Vec<u8>, VirtualImageError> {
        self.image.export_rgba8(|key| load_tile(*key))
    }

    pub(crate) fn from_virtual(image: VirtualImage<TileKey>) -> Self {
        let tiles_per_row = image.tiles_per_row();
        let tiles_per_column = image.tiles_per_column();
        let tile_count = (tiles_per_row as usize)
            .checked_mul(tiles_per_column as usize)
            .unwrap_or_else(|| panic!("tile count overflow for tile image"));
        let mut tile_image = Self {
            image,
            versions: vec![0; tile_count],
        };
        for (tile_x, tile_y, _key) in tile_image.image.iter_tiles() {
            let index = tile_image
                .tile_index(tile_x, tile_y)
                .unwrap_or_else(|error| {
                    panic!("tile index resolution failed for tile image: {error:?}")
                });
            if let Some(slot) = tile_image.versions.get_mut(index) {
                *slot = 1;
            } else {
                panic!("tile version slot missing for ({tile_x}, {tile_y})");
            }
        }
        tile_image
    }

    fn set_tile_recording(
        &mut self,
        tile_x: u32,
        tile_y: u32,
        key: TileKey,
    ) -> Result<(), VirtualImageError> {
        self.image.set_tile(tile_x, tile_y, key)?;
        let index = self.tile_index(tile_x, tile_y)?;
        let slot = self
            .versions
            .get_mut(index)
            .ok_or(VirtualImageError::TileIndexOutOfBounds)?;
        *slot = slot
            .checked_add(1)
            .unwrap_or_else(|| panic!("tile version overflow at ({tile_x}, {tile_y})"));
        Ok(())
    }

    fn tile_index(&self, tile_x: u32, tile_y: u32) -> Result<usize, VirtualImageError> {
        if tile_x >= self.tiles_per_row() || tile_y >= self.tiles_per_column() {
            return Err(VirtualImageError::TileIndexOutOfBounds);
        }
        let row = (tile_y as usize)
            .checked_mul(self.tiles_per_row() as usize)
            .ok_or(VirtualImageError::TileIndexOutOfBounds)?;
        row.checked_add(tile_x as usize)
            .ok_or(VirtualImageError::TileIndexOutOfBounds)
    }
}

#[cfg(feature = "atlas-gpu")]
pub fn apply_tile_key_mappings(
    image: &mut TileImage,
    mappings: &[TileKeyMapping],
) -> Result<(), TileImageApplyError> {
    let mut seen = HashSet::with_capacity(mappings.len());
    for mapping in mappings {
        let coordinate = (mapping.tile_x, mapping.tile_y);
        if !seen.insert(coordinate) {
            return Err(TileImageApplyError::DuplicateTileCoordinate {
                tile_x: mapping.tile_x,
                tile_y: mapping.tile_y,
            });
        }
        let current = image
            .get_tile(mapping.tile_x, mapping.tile_y)
            .map_err(|_| TileImageApplyError::TileOutOfBounds {
                tile_x: mapping.tile_x,
                tile_y: mapping.tile_y,
            })?;
        if current != mapping.previous_key {
            return Err(TileImageApplyError::PreviousKeyMismatch {
                tile_x: mapping.tile_x,
                tile_y: mapping.tile_y,
                expected: mapping.previous_key,
                actual: current,
            });
        }
    }
    for mapping in mappings {
        image
            .set_tile_recording(mapping.tile_x, mapping.tile_y, mapping.new_key)
            .map_err(|_| TileImageApplyError::TileOutOfBounds {
                tile_x: mapping.tile_x,
                tile_y: mapping.tile_y,
            })?;
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct BrushBufferTileRegistry {
    tiles_by_stroke: HashMap<u64, HashMap<BufferTileCoordinate, TileKey>>,
    retained_stroke_by_retain_id: HashMap<u64, u64>,
    retained_retain_id_by_stroke: HashMap<u64, u64>,
}

impl BrushBufferTileRegistry {
    #[cfg(feature = "atlas-gpu")]
    pub fn allocate_tiles(
        &mut self,
        stroke_session_id: u64,
        tiles: impl IntoIterator<Item = BufferTileCoordinate>,
        atlas_store: &TileAtlasStore,
    ) -> Result<(), TileAllocError> {
        if self
            .retained_retain_id_by_stroke
            .contains_key(&stroke_session_id)
        {
            panic!(
                "cannot allocate brush buffer tiles for retained stroke {}",
                stroke_session_id
            );
        }
        let stroke_tiles = self.tiles_by_stroke.entry(stroke_session_id).or_default();
        for tile_coordinate in tiles {
            if stroke_tiles.contains_key(&tile_coordinate) {
                continue;
            }
            let tile_key = atlas_store.allocate()?;
            stroke_tiles.insert(tile_coordinate, tile_key);
        }
        Ok(())
    }

    #[cfg(feature = "atlas-gpu")]
    pub fn release_tiles(
        &mut self,
        stroke_session_id: u64,
        tiles: impl IntoIterator<Item = BufferTileCoordinate>,
        atlas_store: &TileAtlasStore,
    ) {
        let stroke_tiles = self
            .tiles_by_stroke
            .get_mut(&stroke_session_id)
            .unwrap_or_else(|| panic!("release requested for unknown stroke {stroke_session_id}"));
        for tile_coordinate in tiles {
            let tile_key = stroke_tiles.remove(&tile_coordinate).unwrap_or_else(|| {
                panic!(
                    "release requested for missing tile mapping: stroke {} at ({}, {})",
                    stroke_session_id, tile_coordinate.tile_x, tile_coordinate.tile_y
                )
            });
            let released = atlas_store.release(tile_key);
            if !released {
                panic!(
                    "failed to release brush buffer tile for stroke {} at ({}, {})",
                    stroke_session_id, tile_coordinate.tile_x, tile_coordinate.tile_y
                );
            }
        }
        if stroke_tiles.is_empty() {
            self.tiles_by_stroke.remove(&stroke_session_id);
            if let Some(retain_id) = self.retained_retain_id_by_stroke.remove(&stroke_session_id) {
                self.retained_stroke_by_retain_id.remove(&retain_id);
            }
        }
    }

    #[cfg(feature = "atlas-gpu")]
    pub fn retain_stroke_tiles(
        &mut self,
        stroke_session_id: u64,
        atlas_store: &TileAtlasStore,
    ) -> u64 {
        let stroke_tiles = self
            .tiles_by_stroke
            .get(&stroke_session_id)
            .unwrap_or_else(|| panic!("retain requested for unknown stroke {}", stroke_session_id));
        if self
            .retained_retain_id_by_stroke
            .contains_key(&stroke_session_id)
        {
            panic!(
                "retain requested for stroke {} with existing retained batch",
                stroke_session_id
            );
        }
        if stroke_tiles.is_empty() {
            panic!(
                "retain requested for stroke {} without allocated tiles",
                stroke_session_id
            );
        }

        let keys = stroke_tiles.values().copied().collect::<Vec<_>>();
        let retain_id = atlas_store.retain_keys_new_batch(&keys);
        if self
            .retained_stroke_by_retain_id
            .insert(retain_id, stroke_session_id)
            .is_some()
        {
            panic!(
                "retain batch id {} duplicated while retaining stroke {}",
                retain_id, stroke_session_id
            );
        }
        let previous = self
            .retained_retain_id_by_stroke
            .insert(stroke_session_id, retain_id);
        if previous.is_some() {
            panic!(
                "retain id mapping duplicated for stroke {}",
                stroke_session_id
            );
        }
        retain_id
    }

    #[cfg(feature = "atlas-gpu")]
    pub fn release_stroke_on_merge_failed(
        &mut self,
        stroke_session_id: u64,
        atlas_store: &TileAtlasStore,
    ) {
        let stroke_tiles = self
            .tiles_by_stroke
            .remove(&stroke_session_id)
            .unwrap_or_else(|| {
                panic!(
                    "merge-failed release requested for unknown stroke {}",
                    stroke_session_id
                )
            });
        if let Some(retain_id) = self.retained_retain_id_by_stroke.remove(&stroke_session_id) {
            self.retained_stroke_by_retain_id.remove(&retain_id);
        }
        for (tile_coordinate, tile_key) in stroke_tiles {
            let released = atlas_store.release(tile_key);
            if !released {
                panic!(
                    "failed to release merge-failed brush tile for stroke {} at ({}, {})",
                    stroke_session_id, tile_coordinate.tile_x, tile_coordinate.tile_y
                );
            }
        }
    }

    pub fn apply_retained_eviction(
        &mut self,
        retain_id: u64,
        evicted_keys: &[TileKey],
    ) -> Option<u64> {
        let stroke_session_id = self.retained_stroke_by_retain_id.remove(&retain_id)?;
        let removed_retain_id = self
            .retained_retain_id_by_stroke
            .remove(&stroke_session_id)
            .unwrap_or_else(|| {
                panic!(
                    "missing reverse retained mapping for stroke {} and retain batch {}",
                    stroke_session_id, retain_id
                )
            });
        if removed_retain_id != retain_id {
            panic!(
                "retained mapping mismatch for stroke {}: expected retain {}, got {}",
                stroke_session_id, retain_id, removed_retain_id
            );
        }

        let evicted = evicted_keys.iter().copied().collect::<HashSet<_>>();
        let stroke_tiles = self
            .tiles_by_stroke
            .get_mut(&stroke_session_id)
            .unwrap_or_else(|| {
                panic!(
                    "missing stroke mapping while applying retained eviction: stroke {} retain {}",
                    stroke_session_id, retain_id
                )
            });
        stroke_tiles.retain(|_, tile_key| !evicted.contains(tile_key));
        if stroke_tiles.is_empty() {
            self.tiles_by_stroke.remove(&stroke_session_id);
        }
        Some(stroke_session_id)
    }

    pub fn visit_tiles(
        &self,
        stroke_session_id: u64,
        mut visit: impl FnMut(&BufferTileCoordinate, TileKey),
    ) {
        let stroke_tiles = self
            .tiles_by_stroke
            .get(&stroke_session_id)
            .unwrap_or_else(|| {
                panic!(
                    "merge requested for stroke {} without buffer tile mapping",
                    stroke_session_id
                )
            });
        for (tile_coordinate, tile_key) in stroke_tiles.iter() {
            visit(tile_coordinate, *tile_key);
        }
    }
}

#[cfg(test)]
pub(crate) use atlas::{rgba8_tile_len, tile_origin};

mod atlas;
mod merge_callback;
#[cfg(feature = "atlas-gpu")]
mod merge_submission;

#[cfg(feature = "atlas-gpu")]
pub use atlas::{
    GenericR8UintTileAtlasGpuArray, GenericR8UintTileAtlasStore, GenericR32FloatTileAtlasGpuArray,
    GenericR32FloatTileAtlasStore, GenericTileAtlasConfig, GenericTileAtlasGpuArray,
    GenericTileAtlasStore, GroupTileAtlasGpuArray, GroupTileAtlasStore,
    RuntimeGenericTileAtlasConfig, RuntimeGenericTileAtlasGpuArray, RuntimeGenericTileAtlasStore,
    TileAtlasConfig, TileAtlasFormat, TileAtlasGpuArray, TileAtlasStore, TileAtlasUsage,
    TilePayloadKind,
};
pub use merge_callback::{
    TileMergeAckFailure, TileMergeBatchAck, TileMergeCompletionCallback, TileMergeCompletionNotice,
    TileMergeCompletionNoticeId, TileMergeTerminalUpdate,
};
#[cfg(feature = "atlas-gpu")]
pub use merge_submission::{
    AckOutcome, MergeAuditRecord, MergeCompletionAuditRecord, MergePlanRequest, MergePlanTileOp,
    MergeSubmission, MergeTileStore, ReceiptState, RendererSubmitPayload, TileKeyMapping,
    TileMergeEngine, TileMergeError, TilesBusinessResult,
};

#[cfg(test)]
mod tests;
