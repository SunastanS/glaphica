use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::OnceLock;

use render_protocol::{
    BlendMode, ImageHandle, LayerId, RenderNodeSnapshot, RenderTreeSnapshot, StrokeSessionId,
};
use slotmap::SlotMap;
use tiles::{DirtySinceResult, TILE_SIZE, TileDirtyBitset, TileDirtyQuery, TileImage, TileKey};

#[cfg(test)]
use tiles::{TileKeyMapping, apply_tile_key_mappings};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerNodeId(u64);

impl LayerNodeId {
    pub const ROOT: Self = Self(0);
}

pub struct Document {
    layer_tree: LayerTreeNode,
    images: SlotMap<ImageHandle, Arc<TileImage>>,
    size_x: u32,
    size_y: u32,
    revision: u64,
    render_tree_revision: u64,
    next_layer_id: u64,
    render_tree_cache_dirty: bool,
    layer_versions: HashMap<LayerId, u64>,
    layer_dirty_history: HashMap<LayerId, LayerDirtyHistory>,
    active_merge: Option<ActiveMerge>,
    active_preview_buffer: Option<ActivePreviewBuffer>,
    consumed_stroke_sessions: HashSet<StrokeSessionId>,
}

pub enum LayerTreeNode {
    Root {
        children: Vec<LayerTreeNode>,
    },
    Branch {
        id: LayerNodeId,
        blend: BlendMode,
        children: Vec<LayerTreeNode>,
    },
    Leaf {
        id: LayerNodeId,
        blend: BlendMode,
        image_handle: ImageHandle,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupError {
    NotSameLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileCoordinate {
    pub tile_x: u32,
    pub tile_y: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeCommitSummary {
    pub revision: u64,
    pub layer_id: LayerId,
    pub stroke_session_id: StrokeSessionId,
    pub dirty_tiles: Vec<TileCoordinate>,
    pub full_layer_dirty: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentMergeError {
    LayerNotFound {
        layer_id: LayerId,
    },
    LayerNotFoundInStrokeSession {
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
    },
    LayerIsNotLeaf {
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
    },
    RevisionMismatch {
        expected_revision: u64,
        actual_revision: u64,
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
    },
    ActiveMergeExists {
        active_layer_id: LayerId,
        active_stroke_session_id: StrokeSessionId,
        requested_layer_id: LayerId,
        requested_stroke_session_id: StrokeSessionId,
    },
    MissingActiveMerge {
        layer_id: LayerId,
        stroke_session_id: Option<StrokeSessionId>,
    },
    MergeContextMismatch {
        expected_layer_id: LayerId,
        expected_stroke_session_id: StrokeSessionId,
        actual_layer_id: LayerId,
        actual_stroke_session_id: StrokeSessionId,
    },
    DuplicateFinalizeOrAbort {
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
    },
    RevisionOverflow {
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
    },
}

impl DocumentMergeError {
    fn with_stroke_session(self, stroke_session_id: StrokeSessionId) -> Self {
        match self {
            Self::LayerNotFound { layer_id } => Self::LayerNotFoundInStrokeSession {
                layer_id,
                stroke_session_id,
            },
            other => other,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ActiveMerge {
    layer_id: LayerId,
    stroke_session_id: StrokeSessionId,
}

#[derive(Debug, Clone, Copy)]
struct ActivePreviewBuffer {
    layer_id: LayerId,
    stroke_session_id: StrokeSessionId,
}

#[derive(Debug, Clone, Copy)]
struct LayerLookupResult {
    is_leaf: bool,
    image_handle: Option<ImageHandle>,
}

const LAYER_DIRTY_HISTORY_CAPACITY: usize = 200;

#[derive(Debug, Clone)]
struct LayerDirtyHistoryEntry {
    version: u64,
    dirty_tiles: TileDirtyBitset,
}

#[derive(Debug, Clone)]
struct LayerDirtyHistory {
    tiles_per_row: u32,
    tiles_per_column: u32,
    entries: Vec<LayerDirtyHistoryEntry>,
}

impl LayerDirtyHistory {
    fn new(tiles_per_row: u32, tiles_per_column: u32) -> Self {
        Self {
            tiles_per_row,
            tiles_per_column,
            entries: Vec::new(),
        }
    }

    fn record(&mut self, version: u64, dirty_tiles: TileDirtyBitset) {
        if dirty_tiles.is_empty() {
            return;
        }
        if let Some(last) = self.entries.last_mut() {
            if last.version == version {
                last.dirty_tiles
                    .merge_from(&dirty_tiles)
                    .unwrap_or_else(|error| panic!("layer dirty history merge failed: {error:?}"));
                return;
            }
        }
        self.entries.push(LayerDirtyHistoryEntry {
            version,
            dirty_tiles,
        });
        if self.entries.len() > LAYER_DIRTY_HISTORY_CAPACITY {
            let remove_count = self.entries.len() - LAYER_DIRTY_HISTORY_CAPACITY;
            self.entries.drain(0..remove_count);
        }
    }

    fn query(&self, since_version: u64, latest_version: u64) -> DirtySinceResult {
        if document_perf_log_enabled() {
            eprintln!(
                "[document_perf] layer_dirty_query begin since_version={} latest_version={} history_entries={}",
                since_version,
                latest_version,
                self.entries.len(),
            );
        }
        if since_version >= latest_version {
            if document_perf_log_enabled() {
                eprintln!("[document_perf] layer_dirty_query result=up_to_date");
            }
            return DirtySinceResult::UpToDate;
        }
        if self.entries.is_empty() {
            let dirty_tiles = TileDirtyBitset::new(self.tiles_per_row, self.tiles_per_column)
                .unwrap_or_else(|error| panic!("layer dirty bitset init failed: {error:?}"));
            if document_perf_log_enabled() {
                eprintln!(
                    "[document_perf] layer_dirty_query result=has_changes_empty reason=version_advanced_without_recorded_tiles"
                );
            }
            return DirtySinceResult::HasChanges(TileDirtyQuery {
                latest_version,
                dirty_tiles,
            });
        }
        if let Some(oldest) = self.entries.first() {
            let oldest_queryable = oldest.version.saturating_sub(1);
            if since_version < oldest_queryable {
                if document_perf_log_enabled() {
                    eprintln!(
                        "[document_perf] layer_dirty_query result=history_truncated oldest_entry_version={} oldest_queryable={}",
                        oldest.version, oldest_queryable,
                    );
                }
                return DirtySinceResult::HistoryTruncated;
            }
        }
        let mut dirty_tiles = TileDirtyBitset::new(self.tiles_per_row, self.tiles_per_column)
            .unwrap_or_else(|error| panic!("layer dirty bitset init failed: {error:?}"));
        let mut found = false;
        for entry in self
            .entries
            .iter()
            .filter(|entry| entry.version > since_version)
        {
            dirty_tiles
                .merge_from(&entry.dirty_tiles)
                .unwrap_or_else(|error| panic!("layer dirty bitset merge failed: {error:?}"));
            found = true;
        }
        if found {
            if document_perf_log_enabled() {
                eprintln!(
                    "[document_perf] layer_dirty_query result=has_changes dirty_full={} dirty_count={}",
                    dirty_tiles.is_full(),
                    dirty_tiles.iter_dirty_tiles().count(),
                );
            }
            DirtySinceResult::HasChanges(TileDirtyQuery {
                latest_version,
                dirty_tiles,
            })
        } else {
            if document_perf_log_enabled() {
                eprintln!(
                    "[document_perf] layer_dirty_query result=history_truncated reason=no_entries_newer_than_since"
                );
            }
            DirtySinceResult::HistoryTruncated
        }
    }
}

fn document_perf_log_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("GLAPHICA_PERF_LOG").is_some_and(|value| value != "0"))
}

impl Document {
    pub fn new(size_x: u32, size_y: u32) -> Self {
        Self {
            layer_tree: LayerTreeNode::Root {
                children: Vec::new(),
            },
            images: SlotMap::with_key(),
            size_x,
            size_y,
            revision: 0,
            render_tree_revision: 0,
            next_layer_id: 1,
            render_tree_cache_dirty: true,
            layer_versions: HashMap::new(),
            layer_dirty_history: HashMap::new(),
            active_merge: None,
            active_preview_buffer: None,
            consumed_stroke_sessions: HashSet::new(),
        }
    }

    pub fn size_x(&self) -> u32 {
        self.size_x
    }

    pub fn size_y(&self) -> u32 {
        self.size_y
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn render_tree_revision(&self) -> u64 {
        self.render_tree_revision
    }

    pub fn render_tree_snapshot(&self) -> RenderTreeSnapshot {
        RenderTreeSnapshot {
            revision: self.render_tree_revision,
            root: Arc::new(
                self.layer_tree
                    .build_render_node_snapshot(self.active_preview_buffer),
            ),
        }
    }

    pub fn set_active_preview_buffer(
        &mut self,
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
    ) -> Result<(), DocumentMergeError> {
        let layer_lookup = self.lookup_leaf_image_handle(layer_id)?;
        if !layer_lookup.is_leaf {
            return Err(DocumentMergeError::LayerIsNotLeaf {
                layer_id,
                stroke_session_id,
            });
        }

        self.active_preview_buffer = Some(ActivePreviewBuffer {
            layer_id,
            stroke_session_id,
        });
        self.mark_render_tree_dirty();
        Ok(())
    }

    pub fn clear_active_preview_buffer(&mut self, stroke_session_id: StrokeSessionId) -> bool {
        let should_clear = matches!(
            self.active_preview_buffer,
            Some(ActivePreviewBuffer {
                stroke_session_id: active_stroke_session_id,
                ..
            }) if active_stroke_session_id == stroke_session_id
        );
        if should_clear {
            self.active_preview_buffer = None;
            self.mark_render_tree_dirty();
        }
        should_clear
    }

    pub fn root(&self) -> &[LayerTreeNode] {
        match &self.layer_tree {
            LayerTreeNode::Root { children } => children,
            _ => unreachable!("document must always store a root node at top-level"),
        }
    }

    fn root_mut(&mut self) -> &mut Vec<LayerTreeNode> {
        match &mut self.layer_tree {
            LayerTreeNode::Root { children } => children,
            _ => unreachable!("document must always store a root node at top-level"),
        }
    }

    pub fn image(&self, image_handle: ImageHandle) -> Option<Arc<TileImage>> {
        self.images.get(image_handle).cloned()
    }

    pub fn leaf_image_handle(
        &self,
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
    ) -> Result<ImageHandle, DocumentMergeError> {
        let layer_lookup = self
            .lookup_leaf_image_handle(layer_id)
            .map_err(|error| error.with_stroke_session(stroke_session_id))?;
        if !layer_lookup.is_leaf {
            return Err(DocumentMergeError::LayerIsNotLeaf {
                layer_id,
                stroke_session_id,
            });
        }
        layer_lookup
            .image_handle
            .ok_or(DocumentMergeError::LayerNotFoundInStrokeSession {
                layer_id,
                stroke_session_id,
            })
    }

    pub(crate) fn replace_leaf_image(
        &mut self,
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
        image: TileImage,
    ) -> Result<(), DocumentMergeError> {
        let image_handle = self.leaf_image_handle(layer_id, stroke_session_id)?;
        let next_layer_version = self
            .layer_versions
            .get(&layer_id)
            .copied()
            .unwrap_or_else(|| panic!("layer version for {layer_id} is missing"));
        let next_layer_version = next_layer_version
            .checked_add(1)
            .unwrap_or_else(|| panic!("layer version overflow for {layer_id}"));
        self.layer_versions.insert(layer_id, next_layer_version);
        let dirty_tiles = self.full_layer_dirty_tiles();
        self.record_layer_dirty_history(layer_id, next_layer_version, dirty_tiles);
        if let Some(image_entry) = self.images.get_mut(image_handle) {
            *image_entry = Arc::new(image);
            return Ok(());
        }
        Err(DocumentMergeError::LayerNotFoundInStrokeSession {
            layer_id,
            stroke_session_id,
        })
    }

    pub fn apply_merge_image(
        &mut self,
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
        image: TileImage,
        dirty_tiles: &[TileCoordinate],
        full_layer_dirty: bool,
    ) -> Result<MergeCommitSummary, DocumentMergeError> {
        self.validate_active_merge(layer_id, stroke_session_id)?;
        let image_handle = self.leaf_image_handle(layer_id, stroke_session_id)?;
        let previous_version = self
            .layer_versions
            .get(&layer_id)
            .copied()
            .unwrap_or_else(|| panic!("layer version for {layer_id} is missing"));
        let next_version = previous_version
            .checked_add(1)
            .unwrap_or_else(|| panic!("layer version overflow for {layer_id}"));
        let new_image_handle = self.images.insert(Arc::new(image));
        let replaced = self
            .layer_tree
            .replace_leaf_image_handle(layer_id, new_image_handle);
        if !replaced {
            return Err(DocumentMergeError::LayerNotFoundInStrokeSession {
                layer_id,
                stroke_session_id,
            });
        }
        self.layer_versions.insert(layer_id, next_version);
        let dirty_tile_mask = self.build_dirty_tile_mask(dirty_tiles, full_layer_dirty);
        self.record_layer_dirty_history(layer_id, next_version, dirty_tile_mask);
        self.images.remove(image_handle);
        let next_revision =
            self.revision
                .checked_add(1)
                .ok_or(DocumentMergeError::RevisionOverflow {
                    layer_id,
                    stroke_session_id,
                })?;
        self.revision = next_revision;
        self.active_merge = None;
        if matches!(
            self.active_preview_buffer,
            Some(ActivePreviewBuffer {
                layer_id: active_layer_id,
                stroke_session_id: active_stroke_session_id,
            }) if active_layer_id == layer_id && active_stroke_session_id == stroke_session_id
        ) {
            self.active_preview_buffer = None;
        }
        self.consumed_stroke_sessions.insert(stroke_session_id);
        self.mark_render_tree_dirty();
        Ok(MergeCommitSummary {
            revision: next_revision,
            layer_id,
            stroke_session_id,
            dirty_tiles: dirty_tiles.to_vec(),
            full_layer_dirty,
        })
    }

    pub fn leaf_tile_key_at(&self, layer_id: LayerId, tile_x: u32, tile_y: u32) -> Option<TileKey> {
        let layer_lookup = self
            .layer_tree
            .lookup_layer(layer_id)
            .unwrap_or_else(|| panic!("layer {layer_id} not found while querying tile key"));
        assert!(
            layer_lookup.is_leaf,
            "layer {layer_id} is not a leaf while querying tile key"
        );
        let image_handle = layer_lookup
            .image_handle
            .unwrap_or_else(|| panic!("leaf layer {layer_id} missing image handle"));
        let image = self
            .images
            .get(image_handle)
            .unwrap_or_else(|| panic!("image handle for layer {layer_id} not found"));
        if tile_x >= image.tiles_per_row() || tile_y >= image.tiles_per_column() {
            return None;
        }
        image.get_tile(tile_x, tile_y).unwrap_or_else(|error| {
            panic!(
                "virtual image query failed for layer {} at ({}, {}): {:?}",
                layer_id, tile_x, tile_y, error
            )
        })
    }

    pub fn layer_dirty_since(
        &self,
        layer_id: LayerId,
        since_version: u64,
    ) -> Option<DirtySinceResult> {
        let layer_lookup = self.layer_tree.lookup_layer(layer_id)?;
        if !layer_lookup.is_leaf {
            return None;
        }
        let _image_handle = layer_lookup.image_handle?;
        let tracked_version = self
            .layer_versions
            .get(&layer_id)
            .copied()
            .unwrap_or_else(|| panic!("layer version for {layer_id} is missing"));
        if document_perf_log_enabled() {
            eprintln!(
                "[document_perf] layer_dirty_since layer_id={} since_version={} tracked_version={}",
                layer_id, since_version, tracked_version
            );
        }
        let history = self
            .layer_dirty_history
            .get(&layer_id)
            .unwrap_or_else(|| panic!("layer dirty history for {layer_id} is missing"));
        let result = history.query(since_version, tracked_version);
        if document_perf_log_enabled() {
            match &result {
                DirtySinceResult::UpToDate => {
                    eprintln!(
                        "[document_perf] layer_dirty_since layer_id={} result=up_to_date",
                        layer_id
                    );
                }
                DirtySinceResult::HistoryTruncated => {
                    eprintln!(
                        "[document_perf] layer_dirty_since layer_id={} result=history_truncated",
                        layer_id
                    );
                }
                DirtySinceResult::HasChanges(query) => {
                    eprintln!(
                        "[document_perf] layer_dirty_since layer_id={} result=has_changes latest_version={} dirty_full={} dirty_count={}",
                        layer_id,
                        query.latest_version,
                        query.dirty_tiles.is_full(),
                        query.dirty_tiles.iter_dirty_tiles().count(),
                    );
                }
            }
        }
        Some(result)
    }

    pub fn layer_version(&self, layer_id: LayerId) -> Option<u64> {
        self.layer_versions.get(&layer_id).copied()
    }

    pub fn new_layer_root(&mut self) -> LayerNodeId {
        let (id, layer) = self.new_empty_leaf();
        self.root_mut().push(layer);
        self.mark_render_tree_dirty();
        id
    }

    pub fn new_layer_root_with_image(&mut self, image: TileImage, blend: BlendMode) -> LayerNodeId {
        let id = self.alloc_layer_id();
        self.layer_versions.insert(id.0, 0);
        self.layer_dirty_history.insert(
            id.0,
            LayerDirtyHistory::new(self.tiles_per_row(), self.tiles_per_column()),
        );
        let image_handle = self.images.insert(Arc::new(image));
        self.root_mut().push(LayerTreeNode::Leaf {
            id,
            blend,
            image_handle,
        });
        self.mark_render_tree_dirty();
        id
    }

    pub fn new_layer_above(&mut self, active_layer_id: LayerNodeId) -> LayerNodeId {
        let id = self.insert_new_empty_leaf_above(active_layer_id);
        self.mark_render_tree_dirty();
        id
    }

    pub fn group_layers(
        &mut self,
        first: LayerNodeId,
        second: LayerNodeId,
    ) -> Result<LayerNodeId, GroupError> {
        let next_layer_id = &mut self.next_layer_id;
        let mut alloc = || {
            let id = LayerNodeId(*next_layer_id);
            *next_layer_id = next_layer_id
                .checked_add(1)
                .expect("layer id space exhausted");
            id
        };

        let grouped = self.layer_tree.group_layers(first, second, &mut alloc)?;
        if grouped.is_some() {
            self.mark_render_tree_dirty();
        }
        grouped.ok_or(GroupError::NotSameLevel)
    }

    pub fn begin_merge(
        &mut self,
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
        expected_revision: u64,
    ) -> Result<(), DocumentMergeError> {
        if let Some(active_merge) = &self.active_merge {
            return Err(DocumentMergeError::ActiveMergeExists {
                active_layer_id: active_merge.layer_id,
                active_stroke_session_id: active_merge.stroke_session_id,
                requested_layer_id: layer_id,
                requested_stroke_session_id: stroke_session_id,
            });
        }
        if self.consumed_stroke_sessions.contains(&stroke_session_id) {
            return Err(DocumentMergeError::DuplicateFinalizeOrAbort {
                layer_id,
                stroke_session_id,
            });
        }
        if expected_revision != self.revision {
            return Err(DocumentMergeError::RevisionMismatch {
                expected_revision,
                actual_revision: self.revision,
                layer_id,
                stroke_session_id,
            });
        }
        let layer_lookup = self
            .lookup_leaf_image_handle(layer_id)
            .map_err(|error| error.with_stroke_session(stroke_session_id))?;
        if !layer_lookup.is_leaf {
            return Err(DocumentMergeError::LayerIsNotLeaf {
                layer_id,
                stroke_session_id,
            });
        }

        self.active_merge = Some(ActiveMerge {
            layer_id,
            stroke_session_id,
        });
        Ok(())
    }

    pub fn abort_merge(
        &mut self,
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
    ) -> Result<(), DocumentMergeError> {
        self.validate_active_merge(layer_id, stroke_session_id)?;
        self.active_merge = None;
        if matches!(
            self.active_preview_buffer,
            Some(ActivePreviewBuffer {
                layer_id: active_layer_id,
                stroke_session_id: active_stroke_session_id,
            }) if active_layer_id == layer_id && active_stroke_session_id == stroke_session_id
        ) {
            self.active_preview_buffer = None;
            self.mark_render_tree_dirty();
        }
        self.consumed_stroke_sessions.insert(stroke_session_id);
        Ok(())
    }

    pub fn has_active_merge(&self, layer_id: LayerId, stroke_session_id: StrokeSessionId) -> bool {
        matches!(
            self.active_merge,
            Some(ActiveMerge {
                layer_id: active_layer_id,
                stroke_session_id: active_stroke_session_id,
            }) if active_layer_id == layer_id && active_stroke_session_id == stroke_session_id
        )
    }

    pub fn take_render_tree_cache_dirty(&mut self) -> bool {
        let dirty = self.render_tree_cache_dirty;
        self.render_tree_cache_dirty = false;
        dirty
    }

    fn mark_render_tree_dirty(&mut self) {
        self.render_tree_cache_dirty = true;
        self.render_tree_revision = self
            .render_tree_revision
            .checked_add(1)
            .expect("render tree revision overflow");
    }

    fn alloc_layer_id(&mut self) -> LayerNodeId {
        let id = LayerNodeId(self.next_layer_id);
        self.next_layer_id = self
            .next_layer_id
            .checked_add(1)
            .expect("layer id space exhausted");
        id
    }

    fn new_empty_leaf(&mut self) -> (LayerNodeId, LayerTreeNode) {
        let id = self.alloc_layer_id();
        let image = TileImage::new(self.size_x, self.size_y)
            .unwrap_or_else(|error| panic!("failed to create empty layer image: {error:?}"));
        self.layer_versions.insert(id.0, 0);
        self.layer_dirty_history.insert(
            id.0,
            LayerDirtyHistory::new(self.tiles_per_row(), self.tiles_per_column()),
        );
        let image_handle = self.images.insert(Arc::new(image));
        (
            id,
            LayerTreeNode::Leaf {
                id,
                blend: BlendMode::Normal,
                image_handle,
            },
        )
    }

    fn insert_new_empty_leaf_above(&mut self, active_layer_id: LayerNodeId) -> LayerNodeId {
        let (id, layer) = self.new_empty_leaf();
        let remaining = self.layer_tree.insert_leaf_above(active_layer_id, layer);
        assert!(
            remaining.is_none(),
            "active layer not found: {:?}",
            active_layer_id
        );
        id
    }

    fn tiles_per_row(&self) -> u32 {
        self.size_x.div_ceil(TILE_SIZE)
    }

    fn tiles_per_column(&self) -> u32 {
        self.size_y.div_ceil(TILE_SIZE)
    }

    fn full_layer_dirty_tiles(&self) -> TileDirtyBitset {
        let mut dirty_tiles = TileDirtyBitset::new(self.tiles_per_row(), self.tiles_per_column())
            .unwrap_or_else(|error| panic!("create full layer dirty bitset failed: {error:?}"));
        for tile_y in 0..self.tiles_per_column() {
            for tile_x in 0..self.tiles_per_row() {
                dirty_tiles
                    .set(tile_x, tile_y)
                    .unwrap_or_else(|error| panic!("set full dirty tile failed: {error:?}"));
            }
        }
        dirty_tiles
    }

    fn build_dirty_tile_mask(
        &self,
        dirty_tiles: &[TileCoordinate],
        full_layer_dirty: bool,
    ) -> TileDirtyBitset {
        if full_layer_dirty {
            return self.full_layer_dirty_tiles();
        }
        let mut dirty_tile_mask =
            TileDirtyBitset::new(self.tiles_per_row(), self.tiles_per_column())
                .unwrap_or_else(|error| panic!("create layer dirty bitset failed: {error:?}"));
        for tile in dirty_tiles {
            dirty_tile_mask
                .set(tile.tile_x, tile.tile_y)
                .unwrap_or_else(|error| panic!("set layer dirty tile failed: {error:?}"));
        }
        dirty_tile_mask
    }

    fn record_layer_dirty_history(
        &mut self,
        layer_id: LayerId,
        version: u64,
        dirty_tiles: TileDirtyBitset,
    ) {
        let history = self
            .layer_dirty_history
            .get_mut(&layer_id)
            .unwrap_or_else(|| panic!("layer dirty history for {layer_id} is missing"));
        history.record(version, dirty_tiles);
    }

    fn lookup_leaf_image_handle(
        &self,
        layer_id: LayerId,
    ) -> Result<LayerLookupResult, DocumentMergeError> {
        let Some(layer_lookup) = self.layer_tree.lookup_layer(layer_id) else {
            return Err(DocumentMergeError::LayerNotFound { layer_id });
        };
        Ok(layer_lookup)
    }

    fn validate_active_merge(
        &self,
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
    ) -> Result<(), DocumentMergeError> {
        let active = self
            .active_merge
            .as_ref()
            .ok_or(DocumentMergeError::MissingActiveMerge {
                layer_id,
                stroke_session_id: Some(stroke_session_id),
            })?;
        if active.layer_id != layer_id || active.stroke_session_id != stroke_session_id {
            return Err(DocumentMergeError::MergeContextMismatch {
                expected_layer_id: active.layer_id,
                expected_stroke_session_id: active.stroke_session_id,
                actual_layer_id: layer_id,
                actual_stroke_session_id: stroke_session_id,
            });
        }
        Ok(())
    }
}

impl LayerTreeNode {
    fn replace_leaf_image_handle(&mut self, layer_id: LayerId, new_handle: ImageHandle) -> bool {
        match self {
            LayerTreeNode::Root { children } | LayerTreeNode::Branch { children, .. } => children
                .iter_mut()
                .any(|child| child.replace_leaf_image_handle(layer_id, new_handle)),
            LayerTreeNode::Leaf {
                id, image_handle, ..
            } => {
                if id.0 == layer_id {
                    *image_handle = new_handle;
                    true
                } else {
                    false
                }
            }
        }
    }

    fn lookup_layer(&self, layer_id: LayerId) -> Option<LayerLookupResult> {
        match self {
            LayerTreeNode::Root { children } => {
                for child in children {
                    if let Some(result) = child.lookup_layer(layer_id) {
                        return Some(result);
                    }
                }
                None
            }
            LayerTreeNode::Branch { id, children, .. } => {
                if id.0 == layer_id {
                    return Some(LayerLookupResult {
                        is_leaf: false,
                        image_handle: None,
                    });
                }
                for child in children {
                    if let Some(result) = child.lookup_layer(layer_id) {
                        return Some(result);
                    }
                }
                None
            }
            LayerTreeNode::Leaf {
                id, image_handle, ..
            } => {
                if id.0 == layer_id {
                    Some(LayerLookupResult {
                        is_leaf: true,
                        image_handle: Some(*image_handle),
                    })
                } else {
                    None
                }
            }
        }
    }

    fn id(&self) -> Option<LayerNodeId> {
        match self {
            LayerTreeNode::Branch { id, .. } | LayerTreeNode::Leaf { id, .. } => Some(*id),
            LayerTreeNode::Root { .. } => None,
        }
    }

    fn insert_leaf_above(
        &mut self,
        active_layer_id: LayerNodeId,
        mut new_layer: LayerTreeNode,
    ) -> Option<LayerTreeNode> {
        let children = match self {
            LayerTreeNode::Root { children } | LayerTreeNode::Branch { children, .. } => children,
            LayerTreeNode::Leaf { .. } => return Some(new_layer),
        };

        let mut index = 0;
        while index < children.len() {
            let is_active_layer = matches!(&children[index], LayerTreeNode::Leaf { id, .. } if *id == active_layer_id)
                || matches!(&children[index], LayerTreeNode::Branch { id, .. } if *id == active_layer_id);
            if is_active_layer {
                children.insert(index + 1, new_layer);
                return None;
            }

            if let Some(layer) = children[index].insert_leaf_above(active_layer_id, new_layer) {
                new_layer = layer;
                index += 1;
                continue;
            }

            return None;
        }

        Some(new_layer)
    }

    fn group_layers(
        &mut self,
        first: LayerNodeId,
        second: LayerNodeId,
        alloc_layer_id: &mut impl FnMut() -> LayerNodeId,
    ) -> Result<Option<LayerNodeId>, GroupError> {
        let children = match self {
            LayerTreeNode::Root { children } | LayerTreeNode::Branch { children, .. } => children,
            LayerTreeNode::Leaf { .. } => return Ok(None),
        };

        let mut first_index = None;
        let mut second_index = None;
        for (index, child) in children.iter().enumerate() {
            let Some(id) = child.id() else { continue };
            if id == first {
                first_index = Some(index);
            }
            if id == second {
                second_index = Some(index);
            }
        }

        match (first_index, second_index) {
            (Some(first_index), Some(second_index)) => {
                let start = first_index.min(second_index);
                let end = first_index.max(second_index);
                let branch_id = alloc_layer_id();

                let branch_children: Vec<LayerTreeNode> = children.drain(start..=end).collect();
                children.insert(
                    start,
                    LayerTreeNode::Branch {
                        id: branch_id,
                        blend: BlendMode::Normal,
                        children: branch_children,
                    },
                );
                Ok(Some(branch_id))
            }
            (Some(_), None) | (None, Some(_)) => Err(GroupError::NotSameLevel),
            (None, None) => {
                for child in children.iter_mut() {
                    if let Some(id) = child.group_layers(first, second, alloc_layer_id)? {
                        return Ok(Some(id));
                    }
                }
                Ok(None)
            }
        }
    }

    fn build_render_node_snapshot(
        &self,
        active_preview_buffer: Option<ActivePreviewBuffer>,
    ) -> RenderNodeSnapshot {
        match self {
            LayerTreeNode::Root { children } => RenderNodeSnapshot::Group {
                group_id: LayerNodeId::ROOT.0,
                blend: BlendMode::Normal,
                children: children
                    .iter()
                    .map(|child| child.build_render_node_snapshot(active_preview_buffer))
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
                    .into(),
            },
            LayerTreeNode::Branch {
                id,
                blend,
                children,
            } => RenderNodeSnapshot::Group {
                group_id: id.0,
                blend: *blend,
                children: children
                    .iter()
                    .map(|child| child.build_render_node_snapshot(active_preview_buffer))
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
                    .into(),
            },
            LayerTreeNode::Leaf {
                id,
                blend,
                image_handle,
            } => {
                // Always model layers as a group node so that live preview can attach brush buffer
                // leaves without changing the existence/type of the layer node itself.
                let base_layer_leaf = RenderNodeSnapshot::Leaf {
                    layer_id: id.0,
                    blend: BlendMode::Normal,
                    image_source: render_protocol::ImageSource::LayerImage {
                        image_handle: *image_handle,
                    },
                };
                let mut children = vec![base_layer_leaf];
                if let Some(preview_buffer) = active_preview_buffer {
                    if preview_buffer.layer_id == id.0 {
                        children.push(RenderNodeSnapshot::Leaf {
                            layer_id: id.0,
                            blend: BlendMode::Normal,
                            image_source: render_protocol::ImageSource::BrushBuffer {
                                stroke_session_id: preview_buffer.stroke_session_id,
                            },
                        });
                    }
                }
                RenderNodeSnapshot::Group {
                    group_id: id.0,
                    blend: *blend,
                    children: children.into_boxed_slice().into(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_signature(document: &Document) -> String {
        render_node_signature(document.render_tree_snapshot().root.as_ref())
    }

    fn render_node_signature(node: &RenderNodeSnapshot) -> String {
        match node {
            RenderNodeSnapshot::Leaf {
                layer_id, blend, ..
            } => {
                format!("L({layer_id}:{blend:?})")
            }
            RenderNodeSnapshot::Group {
                group_id,
                blend,
                children,
            } => {
                let children_repr = children
                    .iter()
                    .map(render_node_signature)
                    .collect::<Vec<_>>()
                    .join(",");
                format!("G({group_id}:{blend:?})[{children_repr}]")
            }
        }
    }

    fn contains_brush_buffer_leaf(node: &RenderNodeSnapshot) -> bool {
        match node {
            RenderNodeSnapshot::Leaf { image_source, .. } => {
                matches!(
                    image_source,
                    render_protocol::ImageSource::BrushBuffer { .. }
                )
            }
            RenderNodeSnapshot::Group { children, .. } => {
                children.iter().any(contains_brush_buffer_leaf)
            }
        }
    }

    fn leaf_id(node: &LayerTreeNode) -> LayerNodeId {
        match node {
            LayerTreeNode::Leaf { id, .. } => *id,
            _ => panic!("expected leaf"),
        }
    }

    fn branch_id(node: &LayerTreeNode) -> LayerNodeId {
        match node {
            LayerTreeNode::Branch { id, .. } => *id,
            _ => panic!("expected branch"),
        }
    }

    fn branch_blend(node: &LayerTreeNode) -> BlendMode {
        match node {
            LayerTreeNode::Branch { blend, .. } => *blend,
            _ => panic!("expected branch"),
        }
    }

    fn branch_children(node: &LayerTreeNode) -> &[LayerTreeNode] {
        match node {
            LayerTreeNode::Branch { children, .. } => children,
            _ => panic!("expected branch"),
        }
    }

    fn set_leaf_blend(node: &mut LayerTreeNode, target: LayerNodeId, blend: BlendMode) -> bool {
        match node {
            LayerTreeNode::Leaf {
                id,
                blend: leaf_blend,
                ..
            } if *id == target => {
                *leaf_blend = blend;
                true
            }
            LayerTreeNode::Root { children } | LayerTreeNode::Branch { children, .. } => {
                for child in children {
                    if set_leaf_blend(child, target, blend) {
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    fn set_branch_blend(node: &mut LayerTreeNode, target: LayerNodeId, blend: BlendMode) -> bool {
        match node {
            LayerTreeNode::Branch {
                id,
                blend: branch_blend,
                ..
            } if *id == target => {
                *branch_blend = blend;
                true
            }
            LayerTreeNode::Root { children } | LayerTreeNode::Branch { children, .. } => {
                for child in children {
                    if set_branch_blend(child, target, blend) {
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    #[test]
    fn new_document_has_empty_root_and_single_root_group_step() {
        let document = Document::new(64, 32);
        assert_eq!(document.root().len(), 0);
        assert_eq!(snapshot_signature(&document), "G(0:Normal)[]");
    }

    #[test]
    fn new_layer_root_appends_leaf_with_default_properties() {
        let mut document = Document::new(17, 23);
        let id = document.new_layer_root();
        assert_ne!(id, LayerNodeId::ROOT);
        assert_eq!(document.root().len(), 1);

        let node = &document.root()[0];
        match node {
            LayerTreeNode::Leaf {
                id: leaf_id,
                blend,
                image_handle,
            } => {
                assert_eq!(*leaf_id, id);
                assert_eq!(*blend, BlendMode::Normal);
                let image = document
                    .image(*image_handle)
                    .expect("image handle should resolve");
                assert_eq!(image.size_x(), 17);
                assert_eq!(image.size_y(), 23);
            }
            _ => panic!("expected root child to be leaf"),
        }
    }

    #[test]
    fn new_layer_root_with_image_uses_provided_image_and_blend() {
        let mut document = Document::new(17, 23);
        let image = TileImage::new(9, 11).expect("new image");
        let id = document.new_layer_root_with_image(image, BlendMode::Multiply);

        let node = &document.root()[0];
        match node {
            LayerTreeNode::Leaf {
                id: leaf_id,
                blend,
                image_handle,
            } => {
                assert_eq!(*leaf_id, id);
                assert_eq!(*blend, BlendMode::Multiply);
                let image = document
                    .image(*image_handle)
                    .expect("image handle should resolve");
                assert_eq!(image.size_x(), 9);
                assert_eq!(image.size_y(), 11);
            }
            _ => panic!("expected root child to be leaf"),
        }
    }

    #[test]
    fn new_layer_above_inserts_next_to_active_leaf_in_same_parent() {
        let mut document = Document::new(10, 10);
        let a = document.new_layer_root();
        let b = document.new_layer_root();
        let c = document.new_layer_root();

        let branch = document.group_layers(a, b).expect("group must succeed");

        let inserted = document.new_layer_above(a);

        assert_eq!(document.root().len(), 2);
        let branch_node = &document.root()[0];
        assert_eq!(branch_id(branch_node), branch);
        let children = branch_children(branch_node);
        assert_eq!(children.len(), 3);
        assert_eq!(leaf_id(&children[0]), a);
        assert_eq!(leaf_id(&children[1]), inserted);
        assert_eq!(leaf_id(&children[2]), b);

        assert!(matches!(&document.root()[1], LayerTreeNode::Leaf { id, .. } if *id == c));
    }

    #[test]
    fn new_layer_above_inserts_next_to_active_branch_in_parent() {
        let mut document = Document::new(10, 10);
        let a = document.new_layer_root();
        let b = document.new_layer_root();
        let c = document.new_layer_root();

        let branch = document.group_layers(a, b).expect("group must succeed");
        let inserted = document.new_layer_above(branch);

        assert_eq!(document.root().len(), 3);
        assert!(matches!(&document.root()[0], LayerTreeNode::Branch { id, .. } if *id == branch));
        assert!(matches!(&document.root()[1], LayerTreeNode::Leaf { id, .. } if *id == inserted));
        assert!(matches!(&document.root()[2], LayerTreeNode::Leaf { id, .. } if *id == c));
    }

    #[test]
    #[should_panic(expected = "active layer not found")]
    fn new_layer_above_panics_if_active_not_found() {
        let mut document = Document::new(10, 10);
        let _ = document.new_layer_above(LayerNodeId(123));
    }

    #[test]
    fn group_layers_wraps_range_in_new_branch_and_preserves_order() {
        let mut document = Document::new(1, 1);
        let a = document.new_layer_root();
        let b = document.new_layer_root();
        let c = document.new_layer_root();
        let d = document.new_layer_root();

        let branch = document.group_layers(b, d).expect("group must succeed");

        assert_eq!(document.root().len(), 2);
        assert!(matches!(&document.root()[0], LayerTreeNode::Leaf { id, .. } if *id == a));

        let branch_node = &document.root()[1];
        assert_eq!(branch_id(branch_node), branch);
        let children = branch_children(branch_node);
        assert_eq!(children.len(), 3);
        assert_eq!(leaf_id(&children[0]), b);
        assert_eq!(leaf_id(&children[1]), c);
        assert_eq!(leaf_id(&children[2]), d);
        assert_eq!(branch_blend(branch_node), BlendMode::Normal);
    }

    #[test]
    fn group_layers_allows_same_id_and_wraps_single_node() {
        let mut document = Document::new(1, 1);
        let a = document.new_layer_root();
        let b = document.new_layer_root();

        let branch = document.group_layers(a, a).expect("group must succeed");
        assert_eq!(document.root().len(), 2);

        let branch_node = &document.root()[0];
        assert_eq!(branch_id(branch_node), branch);
        let children = branch_children(branch_node);
        assert_eq!(children.len(), 1);
        assert_eq!(leaf_id(&children[0]), a);

        assert!(matches!(&document.root()[1], LayerTreeNode::Leaf { id, .. } if *id == b));
    }

    #[test]
    fn group_layers_returns_err_if_not_same_level() {
        let mut document = Document::new(1, 1);
        let a = document.new_layer_root();
        let b = document.new_layer_root();
        let c = document.new_layer_root();
        let _branch = document.group_layers(a, b).expect("group must succeed");

        let error = document
            .group_layers(a, c)
            .expect_err("must be different level");
        assert_eq!(error, GroupError::NotSameLevel);
    }

    #[test]
    fn render_tree_snapshot_preserves_group_ids_and_child_order() {
        let mut document = Document::new(1, 1);
        let a = document.new_layer_root();
        let b = document.new_layer_root();
        let c = document.new_layer_root();
        let branch = document.group_layers(a, b).expect("group must succeed");

        assert_eq!(
            snapshot_signature(&document),
            format!(
                "G(0:Normal)[G({}:Normal)[G({}:Normal)[L({}:Normal)],G({}:Normal)[L({}:Normal)]],G({}:Normal)[L({}:Normal)]]",
                branch.0, a.0, a.0, b.0, b.0, c.0, c.0
            )
        );
    }

    #[test]
    fn render_tree_snapshot_leaf_includes_blend_mode() {
        let mut document = Document::new(1, 1);
        let a = document.new_layer_root();
        assert!(set_leaf_blend(
            &mut document.layer_tree,
            a,
            BlendMode::Multiply
        ));

        assert_eq!(
            snapshot_signature(&document),
            format!("G(0:Normal)[G({}:Multiply)[L({}:Normal)]]", a.0, a.0)
        );
    }

    #[test]
    fn render_tree_snapshot_group_includes_branch_blend_mode() {
        let mut document = Document::new(1, 1);
        let a = document.new_layer_root();
        let b = document.new_layer_root();
        let branch = document.group_layers(a, b).expect("group must succeed");

        assert!(set_branch_blend(
            &mut document.layer_tree,
            branch,
            BlendMode::Multiply
        ));

        assert_eq!(
            snapshot_signature(&document),
            format!(
                "G(0:Normal)[G({}:Multiply)[G({}:Normal)[L({}:Normal)],G({}:Normal)[L({}:Normal)]]]",
                branch.0, a.0, a.0, b.0, b.0
            )
        );
    }

    #[test]
    fn render_tree_snapshot_leaf_image_handle_resolves() {
        let mut document = Document::new(8, 4);
        let _ = document.new_layer_root();

        let snapshot = document.render_tree_snapshot();
        let image_handle = match snapshot.root.as_ref() {
            RenderNodeSnapshot::Group { children, .. } => match children.first() {
                Some(RenderNodeSnapshot::Group { children, .. }) => match children.first() {
                    Some(RenderNodeSnapshot::Leaf { image_source, .. }) => match image_source {
                        render_protocol::ImageSource::LayerImage { image_handle } => *image_handle,
                        render_protocol::ImageSource::BrushBuffer { .. } => {
                            panic!("document snapshot must not contain brush buffer image source")
                        }
                    },
                    _ => panic!("snapshot should contain one leaf"),
                },
                _ => panic!("snapshot should contain one layer group"),
            },
            RenderNodeSnapshot::Leaf { .. } => panic!("snapshot root must be a group"),
        };

        let image = document
            .image(image_handle)
            .expect("leaf image handle should resolve");
        assert_eq!(image.size_x(), 8);
        assert_eq!(image.size_y(), 4);
    }

    #[test]
    fn render_tree_snapshot_wraps_leaf_with_brush_preview_group() {
        let mut document = Document::new(8, 4);
        let layer_id = document.new_layer_root().0;
        document
            .set_active_preview_buffer(layer_id, 42)
            .expect("set active preview buffer");

        let snapshot = document.render_tree_snapshot();
        match snapshot.root.as_ref() {
            RenderNodeSnapshot::Group { children, .. } => match children.first() {
                Some(RenderNodeSnapshot::Group {
                    blend, children, ..
                }) => {
                    assert_eq!(*blend, BlendMode::Normal);
                    assert_eq!(children.len(), 2);
                    match &children[0] {
                        RenderNodeSnapshot::Leaf {
                            image_source: render_protocol::ImageSource::LayerImage { .. },
                            ..
                        } => {}
                        _ => panic!("preview group first child must be layer image leaf"),
                    }
                    match &children[1] {
                        RenderNodeSnapshot::Leaf {
                            image_source:
                                render_protocol::ImageSource::BrushBuffer { stroke_session_id },
                            ..
                        } => assert_eq!(*stroke_session_id, 42),
                        _ => panic!("preview group second child must be brush buffer leaf"),
                    }
                }
                _ => panic!("snapshot root first child must be wrapped preview group"),
            },
            RenderNodeSnapshot::Leaf { .. } => panic!("snapshot root must be a group"),
        }
    }

    #[test]
    fn clear_active_preview_buffer_restores_plain_leaf() {
        let mut document = Document::new(8, 4);
        let layer_id = document.new_layer_root().0;
        document
            .set_active_preview_buffer(layer_id, 77)
            .expect("set active preview buffer");
        let _ = document.take_render_tree_cache_dirty();

        assert!(document.clear_active_preview_buffer(77));
        assert!(document.take_render_tree_cache_dirty());

        let snapshot = document.render_tree_snapshot();
        match snapshot.root.as_ref() {
            RenderNodeSnapshot::Group { children, .. } => match children.first() {
                Some(RenderNodeSnapshot::Group { children, .. }) => {
                    assert_eq!(children.len(), 1);
                    match children.first() {
                        Some(RenderNodeSnapshot::Leaf {
                            image_source: render_protocol::ImageSource::LayerImage { .. },
                            ..
                        }) => {}
                        _ => panic!("cleared preview must restore plain layer leaf"),
                    }
                }
                _ => panic!("cleared preview must restore layer group"),
            },
            RenderNodeSnapshot::Leaf { .. } => panic!("snapshot root must be a group"),
        }
    }

    #[test]
    fn preview_buffer_bumps_render_tree_revision() {
        let mut document = Document::new(8, 4);
        let layer_id = document.new_layer_root().0;

        let rev0 = document.render_tree_revision();
        document
            .set_active_preview_buffer(layer_id, 42)
            .expect("set active preview buffer");
        let rev1 = document.render_tree_revision();
        assert!(
            rev1 > rev0,
            "render tree revision must advance when preview buffer changes"
        );

        let _ = document.take_render_tree_cache_dirty();
        assert!(document.clear_active_preview_buffer(42));
        let rev2 = document.render_tree_revision();
        assert!(
            rev2 > rev1,
            "render tree revision must advance when preview buffer is cleared"
        );
    }

    fn test_tile_key(raw: u64) -> TileKey {
        tiles::test_tile_key(raw)
    }

    fn first_leaf_image_handle(document: &Document) -> ImageHandle {
        match &document.root()[0] {
            LayerTreeNode::Leaf { image_handle, .. } => *image_handle,
            _ => panic!("expected first root node to be a leaf"),
        }
    }

    #[test]
    fn merge_apply_replaces_tile_key_and_advances_revision() {
        let mut document = Document::new(128, 128);
        let layer_id = document.new_layer_root();
        document
            .begin_merge(layer_id.0, 77, document.revision())
            .expect("begin merge");
        let image_handle = document
            .leaf_image_handle(layer_id.0, 77)
            .expect("leaf image handle should resolve");
        let existing_image = document.image(image_handle).expect("resolve image");
        let mut updated_image = (*existing_image).clone();
        apply_tile_key_mappings(
            &mut updated_image,
            &[TileKeyMapping {
                tile_x: 0,
                tile_y: 0,
                layer_id: layer_id.0,
                previous_key: None,
                new_key: test_tile_key(10),
            }],
        )
        .expect("apply tile key mappings");
        let summary = document
            .apply_merge_image(
                layer_id.0,
                77,
                updated_image,
                &[TileCoordinate {
                    tile_x: 0,
                    tile_y: 0,
                }],
                false,
            )
            .expect("apply merge image");

        assert_eq!(summary.revision, 1);
        assert_eq!(summary.layer_id, layer_id.0);
        assert_eq!(
            summary.dirty_tiles,
            vec![TileCoordinate {
                tile_x: 0,
                tile_y: 0
            }]
        );
        let image = document
            .image(first_leaf_image_handle(&document))
            .expect("resolve image");
        let key = image
            .get_tile(0, 0)
            .expect("read tile")
            .expect("tile should be assigned");
        assert_eq!(key, test_tile_key(10));
    }

    #[test]
    fn merge_apply_clears_active_preview_buffer_for_same_stroke() {
        let mut document = Document::new(128, 128);
        let layer_id = document.new_layer_root();
        let image_handle = document
            .leaf_image_handle(layer_id.0, 900)
            .expect("resolve image handle");
        let existing_image = document
            .image(image_handle)
            .expect("resolve image for merge apply");
        let updated_image = (*existing_image).clone();

        document
            .set_active_preview_buffer(layer_id.0, 900)
            .expect("set active preview buffer");
        assert!(contains_brush_buffer_leaf(
            document.render_tree_snapshot().root.as_ref()
        ));

        document
            .begin_merge(layer_id.0, 900, document.revision())
            .expect("begin merge");
        document
            .apply_merge_image(layer_id.0, 900, updated_image, &[], false)
            .expect("apply merge");

        assert!(!contains_brush_buffer_leaf(
            document.render_tree_snapshot().root.as_ref()
        ));
    }

    #[test]
    fn merge_apply_succeeds_when_querying_dirty_since_previous_layer_version() {
        let mut document = Document::new(128, 128);
        let layer_id = document.new_layer_root();
        document
            .begin_merge(layer_id.0, 121, document.revision())
            .expect("begin merge");
        let image_handle = document
            .leaf_image_handle(layer_id.0, 121)
            .expect("leaf image handle should resolve");
        let existing_image = document.image(image_handle).expect("resolve image");
        let mut updated_image = (*existing_image).clone();
        apply_tile_key_mappings(
            &mut updated_image,
            &[TileKeyMapping {
                tile_x: 0,
                tile_y: 0,
                layer_id: layer_id.0,
                previous_key: None,
                new_key: test_tile_key(11),
            }],
        )
        .expect("apply tile key mappings");

        let summary = document
            .apply_merge_image(
                layer_id.0,
                121,
                updated_image,
                &[TileCoordinate {
                    tile_x: 0,
                    tile_y: 0,
                }],
                false,
            )
            .expect("merge apply should not panic or fail");
        assert_eq!(summary.layer_id, layer_id.0);
    }

    #[test]
    fn layer_dirty_since_uses_layer_history_after_image_handle_swap() {
        let mut document = Document::new(512, 256);
        let layer_id = document.new_layer_root();
        document
            .begin_merge(layer_id.0, 122, document.revision())
            .expect("begin merge");
        let image_handle = document
            .leaf_image_handle(layer_id.0, 122)
            .expect("leaf image handle should resolve");
        let existing_image = document.image(image_handle).expect("resolve image");
        let mut updated_image = (*existing_image).clone();
        apply_tile_key_mappings(
            &mut updated_image,
            &[TileKeyMapping {
                tile_x: 0,
                tile_y: 0,
                layer_id: layer_id.0,
                previous_key: None,
                new_key: test_tile_key(12),
            }],
        )
        .expect("apply tile key mappings");
        document
            .apply_merge_image(
                layer_id.0,
                122,
                updated_image,
                &[TileCoordinate {
                    tile_x: 3,
                    tile_y: 1,
                }],
                false,
            )
            .expect("merge apply should succeed");

        let result = document
            .layer_dirty_since(layer_id.0, 0)
            .expect("layer dirty query should resolve");
        let DirtySinceResult::HasChanges(query) = result else {
            panic!("expected dirty query result");
        };
        let dirty_tiles = query.dirty_tiles.iter_dirty_tiles().collect::<Vec<_>>();
        assert_eq!(dirty_tiles, vec![(3, 1)]);
    }

    #[test]
    fn layer_version_advances_once_per_merge_commit() {
        let mut document = Document::new(512, 256);
        let layer_id = document.new_layer_root();
        assert_eq!(document.layer_version(layer_id.0), Some(0));

        for stroke_session_id in [201u64, 202u64] {
            document
                .begin_merge(layer_id.0, stroke_session_id, document.revision())
                .expect("begin merge");
            let image_handle = document
                .leaf_image_handle(layer_id.0, stroke_session_id)
                .expect("leaf image handle should resolve");
            let existing_image = document.image(image_handle).expect("resolve image");
            let mut updated_image = (*existing_image).clone();
            apply_tile_key_mappings(
                &mut updated_image,
                &[
                    TileKeyMapping {
                        tile_x: 0,
                        tile_y: 0,
                        layer_id: layer_id.0,
                        previous_key: existing_image.get_tile(0, 0).expect("read tile"),
                        new_key: test_tile_key(stroke_session_id + 1),
                    },
                    TileKeyMapping {
                        tile_x: 1,
                        tile_y: 0,
                        layer_id: layer_id.0,
                        previous_key: existing_image.get_tile(1, 0).expect("read tile"),
                        new_key: test_tile_key(stroke_session_id + 2),
                    },
                ],
            )
            .expect("apply tile key mappings");
            document
                .apply_merge_image(
                    layer_id.0,
                    stroke_session_id,
                    updated_image,
                    &[
                        TileCoordinate {
                            tile_x: 0,
                            tile_y: 0,
                        },
                        TileCoordinate {
                            tile_x: 1,
                            tile_y: 0,
                        },
                    ],
                    false,
                )
                .expect("apply merge image");
        }

        assert_eq!(document.layer_version(layer_id.0), Some(2));
    }

    #[test]
    fn layer_dirty_since_returns_empty_changes_when_version_advanced_without_dirty_tiles() {
        let mut document = Document::new(512, 256);
        let layer_id = document.new_layer_root();

        document
            .begin_merge(layer_id.0, 301, document.revision())
            .expect("begin merge");
        let image_handle = document
            .leaf_image_handle(layer_id.0, 301)
            .expect("leaf image handle should resolve");
        let existing_image = document.image(image_handle).expect("resolve image");
        let updated_image = (*existing_image).clone();
        document
            .apply_merge_image(layer_id.0, 301, updated_image, &[], false)
            .expect("apply merge image with empty dirty tiles");

        let result = document
            .layer_dirty_since(layer_id.0, 0)
            .expect("layer dirty query should resolve");
        let DirtySinceResult::HasChanges(query) = result else {
            panic!("expected HasChanges with empty dirty tiles");
        };
        assert_eq!(query.latest_version, 1);
        assert!(query.dirty_tiles.is_empty());
    }

    #[test]
    fn merge_abort_clears_active_merge() {
        let mut document = Document::new(128, 128);
        let layer_id = document.new_layer_root();
        document
            .begin_merge(layer_id.0, 120, document.revision())
            .expect("begin merge");
        assert!(document.has_active_merge(layer_id.0, 120));
        document
            .abort_merge(layer_id.0, 120)
            .expect("explicit abort after begin merge");
        assert!(!document.has_active_merge(layer_id.0, 120));
    }

    #[test]
    fn merge_abort_clears_active_preview_buffer_for_same_stroke() {
        let mut document = Document::new(128, 128);
        let layer_id = document.new_layer_root();
        document
            .set_active_preview_buffer(layer_id.0, 333)
            .expect("set active preview buffer");
        assert!(contains_brush_buffer_leaf(
            document.render_tree_snapshot().root.as_ref()
        ));

        document
            .begin_merge(layer_id.0, 333, document.revision())
            .expect("begin merge");
        document.abort_merge(layer_id.0, 333).expect("abort merge");

        assert!(!contains_brush_buffer_leaf(
            document.render_tree_snapshot().root.as_ref()
        ));
    }
}
