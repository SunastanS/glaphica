use atlas::{AtlasError, Backend, BackendId, CachedTileGroup, TileKey};

use crate::node::GlaNodeId;

#[derive(Debug)]
pub struct DocumentBackupStore {
    backend: Backend,
    backend_id: BackendId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentUndoTileRecord {
    pub(crate) tile_index: usize,
    pub(crate) backup_tile_key: Option<TileKey>,
}

#[derive(Debug)]
pub struct DocumentUndoEntry {
    pub(crate) node_id: GlaNodeId,
    pub(crate) backup_group: CachedTileGroup,
    pub(crate) tile_records: Vec<DocumentUndoTileRecord>,
}

#[derive(Debug)]
pub struct DocumentUndoStack {
    backup_store: DocumentBackupStore,
    entries: Vec<DocumentUndoEntry>,
}

#[derive(Debug)]
pub struct GlaDocUndoRestore {
    pub(crate) node_id: GlaNodeId,
    pub(crate) backup_group: CachedTileGroup,
    pub(crate) tile_actions: Vec<GlaDocUndoTileAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlaDocUndoTileAction {
    RestoreFromBackup {
        tile_index: usize,
        source_tile_key: TileKey,
        destination_tile_key: TileKey,
    },
    Clear {
        tile_index: usize,
    },
}

impl DocumentBackupStore {
    pub fn new(backend: Backend) -> Result<Self, AtlasError> {
        let backend_id = backend.backend_id()?;
        Ok(Self {
            backend,
            backend_id,
        })
    }

    pub fn backend(&self) -> &Backend {
        &self.backend
    }

    pub fn backend_id(&self) -> BackendId {
        self.backend_id
    }

    pub fn retain_cached_group(
        &mut self,
        non_empty_tile_count: usize,
    ) -> Result<CachedTileGroup, AtlasError> {
        let cached_group = if non_empty_tile_count == 0 {
            self.backend.create_cached_group()?
        } else {
            self.backend.alloc_cached(non_empty_tile_count)?
        };
        Ok(cached_group)
    }
}

impl DocumentUndoTileRecord {
    pub const fn new(tile_index: usize, backup_tile_key: Option<TileKey>) -> Self {
        Self {
            tile_index,
            backup_tile_key,
        }
    }

    pub const fn tile_index(&self) -> usize {
        self.tile_index
    }

    pub const fn backup_tile_key(&self) -> Option<TileKey> {
        self.backup_tile_key
    }
}

impl DocumentUndoEntry {
    pub fn node_id(&self) -> GlaNodeId {
        self.node_id
    }

    pub fn backup_group(&self) -> &CachedTileGroup {
        &self.backup_group
    }

    pub fn tile_records(&self) -> &[DocumentUndoTileRecord] {
        &self.tile_records
    }
}

impl DocumentUndoStack {
    pub fn new(backup_backend: Backend) -> Result<Self, AtlasError> {
        Ok(Self {
            backup_store: DocumentBackupStore::new(backup_backend)?,
            entries: Vec::new(),
        })
    }

    pub fn backup_store(&self) -> &DocumentBackupStore {
        &self.backup_store
    }

    pub fn backup_store_mut(&mut self) -> &mut DocumentBackupStore {
        &mut self.backup_store
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn push_entry(
        &mut self,
        node_id: GlaNodeId,
        backup_group: CachedTileGroup,
        tile_records: Vec<DocumentUndoTileRecord>,
    ) {
        self.entries.push(DocumentUndoEntry {
            node_id,
            backup_group,
            tile_records,
        });
    }

    pub(crate) fn pop_entry(&mut self) -> Option<DocumentUndoEntry> {
        self.entries.pop()
    }
}

impl GlaDocUndoRestore {
    pub fn node_id(&self) -> GlaNodeId {
        self.node_id
    }

    pub fn backup_group(&self) -> &CachedTileGroup {
        &self.backup_group
    }

    pub fn tile_actions(&self) -> &[GlaDocUndoTileAction] {
        &self.tile_actions
    }
}
