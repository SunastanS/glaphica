use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasError, Backend, BackendId, CachedTileGroup, TileKey, TileOwner};
use gla_document::DocumentBackupStore;
use gla_image::{GlaImage, GlaImageTileAccessError};
pub use glaphica_core::BrushId;
use renderer::{
    ApplyDabCommand, BrushShaderSpec, CopyTileCommand, MergeTileCommand, RenderCommand,
};

pub mod round;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrushIntermediateTile {
    pub tile_index: usize,
    pub tile_key: TileKey,
}

#[derive(Debug)]
struct SparseTileOwners {
    backend_id: BackendId,
    tile_indices: Vec<usize>,
    tile_owners: Vec<TileOwner>,
}

impl SparseTileOwners {
    fn new(backend_id: BackendId) -> Self {
        Self {
            backend_id,
            tile_indices: Vec::new(),
            tile_owners: Vec::new(),
        }
    }

    fn backend_id(&self) -> BackendId {
        self.backend_id
    }

    fn tile_key(&self, tile_index: usize) -> Option<TileKey> {
        let sparse_index = self
            .tile_indices
            .iter()
            .position(|&stored_tile_index| stored_tile_index == tile_index)?;
        Some(self.tile_owners.get(sparse_index)?.tile_key())
    }

    fn tiles(&self) -> impl Iterator<Item = (usize, TileKey)> + '_ {
        self.tile_indices
            .iter()
            .copied()
            .zip(self.tile_owners.iter().map(TileOwner::tile_key))
    }

    fn ensure_tile(
        &mut self,
        tile_index: usize,
        backend: &Backend,
    ) -> Result<TileKey, AtlasError> {
        if let Some(tile_key) = self.tile_key(tile_index) {
            return Ok(tile_key);
        }

        let tile_owner = backend.alloc_active()?;
        let tile_key = tile_owner.tile_key();
        self.tile_indices.push(tile_index);
        self.tile_owners.push(tile_owner);
        Ok(tile_key)
    }
}

#[derive(Debug)]
pub struct BrushIntermediate {
    tiles: SparseTileOwners,
}

impl BrushIntermediate {
    pub fn backend_id(&self) -> BackendId {
        self.tiles.backend_id()
    }

    pub fn tile_key(&self, tile_index: usize) -> Option<TileKey> {
        self.tiles.tile_key(tile_index)
    }

    pub fn tiles(&self) -> impl Iterator<Item = BrushIntermediateTile> + '_ {
        self.tiles
            .tiles()
            .map(|(tile_index, tile_key)| BrushIntermediateTile {
                tile_index,
                tile_key,
            })
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrushStrokeError {
    Atlas(AtlasError),
    Image(GlaImageTileAccessError),
    WrongImageBackend {
        expected: BackendId,
        actual: BackendId,
    },
}

impl Display for BrushStrokeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Atlas(error) => Display::fmt(error, f),
            Self::Image(error) => Display::fmt(error, f),
            Self::WrongImageBackend { expected, actual } => write!(
                f,
                "commit image backend is {}, but provided backend is {}",
                actual.raw(),
                expected.raw()
            ),
        }
    }
}

impl Error for BrushStrokeError {}

impl From<AtlasError> for BrushStrokeError {
    fn from(error: AtlasError) -> Self {
        Self::Atlas(error)
    }
}

impl From<GlaImageTileAccessError> for BrushStrokeError {
    fn from(error: GlaImageTileAccessError) -> Self {
        Self::Image(error)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StrokeCommitBatch {
    pub backup_group: CachedTileGroup,
    pub backup_tile_indices: Vec<usize>,
    pub backup_tile_keys: Vec<TileKey>,
    pub commands: Vec<RenderCommand>,
}

#[derive(Debug)]
pub struct BrushStrokeState {
    brush_id: BrushId,
    intermediate_backend: Backend,
    intermediate_backend_id: BackendId,
    intermediate: BrushIntermediate,
}

impl BrushStrokeState {
    pub fn new(brush_id: BrushId, intermediate_backend: Backend) -> Result<Self, BrushStrokeError> {
        let intermediate_backend_id = intermediate_backend.backend_id()?;
        Ok(Self {
            brush_id,
            intermediate_backend,
            intermediate_backend_id,
            intermediate: BrushIntermediate {
                tiles: SparseTileOwners::new(intermediate_backend_id),
            },
        })
    }

    pub fn brush_id(&self) -> BrushId {
        self.brush_id
    }

    pub fn intermediate_backend_id(&self) -> BackendId {
        self.intermediate_backend_id
    }

    pub fn intermediate(&self) -> &BrushIntermediate {
        &self.intermediate
    }

    pub fn push_apply_dab(
        &mut self,
        tile_index: usize,
        reference_tile_key: Option<TileKey>,
        parameters: Vec<f32>,
        output: &mut Vec<RenderCommand>,
    ) -> Result<TileKey, BrushStrokeError> {
        let destination_tile_key = self
            .intermediate
            .tiles
            .ensure_tile(tile_index, &self.intermediate_backend)?;
        output.push(RenderCommand::ApplyDab(ApplyDabCommand {
            brush_id: self.brush_id,
            destination_tile_key,
            reference_tile_key,
            parameters,
        }));
        Ok(destination_tile_key)
    }

    pub fn push_preview_merge(
        &self,
        tile_index: usize,
        origin_tile_key: TileKey,
        preview_tile_key: TileKey,
        backup_tile_key: Option<TileKey>,
        parameters: Vec<f32>,
        output: &mut Vec<RenderCommand>,
    ) -> Option<TileKey> {
        let Some(intermediate_tile_key) = self.intermediate.tile_key(tile_index) else {
            return None;
        };
        output.push(RenderCommand::MergeTile(MergeTileCommand {
            brush_id: self.brush_id,
            origin_tile_key,
            intermediate_tile_key,
            destination_tile_key: preview_tile_key,
            backup_tile_key,
            parameters,
        }));
        Some(preview_tile_key)
    }

    pub fn build_commit_batch(
        &self,
        image: &mut GlaImage,
        image_backend: &Backend,
        backup_store: &mut DocumentBackupStore,
        tile_indices: &[usize],
        parameters: Vec<f32>,
    ) -> Result<StrokeCommitBatch, BrushStrokeError> {
        let image_backend_id = image_backend.backend_id()?;
        if image.backend() != image_backend_id {
            return Err(BrushStrokeError::WrongImageBackend {
                expected: image_backend_id,
                actual: image.backend(),
            });
        }

        let mut affected_tiles = tile_indices.to_vec();
        affected_tiles.sort_unstable();
        affected_tiles.dedup();

        let mut backup_tile_indices = Vec::new();
        let mut active_tile_keys = Vec::new();
        let mut had_active_tile = Vec::new();
        for &tile_index in &affected_tiles {
            if self.intermediate.tile_key(tile_index).is_none() {
                continue;
            }
            let active_tile_key = image
                .tile_key(tile_index)
                .ok_or(GlaImageTileAccessError::OutOfBounds)?;
            had_active_tile.push(active_tile_key != TileKey::EMPTY);
            if active_tile_key != TileKey::EMPTY {
                backup_tile_indices.push(tile_index);
                active_tile_keys.push(active_tile_key);
            }
        }

        let backup_group = backup_store.retain_cached_group(active_tile_keys.len())?;
        let backup_tile_keys = backup_group.keys().to_vec();
        let mut commands = Vec::new();
        let mut backup_key_cursor = 0usize;
        let mut affected_cursor = 0usize;

        for &tile_index in &affected_tiles {
            let Some(intermediate_tile_key) = self.intermediate.tile_key(tile_index) else {
                continue;
            };
            let had_active_tile = *had_active_tile
                .get(affected_cursor)
                .ok_or(AtlasError::InvalidState)?;
            affected_cursor += 1;
            let destination_tile_key = ensure_image_active_tile(image, tile_index, image_backend)?;
            let backup_tile_key = if had_active_tile {
                let backup_tile_key = backup_tile_keys
                    .get(backup_key_cursor)
                    .copied()
                    .ok_or(AtlasError::InvalidState)?;
                commands.push(RenderCommand::CopyTile(CopyTileCommand {
                    source_tile_key: destination_tile_key,
                    destination_tile_key: backup_tile_key,
                }));
                backup_key_cursor += 1;
                Some(backup_tile_key)
            } else {
                None
            };

            commands.push(RenderCommand::MergeTile(MergeTileCommand {
                brush_id: self.brush_id,
                origin_tile_key: backup_tile_key.unwrap_or(TileKey::EMPTY),
                intermediate_tile_key,
                destination_tile_key,
                backup_tile_key,
                parameters: parameters.clone(),
            }));
        }

        if backup_key_cursor != backup_tile_keys.len() {
            return Err(AtlasError::InvalidState.into());
        }
        if affected_cursor != had_active_tile.len() {
            return Err(AtlasError::InvalidState.into());
        }

        Ok(StrokeCommitBatch {
            backup_group,
            backup_tile_indices,
            backup_tile_keys,
            commands,
        })
    }
}

fn ensure_image_active_tile(
    image: &mut GlaImage,
    tile_index: usize,
    image_backend: &Backend,
) -> Result<TileKey, BrushStrokeError> {
    let tile_key = image
        .tile_key(tile_index)
        .ok_or(GlaImageTileAccessError::OutOfBounds)?;
    if tile_key != TileKey::EMPTY {
        return Ok(tile_key);
    }

    let tile_owner = image_backend.alloc_active()?;
    let previous = image.replace_tile_owner(tile_index, tile_owner)?;
    if previous.tile_key() != TileKey::EMPTY {
        return Err(AtlasError::InvalidState.into());
    }

    image.tile_key(tile_index).ok_or(GlaImageTileAccessError::OutOfBounds.into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrushShaderRegistration {
    pub brush_id: BrushId,
    pub shader_spec: BrushShaderSpec,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BrushRegistry {
    registrations: Vec<BrushShaderRegistration>,
}

impl BrushRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, registration: BrushShaderRegistration) {
        if let Some(index) = self
            .registrations
            .iter()
            .position(|candidate| candidate.brush_id == registration.brush_id)
        {
            self.registrations[index] = registration;
            return;
        }
        self.registrations.push(registration);
    }

    pub fn shader_spec(&self, brush_id: BrushId) -> Option<BrushShaderSpec> {
        self.registrations
            .iter()
            .find(|registration| registration.brush_id == brush_id)
            .map(|registration| registration.shader_spec)
    }

    pub fn registration(&self, brush_id: BrushId) -> Option<&BrushShaderRegistration> {
        self.registrations
            .iter()
            .find(|registration| registration.brush_id == brush_id)
    }
}

#[cfg(test)]
mod tests {
    use atlas::{AtlasLayout, BackendId, TileState};
    use glaphica_core::IMAGE_TILE_SIZE;
    use renderer::{ApplyDabCommand, CopyTileCommand, MergeTileCommand, RenderCommand};

    use crate::{
        BrushId, BrushStrokeState, StrokeTileBackupCopy, StrokeTileBackupError,
        StrokeTileBackupSession,
    };
    use atlas::{Backend, TileKey};
    use gla_document::DocumentBackupStore;
    use gla_image::{GlaImage, GlaImageLayout};

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
    fn apply_dab_allocates_intermediate_tile_once() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(7));
        let mut state =
            BrushStrokeState::new(BrushId::new(5), backend.clone()).expect("state should build");
        let mut commands = Vec::new();

        let first = state
            .push_apply_dab(4, None, vec![1.0, 2.0], &mut commands)
            .expect("dab should build");
        let second = state
            .push_apply_dab(4, Some(first), vec![3.0], &mut commands)
            .expect("dab should build");

        assert_eq!(first, second);
        assert_eq!(state.intermediate().tile_key(4), Some(first));
        assert_eq!(
            commands,
            vec![
                RenderCommand::ApplyDab(ApplyDabCommand {
                    brush_id: BrushId::new(5),
                    destination_tile_key: first,
                    reference_tile_key: None,
                    parameters: vec![1.0, 2.0],
                }),
                RenderCommand::ApplyDab(ApplyDabCommand {
                    brush_id: BrushId::new(5),
                    destination_tile_key: first,
                    reference_tile_key: Some(first),
                    parameters: vec![3.0],
                }),
            ]
        );
        assert_eq!(backend.tile_state(first), Ok(TileState::Active));
    }

    #[test]
    fn preview_merge_uses_virtual_preview_node_tile() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(7));
        let active_tile = backend.alloc_active().expect("active tile");
        let active_tile_key = active_tile.tile_key();

        let mut state =
            BrushStrokeState::new(BrushId::new(9), backend).expect("state should build");
        let mut commands = Vec::new();
        let intermediate_tile_key = state
            .push_apply_dab(0, None, vec![1.0], &mut commands)
            .expect("dab");
        commands.clear();
        let preview_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(13));
        let preview_tile_key = preview_backend
            .alloc_active()
            .expect("preview tile")
            .tile_key();

        let returned_tile_key = state
            .push_preview_merge(0, active_tile_key, preview_tile_key, None, vec![9.0], &mut commands)
            .expect("preview merge should allocate");

        assert_eq!(returned_tile_key, preview_tile_key);
        assert_eq!(
            commands,
            vec![RenderCommand::MergeTile(MergeTileCommand {
                brush_id: BrushId::new(9),
                origin_tile_key: active_tile_key,
                intermediate_tile_key,
                destination_tile_key: preview_tile_key,
                backup_tile_key: None,
                parameters: vec![9.0],
            })]
        );
    }

    #[test]
    fn commit_batch_copies_non_empty_active_tiles_before_merge() {
        let brush_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(7));
        let image_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(3));
        let backup_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(5));
        let mut image = GlaImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE),
            BackendId::new(3),
        )
        .expect("image should create");
        let first_active = image_backend.alloc_active().expect("first active");
        let first_active_key = first_active.tile_key();
        image.replace_tile_owner(0, first_active).expect("install tile");

        let mut state =
            BrushStrokeState::new(BrushId::new(11), brush_backend).expect("state should build");
        let mut backup_store =
            DocumentBackupStore::new(backup_backend).expect("backup store should build");
        let mut draw_commands = Vec::new();
        let first_intermediate = state
            .push_apply_dab(0, None, vec![1.0], &mut draw_commands)
            .expect("dab");
        let second_intermediate = state
            .push_apply_dab(1, None, vec![2.0], &mut draw_commands)
            .expect("dab");

        let batch = state
            .build_commit_batch(&mut image, &image_backend, &mut backup_store, &[1, 0], vec![7.0])
            .expect("commit batch should build");

        let second_active_key = image.tile_key(1).expect("tile key should exist");
        assert_ne!(second_active_key, TileKey::EMPTY);
        assert_eq!(batch.backup_tile_indices, vec![0]);
        assert_eq!(batch.backup_tile_keys.len(), 1);
        assert_eq!(
            batch.commands,
            vec![
                RenderCommand::CopyTile(CopyTileCommand {
                    source_tile_key: first_active_key,
                    destination_tile_key: batch.backup_tile_keys[0],
                }),
                RenderCommand::MergeTile(MergeTileCommand {
                    brush_id: BrushId::new(11),
                    origin_tile_key: batch.backup_tile_keys[0],
                    intermediate_tile_key: first_intermediate,
                    destination_tile_key: first_active_key,
                    backup_tile_key: Some(batch.backup_tile_keys[0]),
                    parameters: vec![7.0],
                }),
                RenderCommand::MergeTile(MergeTileCommand {
                    brush_id: BrushId::new(11),
                    origin_tile_key: TileKey::EMPTY,
                    intermediate_tile_key: second_intermediate,
                    destination_tile_key: second_active_key,
                    backup_tile_key: None,
                    parameters: vec![7.0],
                }),
            ]
        );
    }

    #[test]
    fn preview_merge_uses_explicit_origin_tile_key() {
        let brush_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(7));
        let origin_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(3));
        let active_tile = origin_backend.alloc_active().expect("active tile");
        let active_tile_key = active_tile.tile_key();
        let mut state =
            BrushStrokeState::new(BrushId::new(11), brush_backend).expect("state should build");
        let mut commands = Vec::new();
        let intermediate_tile_key = state
            .push_apply_dab(0, None, vec![4.0], &mut commands)
            .expect("dab");
        commands.clear();
        let preview_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(13));
        let preview_tile_key = preview_backend
            .alloc_active()
            .expect("preview tile")
            .tile_key();

        let returned_tile_key = state
            .push_preview_merge(0, active_tile_key, preview_tile_key, None, vec![1.0], &mut commands)
            .expect("preview merge should allocate");
        assert_eq!(returned_tile_key, preview_tile_key);

        assert_eq!(
            commands,
            vec![RenderCommand::MergeTile(MergeTileCommand {
                brush_id: BrushId::new(11),
                origin_tile_key: active_tile_key,
                intermediate_tile_key,
                destination_tile_key: preview_tile_key,
                backup_tile_key: None,
                parameters: vec![1.0],
            })]
        );
    }
}
