use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use render_protocol::{
    BlendMode, ImageHandle, LayerId, RenderNodeSnapshot, RenderTreeSnapshot, StrokeSessionId,
};
use slotmap::SlotMap;
use tiles::{TileImage, TileKey, VirtualImageError};

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
    next_layer_id: u64,
    render_tree_cache_dirty: bool,
    dirty_layers: HashSet<LayerId>,
    active_merge: Option<DocumentMergeContext>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TileReplacement {
    pub tile_x: u32,
    pub tile_y: u32,
    pub new_key: TileKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeDirtyHint {
    None,
    FullLayer,
    Tiles(Vec<TileCoordinate>),
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
pub(crate) struct TileCommitEntry {
    pub new_key: TileKey,
    pub previous_key: Option<TileKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentMergeError {
    LayerNotFound {
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
    TileOutOfBounds {
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
        tile_x: u32,
        tile_y: u32,
    },
    DuplicateTileReplacement {
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
        tile_x: u32,
        tile_y: u32,
    },
    NoTileReplacements {
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
    },
    KeyConflict {
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
        tile_x: u32,
        tile_y: u32,
        new_key: TileKey,
        conflict_tile_x: u32,
        conflict_tile_y: u32,
    },
    RevisionOverflow {
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
    },
    VirtualImage {
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
        tile_x: u32,
        tile_y: u32,
        source: VirtualImageError,
    },
}

#[derive(Debug)]
struct DocumentMergeContext {
    layer_id: LayerId,
    stroke_session_id: StrokeSessionId,
    replacements_by_tile: HashMap<TileCoordinate, TileCommitEntry>,
    replacement_key_owner: HashMap<TileKey, TileCoordinate>,
}

#[derive(Debug, Clone, Copy)]
struct LayerLookupResult {
    is_leaf: bool,
    image_handle: Option<ImageHandle>,
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
            next_layer_id: 1,
            render_tree_cache_dirty: true,
            dirty_layers: HashSet::new(),
            active_merge: None,
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

    pub fn render_tree_snapshot(&self, revision: u64) -> RenderTreeSnapshot {
        RenderTreeSnapshot {
            revision,
            root: Arc::new(self.layer_tree.build_render_node_snapshot()),
        }
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
        let layer_lookup = self.lookup_leaf_image_handle(layer_id, stroke_session_id)?;
        if !layer_lookup.is_leaf {
            return Err(DocumentMergeError::LayerIsNotLeaf {
                layer_id,
                stroke_session_id,
            });
        }
        layer_lookup
            .image_handle
            .ok_or(DocumentMergeError::LayerNotFound {
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
        if let Some(image_entry) = self.images.get_mut(image_handle) {
            *image_entry = Arc::new(image);
            return Ok(());
        }
        Err(DocumentMergeError::LayerNotFound {
            layer_id,
            stroke_session_id,
        })
    }

    pub fn apply_merge_image(
        &mut self,
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
        image: TileImage,
        dirty_tiles: Vec<TileCoordinate>,
        full_layer_dirty: bool,
    ) -> Result<MergeCommitSummary, DocumentMergeError> {
        self.validate_active_merge(layer_id, stroke_session_id)?;
        let image_handle = self.leaf_image_handle(layer_id, stroke_session_id)?;
        let new_image_handle = self.images.insert(Arc::new(image));
        let replaced = self
            .layer_tree
            .replace_leaf_image_handle(layer_id, new_image_handle);
        if !replaced {
            return Err(DocumentMergeError::LayerNotFound {
                layer_id,
                stroke_session_id,
            });
        }
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
        self.consumed_stroke_sessions.insert(stroke_session_id);
        self.dirty_layers.insert(layer_id);
        self.mark_render_tree_dirty();
        Ok(MergeCommitSummary {
            revision: next_revision,
            layer_id,
            stroke_session_id,
            dirty_tiles,
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
        image
            .get_tile(tile_x, tile_y)
            .unwrap_or_else(|error| {
                panic!(
                    "virtual image query failed for layer {} at ({}, {}): {:?}",
                    layer_id, tile_x, tile_y, error
                )
            })
    }

    pub fn new_layer_root(&mut self) -> LayerNodeId {
        let (id, layer) = self.new_empty_leaf();
        self.root_mut().push(layer);
        self.mark_render_tree_dirty();
        id
    }

    pub fn new_layer_root_with_image(
        &mut self,
        image: TileImage,
        blend: BlendMode,
    ) -> LayerNodeId {
        let id = self.alloc_layer_id();
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
        let layer_lookup = self.lookup_leaf_image_handle(layer_id, stroke_session_id)?;
        if !layer_lookup.is_leaf {
            return Err(DocumentMergeError::LayerIsNotLeaf {
                layer_id,
                stroke_session_id,
            });
        }

        self.active_merge = Some(DocumentMergeContext {
            layer_id,
            stroke_session_id,
            replacements_by_tile: HashMap::new(),
            replacement_key_owner: HashMap::new(),
        });
        Ok(())
    }

    pub(crate) fn commit_tile_replacements(
        &mut self,
        layer_id: LayerId,
        replacements: &[TileReplacement],
    ) -> Result<(), DocumentMergeError> {
        let (stroke_session_id, image_handle) = {
            let active =
                self.active_merge
                    .as_ref()
                    .ok_or(DocumentMergeError::MissingActiveMerge {
                        layer_id,
                        stroke_session_id: None,
                    })?;
            if active.layer_id != layer_id {
                return Err(DocumentMergeError::MergeContextMismatch {
                    expected_layer_id: active.layer_id,
                    expected_stroke_session_id: active.stroke_session_id,
                    actual_layer_id: layer_id,
                    actual_stroke_session_id: active.stroke_session_id,
                });
            }
            let layer_lookup = self.lookup_leaf_image_handle(layer_id, active.stroke_session_id)?;
            if !layer_lookup.is_leaf {
                return Err(DocumentMergeError::LayerIsNotLeaf {
                    layer_id,
                    stroke_session_id: active.stroke_session_id,
                });
            }
            let image_handle =
                layer_lookup
                    .image_handle
                    .ok_or(DocumentMergeError::LayerNotFound {
                        layer_id,
                        stroke_session_id: active.stroke_session_id,
                    })?;
            (active.stroke_session_id, image_handle)
        };

        let image =
            self.images
                .get(image_handle)
                .cloned()
                .ok_or(DocumentMergeError::LayerNotFound {
                    layer_id,
                    stroke_session_id,
                })?;
        let active = self
            .active_merge
            .as_ref()
            .ok_or(DocumentMergeError::MissingActiveMerge {
                layer_id,
                stroke_session_id: Some(stroke_session_id),
            })?;
        let mut replacements_by_tile = active.replacements_by_tile.clone();
        let mut replacement_key_owner = active.replacement_key_owner.clone();
        for replacement in replacements {
            self.validate_replacement(layer_id, stroke_session_id, &image, replacement)?;
            self.validate_and_register_replacement(
                layer_id,
                stroke_session_id,
                &image,
                replacement,
                &mut replacements_by_tile,
                &mut replacement_key_owner,
            )?;
        }
        if let Some(active_mut) = self.active_merge.as_mut() {
            active_mut.replacements_by_tile = replacements_by_tile;
            active_mut.replacement_key_owner = replacement_key_owner;
        }
        Ok(())
    }

    pub(crate) fn finalize_merge(
        &mut self,
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
        dirty_hint: MergeDirtyHint,
    ) -> Result<MergeCommitSummary, DocumentMergeError> {
        self.validate_active_merge(layer_id, stroke_session_id)?;
        let active = self
            .active_merge
            .as_ref()
            .ok_or(DocumentMergeError::MissingActiveMerge {
                layer_id,
                stroke_session_id: Some(stroke_session_id),
            })?;
        if active.replacements_by_tile.is_empty() {
            return Err(DocumentMergeError::NoTileReplacements {
                layer_id,
                stroke_session_id,
            });
        }
        let staged_replacements = active.replacements_by_tile.clone();
        let next_revision =
            self.revision
                .checked_add(1)
                .ok_or(DocumentMergeError::RevisionOverflow {
                    layer_id,
                    stroke_session_id,
                })?;
        self.revision = next_revision;
        self.active_merge = None;
        self.consumed_stroke_sessions.insert(stroke_session_id);
        self.dirty_layers.insert(layer_id);
        self.mark_render_tree_dirty();

        let mut dirty_tiles: Vec<TileCoordinate> = staged_replacements.keys().copied().collect();
        let mut full_layer_dirty = false;
        match dirty_hint {
            MergeDirtyHint::None => {}
            MergeDirtyHint::FullLayer => {
                full_layer_dirty = true;
            }
            MergeDirtyHint::Tiles(tiles) => {
                for tile in tiles {
                    if !dirty_tiles.contains(&tile) {
                        dirty_tiles.push(tile);
                    }
                }
            }
        }

        Ok(MergeCommitSummary {
            revision: self.revision,
            layer_id,
            stroke_session_id,
            dirty_tiles,
            full_layer_dirty,
        })
    }

    pub fn abort_merge(
        &mut self,
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
    ) -> Result<(), DocumentMergeError> {
        self.validate_active_merge(layer_id, stroke_session_id)?;
        self.active_merge = None;
        self.consumed_stroke_sessions.insert(stroke_session_id);
        Ok(())
    }

    pub fn has_active_merge(&self, layer_id: LayerId, stroke_session_id: StrokeSessionId) -> bool {
        matches!(
            self.active_merge,
            Some(DocumentMergeContext {
                layer_id: active_layer_id,
                stroke_session_id: active_stroke_session_id,
                ..
            }) if active_layer_id == layer_id && active_stroke_session_id == stroke_session_id
        )
    }

    pub fn take_dirty_layers(&mut self) -> Vec<LayerId> {
        self.dirty_layers.drain().collect()
    }

    pub fn take_render_tree_cache_dirty(&mut self) -> bool {
        let dirty = self.render_tree_cache_dirty;
        self.render_tree_cache_dirty = false;
        dirty
    }

    fn mark_render_tree_dirty(&mut self) {
        self.render_tree_cache_dirty = true;
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

    fn lookup_leaf_image_handle(
        &self,
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
    ) -> Result<LayerLookupResult, DocumentMergeError> {
        let Some(layer_lookup) = self.layer_tree.lookup_layer(layer_id) else {
            return Err(DocumentMergeError::LayerNotFound {
                layer_id,
                stroke_session_id,
            });
        };
        Ok(layer_lookup)
    }

    fn validate_replacement(
        &self,
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
        image: &Arc<TileImage>,
        replacement: &TileReplacement,
    ) -> Result<(), DocumentMergeError> {
        if replacement.tile_x >= image.tiles_per_row()
            || replacement.tile_y >= image.tiles_per_column()
        {
            return Err(DocumentMergeError::TileOutOfBounds {
                layer_id,
                stroke_session_id,
                tile_x: replacement.tile_x,
                tile_y: replacement.tile_y,
            });
        }
        Ok(())
    }

    fn validate_and_register_replacement(
        &self,
        layer_id: LayerId,
        stroke_session_id: StrokeSessionId,
        image: &Arc<TileImage>,
        replacement: &TileReplacement,
        replacements_by_tile: &mut HashMap<TileCoordinate, TileCommitEntry>,
        replacement_key_owner: &mut HashMap<TileKey, TileCoordinate>,
    ) -> Result<(), DocumentMergeError> {
        let tile_coordinate = TileCoordinate {
            tile_x: replacement.tile_x,
            tile_y: replacement.tile_y,
        };
        if replacements_by_tile.contains_key(&tile_coordinate) {
            return Err(DocumentMergeError::DuplicateTileReplacement {
                layer_id,
                stroke_session_id,
                tile_x: replacement.tile_x,
                tile_y: replacement.tile_y,
            });
        }
        if let Some(conflict_owner) = replacement_key_owner.get(&replacement.new_key) {
            return Err(DocumentMergeError::KeyConflict {
                layer_id,
                stroke_session_id,
                tile_x: replacement.tile_x,
                tile_y: replacement.tile_y,
                new_key: replacement.new_key,
                conflict_tile_x: conflict_owner.tile_x,
                conflict_tile_y: conflict_owner.tile_y,
            });
        }
        for (existing_tile_x, existing_tile_y, existing_key) in image.iter_tiles() {
            if existing_key != replacement.new_key {
                continue;
            }
            let existing_tile_coordinate = TileCoordinate {
                tile_x: existing_tile_x,
                tile_y: existing_tile_y,
            };
            let is_same_tile = existing_tile_coordinate == tile_coordinate;
            if is_same_tile {
                break;
            }
            if replacements_by_tile.contains_key(&existing_tile_coordinate) {
                break;
            }
            return Err(DocumentMergeError::KeyConflict {
                layer_id,
                stroke_session_id,
                tile_x: replacement.tile_x,
                tile_y: replacement.tile_y,
                new_key: replacement.new_key,
                conflict_tile_x: existing_tile_x,
                conflict_tile_y: existing_tile_y,
            });
        }

        let previous_key = image
            .get_tile(replacement.tile_x, replacement.tile_y)
            .map_err(|source| DocumentMergeError::VirtualImage {
                layer_id,
                stroke_session_id,
                tile_x: replacement.tile_x,
                tile_y: replacement.tile_y,
                source,
            })?;
        replacement_key_owner.insert(replacement.new_key, tile_coordinate);
        replacements_by_tile.insert(
            tile_coordinate,
            TileCommitEntry {
                new_key: replacement.new_key,
                previous_key,
            },
        );
        Ok(())
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

    fn build_render_node_snapshot(&self) -> RenderNodeSnapshot {
        match self {
            LayerTreeNode::Root { children } => RenderNodeSnapshot::Group {
                group_id: LayerNodeId::ROOT.0,
                blend: BlendMode::Normal,
                children: children
                    .iter()
                    .map(Self::build_render_node_snapshot)
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
                    .map(Self::build_render_node_snapshot)
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
                    .into(),
            },
            LayerTreeNode::Leaf {
                id,
                blend,
                image_handle,
            } => RenderNodeSnapshot::Leaf {
                layer_id: id.0,
                blend: *blend,
                image_handle: *image_handle,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_signature(document: &Document) -> String {
        render_node_signature(document.render_tree_snapshot(7).root.as_ref())
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
                "G(0:Normal)[G({}:Normal)[L({}:Normal),L({}:Normal)],L({}:Normal)]",
                branch.0, a.0, b.0, c.0
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
            format!("G(0:Normal)[L({}:Multiply)]", a.0)
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
                "G(0:Normal)[G({}:Multiply)[L({}:Normal),L({}:Normal)]]",
                branch.0, a.0, b.0
            )
        );
    }

    #[test]
    fn render_tree_snapshot_leaf_image_handle_resolves() {
        let mut document = Document::new(8, 4);
        let _ = document.new_layer_root();

        let snapshot = document.render_tree_snapshot(1);
        let image_handle = match snapshot.root.as_ref() {
            RenderNodeSnapshot::Group { children, .. } => match children.first() {
                Some(RenderNodeSnapshot::Leaf { image_handle, .. }) => *image_handle,
                _ => panic!("snapshot should contain one leaf"),
            },
            RenderNodeSnapshot::Leaf { .. } => panic!("snapshot root must be a group"),
        };

        let image = document
            .image(image_handle)
            .expect("leaf image handle should resolve");
        assert_eq!(image.size_x(), 8);
        assert_eq!(image.size_y(), 4);
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
    fn merge_finalize_replaces_tile_key_and_advances_revision() {
        let mut document = Document::new(128, 128);
        let layer_id = document.new_layer_root();
        document
            .begin_merge(layer_id.0, 77, document.revision())
            .expect("begin merge");
        document
            .commit_tile_replacements(
                layer_id.0,
                &[TileReplacement {
                    tile_x: 0,
                    tile_y: 0,
                    new_key: test_tile_key(10),
                }],
            )
            .expect("commit replacement");
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
                vec![TileCoordinate { tile_x: 0, tile_y: 0 }],
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
    fn merge_commit_batch_failure_keeps_context_unchanged() {
        let mut document = Document::new(128, 128);
        let layer_id = document.new_layer_root();
        document
            .begin_merge(layer_id.0, 88, document.revision())
            .expect("begin merge");

        let error = document
            .commit_tile_replacements(
                layer_id.0,
                &[
                    TileReplacement {
                        tile_x: 0,
                        tile_y: 0,
                        new_key: test_tile_key(11),
                    },
                    TileReplacement {
                        tile_x: 2,
                        tile_y: 0,
                        new_key: test_tile_key(12),
                    },
                ],
            )
            .expect_err("out of bounds should fail commit");
        assert!(matches!(
            error,
            DocumentMergeError::TileOutOfBounds {
                layer_id: id,
                stroke_session_id: 88,
                tile_x: 2,
                tile_y: 0
            } if id == layer_id.0
        ));
        let finalize_error = document
            .finalize_merge(layer_id.0, 88, MergeDirtyHint::None)
            .expect_err("empty staged set must not finalize");
        assert!(matches!(
            finalize_error,
            DocumentMergeError::NoTileReplacements {
                layer_id: id,
                stroke_session_id: 88
            } if id == layer_id.0
        ));
        assert_eq!(document.revision(), 0);
    }

    #[test]
    fn finalize_failure_keeps_active_merge_for_explicit_abort() {
        let mut document = Document::new(128, 128);
        let layer_id = document.new_layer_root();
        document
            .begin_merge(layer_id.0, 120, document.revision())
            .expect("begin merge");
        let finalize_error = document
            .finalize_merge(layer_id.0, 120, MergeDirtyHint::None)
            .expect_err("finalize without replacements must fail");
        assert!(matches!(
            finalize_error,
            DocumentMergeError::NoTileReplacements {
                layer_id: id,
                stroke_session_id: 120
            } if id == layer_id.0
        ));
        assert!(document.has_active_merge(layer_id.0, 120));
        document
            .abort_merge(layer_id.0, 120)
            .expect("explicit abort after finalize failure");
        assert!(!document.has_active_merge(layer_id.0, 120));
    }

    #[test]
    fn merge_rejects_replayed_stroke_session_after_finalize() {
        let mut document = Document::new(128, 128);
        let layer_id = document.new_layer_root();
        document
            .begin_merge(layer_id.0, 99, document.revision())
            .expect("begin merge");
        document
            .commit_tile_replacements(
                layer_id.0,
                &[TileReplacement {
                    tile_x: 0,
                    tile_y: 0,
                    new_key: test_tile_key(21),
                }],
            )
            .expect("commit replacement");
        document
            .finalize_merge(layer_id.0, 99, MergeDirtyHint::None)
            .expect("finalize merge");

        let replay_error = document
            .begin_merge(layer_id.0, 99, document.revision())
            .expect_err("replayed stroke session must fail");
        assert!(matches!(
            replay_error,
            DocumentMergeError::DuplicateFinalizeOrAbort {
                layer_id: id,
                stroke_session_id: 99
            } if id == layer_id.0
        ));
    }
}
