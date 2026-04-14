use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasError, Backend, BackendId, CachedTileGroup, TileKey};
use gla_image::{GlaImage, GlaImageTileAccessError};
use glaphica_core::BlendMode;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrushPreviewTile {
    pub tile_index: usize,
    pub tile_key: TileKey,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrushPreview {
    backend_id: BackendId,
    opacity: f32,
    blend_mode: BlendMode,
    tile_indices: Vec<usize>,
    tile_keys: Vec<TileKey>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BrushPreviewError {
    InvalidOpacity(f32),
}

impl Display for BrushPreviewError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOpacity(opacity) => {
                write!(f, "brush preview opacity {opacity} must be finite and within [0, 1]")
            }
        }
    }
}

impl Error for BrushPreviewError {}

impl BrushPreview {
    pub fn new(
        backend_id: BackendId,
        opacity: f32,
        blend_mode: BlendMode,
    ) -> Result<Self, BrushPreviewError> {
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err(BrushPreviewError::InvalidOpacity(opacity));
        }

        Ok(Self {
            backend_id,
            opacity,
            blend_mode,
            tile_indices: Vec::new(),
            tile_keys: Vec::new(),
        })
    }

    pub fn backend_id(&self) -> BackendId {
        self.backend_id
    }

    pub fn opacity(&self) -> f32 {
        self.opacity
    }

    pub fn blend_mode(&self) -> BlendMode {
        self.blend_mode
    }

    pub fn tiles(&self) -> impl Iterator<Item = BrushPreviewTile> + '_ {
        self.tile_indices
            .iter()
            .copied()
            .zip(self.tile_keys.iter().copied())
            .map(|(tile_index, tile_key)| BrushPreviewTile {
                tile_index,
                tile_key,
            })
    }

    pub fn tile_key(&self, tile_index: usize) -> Option<TileKey> {
        let preview_index = self
            .tile_indices
            .iter()
            .position(|&preview_tile_index| preview_tile_index == tile_index)?;
        self.tile_keys.get(preview_index).copied()
    }

    pub fn set_tile(&mut self, tile_index: usize, tile_key: TileKey) {
        if let Some(preview_index) = self
            .tile_indices
            .iter()
            .position(|&preview_tile_index| preview_tile_index == tile_index)
        {
            self.tile_keys[preview_index] = tile_key;
            return;
        }

        self.tile_indices.push(tile_index);
        self.tile_keys.push(tile_key);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrokeTileState {
    pub tile_index: usize,
    pub active_tile_key: TileKey,
    pub backup_tile_key: TileKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrokeTileBackupCopy {
    pub source_tile_key: TileKey,
    pub cached_tile_key: TileKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrokeBackupResult {
    pub tile: StrokeTileState,
    pub snapshot_copy: Option<StrokeTileBackupCopy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrokeTileBackupError {
    Atlas(AtlasError),
    Image(GlaImageTileAccessError),
    WrongBackend {
        expected: BackendId,
        actual: BackendId,
    },
}

impl Display for StrokeTileBackupError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Atlas(error) => Display::fmt(error, f),
            Self::Image(error) => Display::fmt(error, f),
            Self::WrongBackend { expected, actual } => write!(
                f,
                "stroke backup session targets backend {}, but image uses backend {}",
                expected.raw(),
                actual.raw()
            ),
        }
    }
}

impl Error for StrokeTileBackupError {}

impl From<AtlasError> for StrokeTileBackupError {
    fn from(error: AtlasError) -> Self {
        Self::Atlas(error)
    }
}

impl From<GlaImageTileAccessError> for StrokeTileBackupError {
    fn from(error: GlaImageTileAccessError) -> Self {
        Self::Image(error)
    }
}

#[derive(Debug, Clone)]
pub struct StrokeTileBackupSession {
    backend: Backend,
    backend_id: BackendId,
    backup_group: CachedTileGroup,
    backed_up_tile_indices: Vec<usize>,
    backup_tile_keys: Vec<TileKey>,
}

impl StrokeTileBackupSession {
    pub fn new(backend: Backend) -> Result<Self, StrokeTileBackupError> {
        let backend_id = backend.backend_id()?;
        let backup_group = backend.create_cached_group()?;
        Ok(Self {
            backend,
            backend_id,
            backup_group,
            backed_up_tile_indices: Vec::new(),
            backup_tile_keys: Vec::new(),
        })
    }

    pub fn backend_id(&self) -> BackendId {
        self.backend_id
    }

    pub fn backup_group(&self) -> &CachedTileGroup {
        &self.backup_group
    }

    pub fn backup_tile_key(&self, tile_index: usize) -> Option<TileKey> {
        let backup_index = self
            .backed_up_tile_indices
            .iter()
            .position(|&backed_up_tile_index| backed_up_tile_index == tile_index)?;
        self.backup_tile_keys.get(backup_index).copied()
    }

    pub fn ensure_tile_backup(
        &mut self,
        image: &GlaImage,
        tile_index: usize,
    ) -> Result<StrokeBackupResult, StrokeTileBackupError> {
        if image.backend() != self.backend_id {
            return Err(StrokeTileBackupError::WrongBackend {
                expected: self.backend_id,
                actual: image.backend(),
            });
        }

        let active_tile_key = image
            .tile_key(tile_index)
            .ok_or(GlaImageTileAccessError::OutOfBounds)?;
        if active_tile_key == TileKey::EMPTY {
            return Ok(StrokeBackupResult {
                tile: StrokeTileState {
                    tile_index,
                    active_tile_key,
                    backup_tile_key: TileKey::EMPTY,
                },
                snapshot_copy: None,
            });
        }

        if let Some(backup_tile_key) = self.backup_tile_key(tile_index) {
            return Ok(StrokeBackupResult {
                tile: StrokeTileState {
                    tile_index,
                    active_tile_key,
                    backup_tile_key,
                },
                snapshot_copy: None,
            });
        }

        let backup_tile_key = self.backend.alloc_cached_in_group(&mut self.backup_group)?;
        self.backed_up_tile_indices.push(tile_index);
        self.backup_tile_keys.push(backup_tile_key);
        Ok(StrokeBackupResult {
            tile: StrokeTileState {
                tile_index,
                active_tile_key,
                backup_tile_key,
            },
            snapshot_copy: Some(StrokeTileBackupCopy {
                source_tile_key: active_tile_key,
                cached_tile_key: backup_tile_key,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use atlas::{AtlasLayout, Backend, BackendId, TileKey, TileState};
    use gla_image::{GlaImage, GlaImageLayout};
    use glaphica_core::BlendMode;

    use super::{
        BrushPreview, BrushPreviewError, StrokeTileBackupCopy, StrokeTileBackupError,
        StrokeTileBackupSession,
    };

    const IMAGE_TILE_SIZE: u32 = glaphica_core::IMAGE_TILE_SIZE;

    #[test]
    fn first_non_empty_tile_backup_requests_snapshot_copy() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(2));
        let mut image = GlaImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE),
            BackendId::new(2),
        )
        .expect("image should create");
        let active_tile = backend.alloc_active().expect("active tile should allocate");
        let active_tile_key = active_tile.tile_key();
        image
            .replace_tile_owner(1, active_tile)
            .expect("tile owner should install");

        let mut session =
            StrokeTileBackupSession::new(backend.clone()).expect("session should create");
        let prepared = session
            .ensure_tile_backup(&image, 1)
            .expect("backup should prepare");

        assert_eq!(prepared.tile.tile_index, 1);
        assert_eq!(prepared.tile.active_tile_key, active_tile_key);
        assert_ne!(prepared.tile.backup_tile_key, TileKey::EMPTY);
        assert_eq!(
            prepared.snapshot_copy,
            Some(StrokeTileBackupCopy {
                source_tile_key: active_tile_key,
                cached_tile_key: prepared.tile.backup_tile_key,
            })
        );
        assert_eq!(session.backup_group().keys(), &[prepared.tile.backup_tile_key]);
        assert_eq!(backend.tile_state(prepared.tile.backup_tile_key), Ok(TileState::Cached));
    }

    #[test]
    fn repeated_tile_backup_reuses_existing_cached_tile() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(2));
        let mut image = GlaImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE),
            BackendId::new(2),
        )
        .expect("image should create");
        let active_tile = backend.alloc_active().expect("active tile should allocate");
        image
            .replace_tile_owner(0, active_tile)
            .expect("tile owner should install");

        let mut session =
            StrokeTileBackupSession::new(backend).expect("session should create");
        let first = session
            .ensure_tile_backup(&image, 0)
            .expect("first backup should prepare");
        let second = session
            .ensure_tile_backup(&image, 0)
            .expect("second backup should prepare");

        assert_eq!(second.tile.backup_tile_key, first.tile.backup_tile_key);
        assert_eq!(second.snapshot_copy, None);
        assert_eq!(session.backup_group().keys(), &[first.tile.backup_tile_key]);
    }

    #[test]
    fn empty_tile_does_not_allocate_backup() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(2));
        let image = GlaImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE),
            BackendId::new(2),
        )
        .expect("image should create");

        let mut session =
            StrokeTileBackupSession::new(backend).expect("session should create");
        let prepared = session
            .ensure_tile_backup(&image, 0)
            .expect("empty tile should prepare");

        assert_eq!(prepared.tile.active_tile_key, TileKey::EMPTY);
        assert_eq!(prepared.tile.backup_tile_key, TileKey::EMPTY);
        assert_eq!(prepared.snapshot_copy, None);
        assert!(session.backup_group().keys().is_empty());
    }

    #[test]
    fn backup_session_rejects_image_from_other_backend() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(2));
        let image = GlaImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE),
            BackendId::new(3),
        )
        .expect("image should create");

        let mut session =
            StrokeTileBackupSession::new(backend).expect("session should create");
        assert_eq!(
            session.ensure_tile_backup(&image, 0),
            Err(StrokeTileBackupError::WrongBackend {
                expected: BackendId::new(2),
                actual: BackendId::new(3),
            })
        );
    }

    #[test]
    fn brush_preview_tracks_sparse_tiles_by_index() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(7));
        let cached = backend.alloc_cached(2).expect("cached tiles");
        let mut preview =
            BrushPreview::new(BackendId::new(7), 1.0, BlendMode::Normal).expect("preview");

        preview.set_tile(5, cached.keys()[0]);
        preview.set_tile(2, cached.keys()[1]);
        preview.set_tile(5, cached.keys()[1]);

        assert_eq!(preview.backend_id(), BackendId::new(7));
        assert_eq!(preview.tile_key(2), Some(cached.keys()[1]));
        assert_eq!(preview.tile_key(5), Some(cached.keys()[1]));
        assert_eq!(preview.tile_key(9), None);
        assert_eq!(
            preview.tiles().collect::<Vec<_>>(),
            vec![
                super::BrushPreviewTile {
                    tile_index: 5,
                    tile_key: cached.keys()[1],
                },
                super::BrushPreviewTile {
                    tile_index: 2,
                    tile_key: cached.keys()[1],
                },
            ]
        );
    }

    #[test]
    fn brush_preview_rejects_invalid_opacity() {
        assert_eq!(
            BrushPreview::new(BackendId::new(7), 1.5, BlendMode::Normal),
            Err(BrushPreviewError::InvalidOpacity(1.5))
        );
    }
}
