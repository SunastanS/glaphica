use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasError, Backend, BackendId, CachedTileGroup, TileCredential, TileKey, TileManager};
use gla_image::{GlaImage, GlaImageEnsureActiveTileError, GlaImageTileAccessError};
use glaphica_core::CopyTileCommand;
use renderer::RenderCommand;

type TileCopyCommand = CopyTileCommand<TileKey>;

#[derive(Debug, Clone)]
pub struct GlaImageUndo {
    image_backend: Backend,
    image_backend_id: BackendId,
    backup_backend: Backend,
    backup_backend_id: BackendId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlaImageUndoTileRecord {
    tile_index: usize,
    backup_tile_key: TileKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlaImageUndoBackup {
    backup_group: CachedTileGroup,
    tile_records: Vec<GlaImageUndoTileRecord>,
    copy_commands: Vec<TileCopyCommand>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BackupResult {
    pub commands: Vec<RenderCommand>,
    pub backup_group: CachedTileGroup,
    pub origin_keys: Vec<(usize, TileKey)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlaImageUndoRestore {
    backup_group: CachedTileGroup,
    tile_actions: Vec<GlaImageUndoTileAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlaImageUndoTileAction {
    RestoreFromBackup {
        tile_index: usize,
        copy_command: TileCopyCommand,
    },
    Clear {
        tile_index: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlaImageUndoError {
    Atlas(AtlasError),
    Image(GlaImageTileAccessError),
    ImageEnsureActive(GlaImageEnsureActiveTileError),
    WrongImageBackend {
        expected: BackendId,
        actual: BackendId,
    },
}

impl Display for GlaImageUndoError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Atlas(error) => Display::fmt(error, f),
            Self::Image(error) => Display::fmt(error, f),
            Self::ImageEnsureActive(error) => Display::fmt(error, f),
            Self::WrongImageBackend { expected, actual } => write!(
                f,
                "image belongs to backend {}, expected backend {}",
                actual.raw(),
                expected.raw()
            ),
        }
    }
}

impl Error for GlaImageUndoError {}

impl From<AtlasError> for GlaImageUndoError {
    fn from(error: AtlasError) -> Self {
        Self::Atlas(error)
    }
}

impl From<GlaImageTileAccessError> for GlaImageUndoError {
    fn from(error: GlaImageTileAccessError) -> Self {
        Self::Image(error)
    }
}

impl From<GlaImageEnsureActiveTileError> for GlaImageUndoError {
    fn from(error: GlaImageEnsureActiveTileError) -> Self {
        Self::ImageEnsureActive(error)
    }
}

impl GlaImageUndo {
    pub fn new(image_backend: Backend, backup_backend: Backend) -> Self {
        let image_backend_id = image_backend.backend_id();
        let backup_backend_id = backup_backend.backend_id();
        Self {
            image_backend,
            image_backend_id,
            backup_backend,
            backup_backend_id,
        }
    }

    pub fn image_backend(&self) -> &Backend {
        &self.image_backend
    }

    pub fn image_backend_id(&self) -> BackendId {
        self.image_backend_id
    }

    pub fn backup_backend(&self) -> &Backend {
        &self.backup_backend
    }

    pub fn backup_backend_id(&self) -> BackendId {
        self.backup_backend_id
    }

    pub fn backends(&self) -> [&Backend; 2] {
        [&self.image_backend, &self.backup_backend]
    }

    pub fn backup_tiles(
        &self,
        image: &GlaImage,
        tile_indices: &[usize],
    ) -> Result<GlaImageUndoBackup, GlaImageUndoError> {
        self.validate_image_backend(image)?;

        let affected_tiles = stable_dedup_tile_indices(tile_indices);
        let mut backup_tile_indices = Vec::new();
        for &tile_index in &affected_tiles {
            if !image.is_tile_empty(tile_index)? {
                backup_tile_indices.push(tile_index);
            }
        }

        let backup_group = if backup_tile_indices.is_empty() {
            self.backup_backend.create_cached_group()?
        } else {
            self.backup_backend
                .alloc_cached(backup_tile_indices.len())?
        };
        let backup_tile_keys = backup_group.keys();

        let mut backup_key_cursor = 0usize;
        let mut tile_records = Vec::with_capacity(affected_tiles.len());
        let mut copy_commands = Vec::with_capacity(backup_tile_keys.len());
        for tile_index in affected_tiles {
            let source_tile_key = image.tile_key(tile_index)?;
            if source_tile_key.is_empty() {
                tile_records.push(GlaImageUndoTileRecord::new(
                    tile_index,
                    self.backup_backend.empty_tile_key(),
                ));
                continue;
            }

            let backup_tile_key = backup_tile_keys
                .get(backup_key_cursor)
                .copied()
                .ok_or(AtlasError::InvalidState)?;
            backup_key_cursor += 1;
            copy_commands.push(TileCopyCommand {
                source_tile_key,
                destination_tile_key: backup_tile_key,
            });
            tile_records.push(GlaImageUndoTileRecord::new(tile_index, backup_tile_key));
        }

        if backup_key_cursor != backup_tile_keys.len() {
            return Err(AtlasError::InvalidState.into());
        }

        Ok(GlaImageUndoBackup {
            backup_group,
            tile_records,
            copy_commands,
        })
    }

    pub fn execute_backup(
        &self,
        source_credential_pairs: &[(usize, TileCredential)],
    ) -> Result<BackupResult, GlaImageUndoError> {
        let image_manager = TileManager::from(self.image_backend.clone());
        let backup_manager = TileManager::from(self.backup_backend.clone());

        let mut non_empty_count = 0usize;
        for &(_, credential) in source_credential_pairs {
            let Some(tile_key) = image_manager.resolve(credential)? else {
                continue;
            };
            if !tile_key.is_empty() {
                non_empty_count += 1;
            }
        }

        let backup_group = if non_empty_count > 0 {
            backup_manager.backend().alloc_cached(non_empty_count)?
        } else {
            backup_manager.backend().create_cached_group()?
        };
        let backup_keys = backup_group.keys();
        let mut backup_key_cursor = 0usize;
        let mut commands = Vec::with_capacity(non_empty_count);
        let mut origin_keys = Vec::with_capacity(source_credential_pairs.len());

        for &(tile_index, credential) in source_credential_pairs {
            let source = image_manager.resolve(credential)?;
            let Some(source_key) = source else {
                origin_keys.push((tile_index, self.backup_backend.empty_tile_key()));
                continue;
            };
            if source_key.is_empty() {
                origin_keys.push((tile_index, self.backup_backend.empty_tile_key()));
                continue;
            }

            let backup_tile_key = backup_keys
                .get(backup_key_cursor)
                .copied()
                .ok_or(AtlasError::InvalidState)?;
            backup_key_cursor += 1;
            commands.push(RenderCommand::CopyTile(TileCopyCommand {
                source_tile_key: source_key,
                destination_tile_key: backup_tile_key,
            }));
            origin_keys.push((tile_index, backup_tile_key));
        }

        if backup_key_cursor != backup_keys.len() {
            return Err(AtlasError::InvalidState.into());
        }

        Ok(BackupResult {
            commands,
            backup_group,
            origin_keys,
        })
    }

    pub fn restore_tiles(
        &self,
        image: &mut GlaImage,
        backup_group: CachedTileGroup,
        tile_records: &[GlaImageUndoTileRecord],
    ) -> Result<GlaImageUndoRestore, GlaImageUndoError> {
        self.validate_image_backend(image)?;

        let mut tile_actions = Vec::with_capacity(tile_records.len());
        for record in tile_records {
            let source_tile_key = record.backup_tile_key();
            if !source_tile_key.is_empty() {
                let destination_tile_key = image.ensure_active_tile_key(record.tile_index())?;
                tile_actions.push(GlaImageUndoTileAction::RestoreFromBackup {
                    tile_index: record.tile_index(),
                    copy_command: TileCopyCommand {
                        source_tile_key,
                        destination_tile_key,
                    },
                });
                continue;
            }

            image.clear_tile(record.tile_index())?;
            tile_actions.push(GlaImageUndoTileAction::Clear {
                tile_index: record.tile_index(),
            });
        }

        Ok(GlaImageUndoRestore {
            backup_group,
            tile_actions,
        })
    }

    fn validate_image_backend(&self, image: &GlaImage) -> Result<(), GlaImageUndoError> {
        let actual = image.backend_id();
        if actual != self.image_backend_id {
            return Err(GlaImageUndoError::WrongImageBackend {
                expected: self.image_backend_id,
                actual,
            });
        }
        Ok(())
    }
}

impl GlaImageUndoTileRecord {
    pub const fn new(tile_index: usize, backup_tile_key: TileKey) -> Self {
        Self {
            tile_index,
            backup_tile_key,
        }
    }

    pub const fn tile_index(&self) -> usize {
        self.tile_index
    }

    pub const fn backup_tile_key(&self) -> TileKey {
        self.backup_tile_key
    }
}

impl GlaImageUndoBackup {
    pub fn backup_group(&self) -> &CachedTileGroup {
        &self.backup_group
    }

    pub fn into_backup_group(self) -> CachedTileGroup {
        self.backup_group
    }

    pub fn tile_records(&self) -> &[GlaImageUndoTileRecord] {
        &self.tile_records
    }

    pub fn copy_commands(&self) -> &[TileCopyCommand] {
        &self.copy_commands
    }

    pub fn into_parts(
        self,
    ) -> (
        CachedTileGroup,
        Vec<GlaImageUndoTileRecord>,
        Vec<TileCopyCommand>,
    ) {
        (self.backup_group, self.tile_records, self.copy_commands)
    }
}

impl GlaImageUndoRestore {
    pub fn backup_group(&self) -> &CachedTileGroup {
        &self.backup_group
    }

    pub fn tile_actions(&self) -> &[GlaImageUndoTileAction] {
        &self.tile_actions
    }
}

fn stable_dedup_tile_indices(tile_indices: &[usize]) -> Vec<usize> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for &tile_index in tile_indices {
        if seen.insert(tile_index) {
            result.push(tile_index);
        }
    }
    result
}
