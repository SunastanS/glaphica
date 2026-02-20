use std::fmt;

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

    pub fn keys(&self) -> &[TileKey] {
        &self.keys
    }

    pub fn into_keys(self) -> Vec<TileKey> {
        self.keys
    }

    pub(crate) fn new(id: TileSetId, owner_tag: u64, keys: Vec<TileKey>) -> Self {
        Self {
            id,
            owner_tag,
            keys,
        }
    }

    pub(crate) fn owner_tag(&self) -> u64 {
        self.owner_tag
    }
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
                write!(formatter, "tile set handle does not belong to this atlas store")
            }
            TileSetError::RollbackReleaseFailed => {
                write!(formatter, "tile set rollback failed to release reserved tile key")
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
                write!(formatter, "tile atlas tiles_per_row/tiles_per_column must be at least 1")
            }
            TileAtlasCreateError::AtlasTileGridTooLarge => {
                write!(formatter, "tile atlas tile grid exceeds supported tile index range")
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

    pub fn set_tile(&mut self, tile_x: u32, tile_y: u32, key: K) -> Result<(), VirtualImageError> {
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

#[cfg(test)]
pub(crate) use atlas::{rgba8_tile_len, tile_origin};

mod atlas;
mod merge_callback;
mod merge_submission;

pub use atlas::{
    GenericR32FloatTileAtlasGpuArray, GenericR32FloatTileAtlasStore,
    GenericR8UintTileAtlasGpuArray, GenericR8UintTileAtlasStore, GenericTileAtlasConfig,
    GenericTileAtlasGpuArray, GenericTileAtlasStore, GroupTileAtlasGpuArray, GroupTileAtlasStore,
    RuntimeGenericTileAtlasConfig, RuntimeGenericTileAtlasGpuArray, RuntimeGenericTileAtlasStore,
    TileAtlasConfig, TileAtlasGpuArray, TileAtlasStore, TilePayloadKind,
};
pub use merge_callback::{
    TileMergeAckFailure, TileMergeBatchAck, TileMergeCompletionCallback,
    TileMergeCompletionNotice, TileMergeCompletionNoticeId, TileMergeTerminalUpdate,
};
pub use merge_submission::{
    AckOutcome, MergePlanRequest, MergePlanTileOp, MergeSubmission, MergeTileStore,
    ReceiptState, RendererSubmitPayload, TileKeyMapping, TileMergeEngine, TileMergeError,
    TilesBusinessResult,
};

#[cfg(test)]
mod tests;
