use atlas::CachedTileGroup;
use gla_undo::{GlaImageUndoRestore, GlaImageUndoTileRecord};

use crate::node::GlaNodeId;

#[derive(Debug)]
pub struct DocumentUndoEntry {
    pub(crate) node_id: GlaNodeId,
    pub(crate) backup_group: CachedTileGroup,
    pub(crate) tile_records: Vec<GlaImageUndoTileRecord>,
}

#[derive(Debug)]
pub struct DocumentUndoStack {
    entries: Vec<DocumentUndoEntry>,
}

#[derive(Debug)]
pub struct GlaDocUndoRestore {
    pub(crate) node_id: GlaNodeId,
    pub(crate) image_restore: GlaImageUndoRestore,
}

impl DocumentUndoEntry {
    pub fn node_id(&self) -> GlaNodeId {
        self.node_id
    }

    pub fn backup_group(&self) -> &CachedTileGroup {
        &self.backup_group
    }

    pub fn tile_records(&self) -> &[GlaImageUndoTileRecord] {
        &self.tile_records
    }
}

impl DocumentUndoStack {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
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
        tile_records: Vec<GlaImageUndoTileRecord>,
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

    pub fn image_restore(&self) -> &GlaImageUndoRestore {
        &self.image_restore
    }
}

impl Default for DocumentUndoStack {
    fn default() -> Self {
        Self::new()
    }
}
