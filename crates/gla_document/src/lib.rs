mod backup;
mod document;
mod node;
mod render;
mod storage;

pub use atlas::BackendId;
pub use gla_image::{GlaImage, GlaImageCreateError, GlaImageLayout};
pub use glaphica_core::BlendMode;

pub use gla_undo::{
    GlaImageUndo, GlaImageUndoBackup, GlaImageUndoError, GlaImageUndoTileRecord, RestoreResult,
};

pub use crate::backup::{DocumentUndoStack, GlaDocUndoRestore};
pub use crate::document::{GlaDoc, GlaDocError, GlaDocUndoError};
pub use crate::node::{GlaBranchNode, GlaLeafNode, GlaNode, GlaNodeId, GlaNodeKind};
pub use crate::render::{
    GlaCompositeCommand, GlaRenderPass, GlaRenderRefresh, GlaRenderRefreshKind, GlaRenderSource,
    GlaRenderTarget,
};
pub use crate::storage::{
    GlaDocLeafSource, GlaDocLoadResult, GlaDocStorageError, GlaDocTileAsset,
    tile_asset_relative_path, write_tile_asset_file,
};
