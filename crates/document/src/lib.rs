mod shared_tree;
mod storage;
mod view;

use std::collections::HashMap;
use std::sync::Arc;

use glaphica_core::{BackendId, CanvasVec2, ImageDirtyTracker, NodeId, RenderTreeGeneration};
use images::Image;
use images::layout::ImageLayout;

pub use images::ImageCreateError;
pub use shared_tree::{
    FlatLeafContent, FlatNodeKind, FlatRenderNode, FlatRenderTree, MaterializeParametricCmd,
    NodeConfig, ParametricMesh, ParametricVertex, RenderCmd, RenderSource, SharedRenderTree,
};
pub use storage::{
    DocumentStorageError, DocumentStorageManifest, RasterLayerAssetMetadata,
    RasterLayerExportRequest, StoredBranchBlendMode, StoredLayerNode, StoredLeafBlendMode,
};
pub use view::View;

pub struct Document {
    layer_tree: UiLayerTree,
    layout: ImageLayout,
    metadata: Metadata,
    leaf_backend: BackendId,
    render_cache_backend: BackendId,
    next_node_id: NodeId,
    next_layer_label_index: u64,
    next_group_label_index: u64,
    active_node: Option<NodeId>,
}

pub struct Metadata {
    name: String,
}

impl Metadata {
    pub fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NewLayerKind {
    Raster,
    SolidColor { color: [f32; 4] },
}

#[derive(Debug)]
pub enum LayerEditError {
    NoActiveNode,
    InvalidNode,
    RootSelectionNotAllowed,
    MoveOutOfBounds,
    ImageCreate(ImageCreateError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayerMoveTarget {
    pub parent_id: NodeId,
    pub index: usize,
}

impl From<ImageCreateError> for LayerEditError {
    fn from(err: ImageCreateError) -> Self {
        Self::ImageCreate(err)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiNodeKind {
    Branch,
    RasterLayer,
    SpecialLayer,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiLayerTreeItem {
    pub id: NodeId,
    pub label: String,
    pub kind: UiNodeKind,
    pub solid_color: Option<[f32; 4]>,
    pub children: Vec<UiLayerTreeItem>,
}

impl Document {
    pub fn new(
        name: String,
        layout: ImageLayout,
        leaf_backend: BackendId,
        render_cache_backend: BackendId,
    ) -> Result<Self, ImageCreateError> {
        let background_id = NodeId(0);
        let paint_layer_id = NodeId(1);
        let root_id = NodeId(2);
        let image = Image::new(layout, leaf_backend)?;
        let layer_tree = UiLayerTree {
            root: UiLayerNode::Branch(UiBranchNode {
                meta: UiNodeMeta {
                    id: root_id,
                    label: "Root".to_string(),
                },
                config: BranchConfig {
                    opacity: 1.0,
                    blend_mode: BranchBlendMode::Base(LeafBlendMode::Normal),
                },
                children: vec![
                    UiLayerNode::Leaf(UiLeafNode {
                        meta: UiNodeMeta {
                            id: background_id,
                            label: "Layer 1".to_string(),
                        },
                        config: LeafConfig {
                            opacity: 1.0,
                            blend_mode: LeafBlendMode::Normal,
                        },
                        content: UiLeafContent::Special(SpecialLayer::SolidColor(
                            SolidColorLayer {
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                        )),
                    }),
                    UiLayerNode::Leaf(UiLeafNode {
                        meta: UiNodeMeta {
                            id: paint_layer_id,
                            label: "Layer 2".to_string(),
                        },
                        config: LeafConfig {
                            opacity: 1.0,
                            blend_mode: LeafBlendMode::Normal,
                        },
                        content: UiLeafContent::Raster { image },
                    }),
                ],
            }),
            layout,
        };
        Ok(Self {
            layer_tree,
            layout,
            metadata: Metadata { name },
            leaf_backend,
            render_cache_backend,
            next_node_id: NodeId(root_id.0 + 1),
            next_layer_label_index: 3,
            next_group_label_index: 1,
            active_node: Some(paint_layer_id),
        })
    }

    pub fn new_solid_color(
        name: String,
        layout: ImageLayout,
        leaf_backend: BackendId,
        render_cache_backend: BackendId,
        color: [f32; 4],
    ) -> Result<Self, ImageCreateError> {
        let initial_id = NodeId(0);
        let layer_tree = UiLayerTree {
            root: UiLayerNode::Leaf(UiLeafNode {
                meta: UiNodeMeta {
                    id: initial_id,
                    label: "Layer 1".to_string(),
                },
                config: LeafConfig {
                    opacity: 1.0,
                    blend_mode: LeafBlendMode::Normal,
                },
                content: UiLeafContent::Special(SpecialLayer::SolidColor(SolidColorLayer {
                    color,
                })),
            }),
            layout,
        };
        Ok(Self {
            layer_tree,
            layout,
            metadata: Metadata { name },
            leaf_backend,
            render_cache_backend,
            next_node_id: NodeId(initial_id.0 + 1),
            next_layer_label_index: 2,
            next_group_label_index: 1,
            active_node: Some(initial_id),
        })
    }

    pub fn leaf_backend(&self) -> BackendId {
        self.leaf_backend
    }

    pub fn render_cache_backend(&self) -> BackendId {
        self.render_cache_backend
    }

    pub fn layout(&self) -> ImageLayout {
        self.layout
    }

    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    pub fn layer_tree(&self) -> &UiLayerTree {
        &self.layer_tree
    }

    pub fn collect_raster_tile_keys(&self) -> Vec<glaphica_core::TileKey> {
        let mut keys = Vec::new();
        collect_raster_tile_keys_from_node(&self.layer_tree.root, &mut keys);
        keys
    }

    pub fn layer_tree_items(&self) -> Vec<UiLayerTreeItem> {
        self.layer_tree.items()
    }

    pub fn build_flat_render_tree(
        &self,
        generation: RenderTreeGeneration,
    ) -> Result<FlatRenderTree, ImageCreateError> {
        let render_tree = self
            .layer_tree
            .infer_render_tree(self.leaf_backend, self.render_cache_backend)?;
        Ok(render_tree.flatten(generation))
    }

    pub fn active_node(&self) -> Option<NodeId> {
        self.active_node
    }

    pub fn selected_node(&self) -> Option<NodeId> {
        self.active_node
    }

    pub fn can_select_node(&self, id: NodeId) -> bool {
        self.layer_tree.can_select_node(id)
    }

    pub fn can_paint_to_node(&self, id: NodeId) -> bool {
        self.layer_tree.can_paint_to_node(id)
    }

    pub fn active_paint_node(&self) -> Option<NodeId> {
        self.active_node.filter(|id| self.can_paint_to_node(*id))
    }

    pub fn set_active_node(&mut self, id: NodeId) -> bool {
        if !self.can_select_node(id) {
            return false;
        }
        self.active_node = Some(id);
        true
    }

    pub fn clear_active_node(&mut self) {
        self.active_node = None;
    }

    pub fn create_layer_above_active(
        &mut self,
        kind: NewLayerKind,
    ) -> Result<NodeId, LayerEditError> {
        let active_id = self.active_node.ok_or(LayerEditError::NoActiveNode)?;
        if !self.layer_tree.can_select_node(active_id) {
            return Err(LayerEditError::InvalidNode);
        }
        let node_id = self.allocate_node_id();
        let label = self.allocate_layer_label();
        let new_node = self.build_new_layer_node(node_id, label, kind)?;
        self.layer_tree.insert_node_above(active_id, new_node)?;
        self.active_node = Some(node_id);
        Ok(node_id)
    }

    pub fn create_group_above_active(&mut self) -> Result<NodeId, LayerEditError> {
        let active_id = self.active_node.ok_or(LayerEditError::NoActiveNode)?;
        if !self.layer_tree.can_select_node(active_id) {
            return Err(LayerEditError::InvalidNode);
        }
        let node_id = self.allocate_node_id();
        let label = self.allocate_group_label();
        let new_node = UiLayerNode::Branch(UiBranchNode {
            meta: UiNodeMeta { id: node_id, label },
            config: BranchConfig {
                opacity: 1.0,
                blend_mode: BranchBlendMode::Base(LeafBlendMode::Normal),
            },
            children: Vec::new(),
        });
        self.layer_tree.insert_node_above(active_id, new_node)?;
        self.active_node = Some(node_id);
        Ok(node_id)
    }

    pub fn move_active_node_up(&mut self) -> Result<(), LayerEditError> {
        let active_id = self.active_node.ok_or(LayerEditError::NoActiveNode)?;
        self.layer_tree.move_node(active_id, 1)
    }

    pub fn move_active_node_down(&mut self) -> Result<(), LayerEditError> {
        let active_id = self.active_node.ok_or(LayerEditError::NoActiveNode)?;
        self.layer_tree.move_node(active_id, -1)
    }

    pub fn move_node_to(
        &mut self,
        node_id: NodeId,
        target: LayerMoveTarget,
    ) -> Result<(), LayerEditError> {
        self.layer_tree.move_node_to(node_id, target)?;
        self.active_node = Some(node_id);
        Ok(())
    }

    pub fn get_leaf_image(&self, node_id: NodeId) -> Option<&Image> {
        self.layer_tree.get_leaf_image(node_id)
    }

    pub fn get_leaf_image_mut(&mut self, node_id: NodeId) -> Option<&mut Image> {
        self.layer_tree.get_leaf_image_mut(node_id)
    }

    pub fn get_solid_color(&self, node_id: NodeId) -> Option<[f32; 4]> {
        self.layer_tree.get_solid_color(node_id)
    }

    pub fn set_solid_color(
        &mut self,
        node_id: NodeId,
        color: [f32; 4],
    ) -> Option<ImageDirtyTracker> {
        if !self.layer_tree.set_solid_color(node_id, color) {
            return None;
        }

        let mut dirty = ImageDirtyTracker::default();
        for tile_index in 0..self.layout.total_tiles() as usize {
            dirty.mark(node_id, tile_index);
        }
        Some(dirty)
    }

    /// Incrementally syncs tile keys from UiLayerTree to FlatRenderTree.
    ///
    /// This method performs a lazy update: instead of rebuilding the entire
    /// FlatRenderTree from UiLayerTree, it only updates the tile keys at
    /// specified positions by querying UiLayerTree directly.
    ///
    /// # Usage
    ///
    /// This method should be called immediately after modifying tile keys in
    /// UiLayerTree (e.g., after a brush stroke allocates new atlas slots).
    /// Do not call this method in isolation - it assumes UiLayerTree already
    /// contains the correct tile keys to sync.
    ///
    /// # Parameters
    ///
    /// - `tree`: The current FlatRenderTree to update
    /// - `updates`: List of (NodeId, tile_index) positions to sync
    /// - `new_generation`: New generation number for the updated tree
    pub fn sync_tile_keys_to_flat_tree(
        &self,
        tree: &FlatRenderTree,
        updates: &[(NodeId, usize)],
        new_generation: RenderTreeGeneration,
    ) -> FlatRenderTree {
        if updates.is_empty() {
            return FlatRenderTree {
                generation: new_generation,
                nodes: tree.nodes.clone(),
                root_id: tree.root_id,
            };
        }

        let mut new_nodes = (*tree.nodes).clone();

        for (node_id, tile_index) in updates {
            let ui_image = match self.layer_tree.get_leaf_image(*node_id) {
                Some(img) => img,
                None => continue,
            };

            let new_tile_key = match ui_image.tile_key(*tile_index) {
                Some(key) => key,
                None => continue,
            };

            if let Some(node) = new_nodes.get_mut(node_id) {
                let image = match &mut node.kind {
                    FlatNodeKind::Leaf { content } => match content {
                        FlatLeafContent::Raster { image } => image,
                        FlatLeafContent::Parametric { render_cache, .. } => render_cache,
                    },
                    //TODO: We should update that after definede the parametric layer kind
                    FlatNodeKind::Branch { render_cache, .. } => render_cache,
                };
                if image.set_tile_key(*tile_index, new_tile_key).is_err() {
                    continue;
                }
            }
        }

        FlatRenderTree {
            generation: new_generation,
            nodes: Arc::new(new_nodes),
            root_id: tree.root_id,
        }
    }

    fn allocate_node_id(&mut self) -> NodeId {
        let id = self.next_node_id;
        self.next_node_id = NodeId(id.0 + 1);
        id
    }

    fn allocate_layer_label(&mut self) -> String {
        let index = self.next_layer_label_index;
        self.next_layer_label_index += 1;
        format!("Layer {index}")
    }

    fn allocate_group_label(&mut self) -> String {
        let index = self.next_group_label_index;
        self.next_group_label_index += 1;
        format!("Group {index}")
    }

    fn build_new_layer_node(
        &self,
        id: NodeId,
        label: String,
        kind: NewLayerKind,
    ) -> Result<UiLayerNode, ImageCreateError> {
        let content = match kind {
            NewLayerKind::Raster => UiLeafContent::Raster {
                image: Image::new(self.layout, self.leaf_backend)?,
            },
            NewLayerKind::SolidColor { color } => {
                UiLeafContent::Special(SpecialLayer::SolidColor(SolidColorLayer { color }))
            }
        };
        Ok(UiLayerNode::Leaf(UiLeafNode {
            meta: UiNodeMeta { id, label },
            config: LeafConfig {
                opacity: 1.0,
                blend_mode: LeafBlendMode::Normal,
            },
            content,
        }))
    }
}

pub struct UiLayerTree {
    root: UiLayerNode,
    layout: ImageLayout,
}

impl UiLayerTree {
    pub fn items(&self) -> Vec<UiLayerTreeItem> {
        vec![build_layer_tree_item(&self.root)]
    }

    pub fn root_id(&self) -> NodeId {
        self.root.id()
    }

    pub fn get_node(&self, node_id: NodeId) -> Option<&UiLayerNode> {
        get_node_from_node(&self.root, node_id)
    }

    pub fn contains_node(&self, node_id: NodeId) -> bool {
        self.get_node(node_id).is_some()
    }

    pub fn can_select_node(&self, node_id: NodeId) -> bool {
        match self.get_node(node_id) {
            Some(UiLayerNode::Branch(_)) => node_id != self.root_id(),
            Some(UiLayerNode::Leaf(_)) => true,
            None => false,
        }
    }

    pub fn can_paint_to_node(&self, node_id: NodeId) -> bool {
        matches!(
            self.get_node(node_id),
            Some(UiLayerNode::Leaf(UiLeafNode {
                content: UiLeafContent::Raster { .. },
                ..
            }))
        )
    }

    pub fn insert_node_above(
        &mut self,
        anchor_id: NodeId,
        new_node: UiLayerNode,
    ) -> Result<(), LayerEditError> {
        if anchor_id == self.root_id() {
            return Err(LayerEditError::RootSelectionNotAllowed);
        }
        if insert_node_above_in_branch(&mut self.root, anchor_id, new_node) {
            return Ok(());
        }
        Err(LayerEditError::InvalidNode)
    }

    pub fn move_node(&mut self, node_id: NodeId, offset: isize) -> Result<(), LayerEditError> {
        if node_id == self.root_id() {
            return Err(LayerEditError::RootSelectionNotAllowed);
        }
        match move_node_in_branch(&mut self.root, node_id, offset) {
            Some(true) => Ok(()),
            Some(false) => Err(LayerEditError::MoveOutOfBounds),
            None => Err(LayerEditError::InvalidNode),
        }
    }

    pub fn move_node_to(
        &mut self,
        node_id: NodeId,
        target: LayerMoveTarget,
    ) -> Result<(), LayerEditError> {
        if node_id == self.root_id() {
            return Err(LayerEditError::RootSelectionNotAllowed);
        }
        let moving_node = self.get_node(node_id).ok_or(LayerEditError::InvalidNode)?;
        let target_parent = self
            .get_node(target.parent_id)
            .ok_or(LayerEditError::InvalidNode)?;
        if !matches!(target_parent, UiLayerNode::Branch(_)) {
            return Err(LayerEditError::InvalidNode);
        }
        if subtree_contains_node(moving_node, target.parent_id) {
            return Err(LayerEditError::InvalidNode);
        }

        let (moved_node, source_parent_id, source_index) =
            remove_node_from_branch(&mut self.root, node_id).ok_or(LayerEditError::InvalidNode)?;
        let adjusted_index = adjust_move_index(
            source_parent_id,
            source_index,
            target.parent_id,
            target.index,
        );
        insert_node_at_parent(&mut self.root, target.parent_id, adjusted_index, moved_node)
    }

    pub fn get_leaf_image(&self, node_id: NodeId) -> Option<&Image> {
        get_leaf_image_from_node(&self.root, node_id)
    }

    pub fn get_leaf_image_mut(&mut self, node_id: NodeId) -> Option<&mut Image> {
        get_leaf_image_from_node_mut(&mut self.root, node_id)
    }

    pub fn get_solid_color(&self, node_id: NodeId) -> Option<[f32; 4]> {
        get_solid_color_from_node(&self.root, node_id)
    }

    pub fn set_solid_color(&mut self, node_id: NodeId, color: [f32; 4]) -> bool {
        set_solid_color_from_node(&mut self.root, node_id, color)
    }

    pub fn infer_render_tree(
        &self,
        leaf_backend: BackendId,
        render_cache_backend: BackendId,
    ) -> Result<RenderLayerTree, ImageCreateError> {
        let rendered_nodes = infer_render_nodes(
            &self.root,
            1.0,
            true,
            leaf_backend,
            render_cache_backend,
            self.layout,
        )?;
        let root = rendered_nodes.into_iter().next().unwrap_or_else(|| {
            RenderLayerNode::Leaf(RenderLeafNode {
                id: NodeId(u64::MAX),
                config: LeafConfig {
                    opacity: 1.0,
                    blend_mode: LeafBlendMode::Normal,
                },
                content: RenderLeafContent::Raster {
                    image: Image::new(self.layout, leaf_backend).expect(
                        "fallback image creation should succeed with valid layout and backend",
                    ),
                },
            })
        });
        Ok(RenderLayerTree {
            root,
            layout: self.layout,
        })
    }
}

fn get_node_from_node(node: &UiLayerNode, node_id: NodeId) -> Option<&UiLayerNode> {
    if node.id() == node_id {
        return Some(node);
    }
    match node {
        UiLayerNode::Branch(branch) => branch
            .children
            .iter()
            .find_map(|child| get_node_from_node(child, node_id)),
        UiLayerNode::Leaf(_) => None,
    }
}

fn build_layer_tree_item(node: &UiLayerNode) -> UiLayerTreeItem {
    match node {
        UiLayerNode::Branch(branch) => UiLayerTreeItem {
            id: branch.meta.id,
            label: branch.meta.label.clone(),
            kind: UiNodeKind::Branch,
            solid_color: None,
            children: branch.children.iter().map(build_layer_tree_item).collect(),
        },
        UiLayerNode::Leaf(leaf) => UiLayerTreeItem {
            id: leaf.meta.id,
            label: leaf.meta.label.clone(),
            kind: match &leaf.content {
                UiLeafContent::Raster { .. } => UiNodeKind::RasterLayer,
                UiLeafContent::Special(_) => UiNodeKind::SpecialLayer,
            },
            solid_color: match &leaf.content {
                UiLeafContent::Raster { .. } => None,
                UiLeafContent::Special(SpecialLayer::SolidColor(layer)) => Some(layer.color),
            },
            children: Vec::new(),
        },
    }
}

fn insert_node_above_in_branch(
    node: &mut UiLayerNode,
    anchor_id: NodeId,
    new_node: UiLayerNode,
) -> bool {
    let UiLayerNode::Branch(branch) = node else {
        return false;
    };

    if let Some(index) = branch
        .children
        .iter()
        .position(|child| child.id() == anchor_id)
    {
        branch.children.insert(index + 1, new_node);
        return true;
    }

    for child in &mut branch.children {
        if insert_node_above_in_branch(child, anchor_id, new_node.clone()) {
            return true;
        }
    }

    false
}

fn move_node_in_branch(node: &mut UiLayerNode, target_id: NodeId, offset: isize) -> Option<bool> {
    let UiLayerNode::Branch(branch) = node else {
        return None;
    };

    if let Some(index) = branch
        .children
        .iter()
        .position(|child| child.id() == target_id)
    {
        let next_index = index as isize + offset;
        if !(0..branch.children.len() as isize).contains(&next_index) {
            return Some(false);
        }
        branch.children.swap(index, next_index as usize);
        return Some(true);
    }

    for child in &mut branch.children {
        if let Some(moved) = move_node_in_branch(child, target_id, offset) {
            return Some(moved);
        }
    }

    None
}

fn subtree_contains_node(node: &UiLayerNode, target_id: NodeId) -> bool {
    if node.id() == target_id {
        return true;
    }
    match node {
        UiLayerNode::Branch(branch) => branch
            .children
            .iter()
            .any(|child| subtree_contains_node(child, target_id)),
        UiLayerNode::Leaf(_) => false,
    }
}

fn remove_node_from_branch(
    node: &mut UiLayerNode,
    target_id: NodeId,
) -> Option<(UiLayerNode, NodeId, usize)> {
    let UiLayerNode::Branch(branch) = node else {
        return None;
    };

    if let Some(index) = branch
        .children
        .iter()
        .position(|child| child.id() == target_id)
    {
        let removed = branch.children.remove(index);
        return Some((removed, branch.meta.id, index));
    }

    for child in &mut branch.children {
        if let Some(removed) = remove_node_from_branch(child, target_id) {
            return Some(removed);
        }
    }

    None
}

fn adjust_move_index(
    source_parent_id: NodeId,
    source_index: usize,
    target_parent_id: NodeId,
    target_index: usize,
) -> usize {
    if source_parent_id == target_parent_id && source_index < target_index {
        target_index.saturating_sub(1)
    } else {
        target_index
    }
}

fn insert_node_at_parent(
    node: &mut UiLayerNode,
    parent_id: NodeId,
    index: usize,
    new_node: UiLayerNode,
) -> Result<(), LayerEditError> {
    let Some(parent) = get_node_from_node_mut(node, parent_id) else {
        return Err(LayerEditError::InvalidNode);
    };
    let UiLayerNode::Branch(branch) = parent else {
        return Err(LayerEditError::InvalidNode);
    };
    if index > branch.children.len() {
        return Err(LayerEditError::MoveOutOfBounds);
    }
    branch.children.insert(index, new_node);
    Ok(())
}

fn get_leaf_image_from_node(node: &UiLayerNode, node_id: NodeId) -> Option<&Image> {
    match node {
        UiLayerNode::Branch(branch) => {
            for child in &branch.children {
                if let Some(image) = get_leaf_image_from_node(child, node_id) {
                    return Some(image);
                }
            }
            None
        }
        UiLayerNode::Leaf(leaf) => {
            if leaf.meta.id == node_id {
                match &leaf.content {
                    UiLeafContent::Raster { image } => Some(image),
                    UiLeafContent::Special(_) => None,
                }
            } else {
                None
            }
        }
    }
}

fn get_leaf_image_from_node_mut(node: &mut UiLayerNode, node_id: NodeId) -> Option<&mut Image> {
    match node {
        UiLayerNode::Branch(branch) => {
            for child in &mut branch.children {
                if let Some(image) = get_leaf_image_from_node_mut(child, node_id) {
                    return Some(image);
                }
            }
            None
        }
        UiLayerNode::Leaf(leaf) => {
            if leaf.meta.id == node_id {
                match &mut leaf.content {
                    UiLeafContent::Raster { image } => Some(image),
                    UiLeafContent::Special(_) => None,
                }
            } else {
                None
            }
        }
    }
}

fn get_node_from_node_mut(node: &mut UiLayerNode, node_id: NodeId) -> Option<&mut UiLayerNode> {
    if node.id() == node_id {
        return Some(node);
    }
    match node {
        UiLayerNode::Branch(branch) => branch
            .children
            .iter_mut()
            .find_map(|child| get_node_from_node_mut(child, node_id)),
        UiLayerNode::Leaf(_) => None,
    }
}

fn get_solid_color_from_node(node: &UiLayerNode, node_id: NodeId) -> Option<[f32; 4]> {
    match node {
        UiLayerNode::Branch(branch) => {
            for child in &branch.children {
                if let Some(color) = get_solid_color_from_node(child, node_id) {
                    return Some(color);
                }
            }
            None
        }
        UiLayerNode::Leaf(leaf) => {
            if leaf.meta.id != node_id {
                return None;
            }
            match leaf.content {
                UiLeafContent::Raster { .. } => None,
                UiLeafContent::Special(SpecialLayer::SolidColor(layer)) => Some(layer.color),
            }
        }
    }
}

fn set_solid_color_from_node(node: &mut UiLayerNode, node_id: NodeId, color: [f32; 4]) -> bool {
    match node {
        UiLayerNode::Branch(branch) => branch
            .children
            .iter_mut()
            .any(|child| set_solid_color_from_node(child, node_id, color)),
        UiLayerNode::Leaf(leaf) => {
            if leaf.meta.id != node_id {
                return false;
            }
            match &mut leaf.content {
                UiLeafContent::Raster { .. } => false,
                UiLeafContent::Special(layer) => layer.set_solid_color(color),
            }
        }
    }
}

fn collect_raster_tile_keys_from_node(
    node: &UiLayerNode,
    output: &mut Vec<glaphica_core::TileKey>,
) {
    match node {
        UiLayerNode::Branch(branch) => {
            for child in &branch.children {
                collect_raster_tile_keys_from_node(child, output);
            }
        }
        UiLayerNode::Leaf(leaf) => {
            let UiLeafContent::Raster { image } = &leaf.content else {
                return;
            };
            for tile_key in image.tile_keys().iter().copied() {
                if tile_key != glaphica_core::TileKey::EMPTY {
                    output.push(tile_key);
                }
            }
        }
    }
}

pub struct RenderLayerTree {
    root: RenderLayerNode,
    layout: ImageLayout,
}

impl RenderLayerTree {
    pub fn flatten(self, generation: RenderTreeGeneration) -> FlatRenderTree {
        let mut nodes = HashMap::new();
        let root_id = flatten_node(&self.root, None, &mut nodes);
        FlatRenderTree {
            generation,
            nodes: Arc::new(nodes),
            root_id,
        }
    }
}

fn flatten_node(
    node: &RenderLayerNode,
    parent_id: Option<NodeId>,
    nodes: &mut HashMap<NodeId, FlatRenderNode>,
) -> Option<NodeId> {
    match node {
        RenderLayerNode::Branch(branch) => {
            let id = branch.id;
            let mut child_ids = Vec::new();
            for child in &branch.children {
                if let Some(child_id) = flatten_node(child, Some(id), nodes) {
                    child_ids.push(child_id);
                }
            }
            nodes.insert(
                id,
                FlatRenderNode {
                    parent_id,
                    config: shared_tree::NodeConfig {
                        opacity: branch.config.opacity,
                        blend_mode: match branch.config.blend_mode {
                            BranchBlendMode::Base(mode) => mode,
                            BranchBlendMode::Penetrate => LeafBlendMode::Normal,
                        },
                    },
                    kind: FlatNodeKind::Branch {
                        children: child_ids,
                        render_cache: branch.render_cache.clone(),
                    },
                },
            );
            Some(id)
        }
        RenderLayerNode::Leaf(leaf) => {
            let id = leaf.id;
            let kind = match &leaf.content {
                RenderLeafContent::Raster { image } => FlatNodeKind::Leaf {
                    content: FlatLeafContent::Raster {
                        image: image.clone(),
                    },
                },
                RenderLeafContent::Parametric { mesh, render_cache } => FlatNodeKind::Leaf {
                    content: FlatLeafContent::Parametric {
                        mesh: mesh.clone(),
                        render_cache: render_cache.clone(),
                    },
                },
            };
            nodes.insert(
                id,
                FlatRenderNode {
                    parent_id,
                    config: shared_tree::NodeConfig {
                        opacity: leaf.config.opacity,
                        blend_mode: leaf.config.blend_mode,
                    },
                    kind,
                },
            );
            Some(id)
        }
    }
}

fn infer_render_nodes(
    node: &UiLayerNode,
    parent_opacity: f32,
    is_bottom: bool,
    leaf_backend: BackendId,
    render_cache_backend: BackendId,
    layout: ImageLayout,
) -> Result<Vec<RenderLayerNode>, ImageCreateError> {
    match node {
        UiLayerNode::Branch(branch) => infer_render_branch(
            branch,
            parent_opacity,
            is_bottom,
            leaf_backend,
            render_cache_backend,
            layout,
        ),
        UiLayerNode::Leaf(leaf) => infer_render_leaf(
            leaf,
            parent_opacity,
            is_bottom,
            render_cache_backend,
            layout,
        ),
    }
}

fn infer_render_branch(
    branch: &UiBranchNode,
    parent_opacity: f32,
    is_bottom: bool,
    leaf_backend: BackendId,
    render_cache_backend: BackendId,
    layout: ImageLayout,
) -> Result<Vec<RenderLayerNode>, ImageCreateError> {
    let combined_opacity = parent_opacity * branch.config.opacity;

    if matches!(branch.config.blend_mode, BranchBlendMode::Penetrate) {
        let mut result = Vec::new();
        for (i, child) in branch.children.iter().enumerate() {
            let nodes = infer_render_nodes(
                child,
                combined_opacity,
                i == 0 && is_bottom,
                leaf_backend,
                render_cache_backend,
                layout,
            )?;
            result.extend(nodes);
        }
        return Ok(result);
    }

    if branch.children.len() == 1 {
        return infer_render_nodes(
            &branch.children[0],
            combined_opacity,
            is_bottom,
            leaf_backend,
            render_cache_backend,
            layout,
        );
    }

    let mut rendered_children = Vec::new();
    for (i, child) in branch.children.iter().enumerate() {
        let nodes = infer_render_nodes(
            child,
            1.0,
            i == 0 && is_bottom,
            leaf_backend,
            render_cache_backend,
            layout,
        )?;
        rendered_children.extend(nodes);
    }

    if rendered_children.is_empty() {
        return Ok(vec![]);
    }

    let blend_mode = match branch.config.blend_mode {
        BranchBlendMode::Base(mode) => mode,
        BranchBlendMode::Penetrate => unreachable!(),
    };

    let render_cache = Image::new(layout, render_cache_backend)?;

    Ok(vec![RenderLayerNode::Branch(RenderBranchNode {
        id: branch.meta.id,
        config: BranchConfig {
            opacity: combined_opacity,
            blend_mode: BranchBlendMode::Base(blend_mode),
        },
        children: rendered_children,
        render_cache,
    })])
}

fn infer_render_leaf(
    leaf: &UiLeafNode,
    parent_opacity: f32,
    is_bottom: bool,
    render_cache_backend: BackendId,
    layout: ImageLayout,
) -> Result<Vec<RenderLayerNode>, ImageCreateError> {
    let blend_mode = if is_bottom {
        LeafBlendMode::Normal
    } else {
        leaf.config.blend_mode
    };

    let content = match &leaf.content {
        UiLeafContent::Raster { image } => RenderLeafContent::Raster {
            image: image.clone(),
        },
        UiLeafContent::Special(layer) => RenderLeafContent::Parametric {
            mesh: layer.to_parametric_mesh(layout),
            render_cache: Image::new(layout, render_cache_backend)?,
        },
    };

    Ok(vec![RenderLayerNode::Leaf(RenderLeafNode {
        id: leaf.meta.id,
        config: LeafConfig {
            opacity: parent_opacity * leaf.config.opacity,
            blend_mode,
        },
        content,
    })])
}

#[derive(Clone, PartialEq)]
pub enum UiLayerNode {
    Branch(UiBranchNode),
    Leaf(UiLeafNode),
}

impl UiLayerNode {
    pub fn id(&self) -> NodeId {
        match self {
            Self::Branch(branch) => branch.meta.id,
            Self::Leaf(leaf) => leaf.meta.id,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Branch(branch) => &branch.meta.label,
            Self::Leaf(leaf) => &leaf.meta.label,
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct UiNodeMeta {
    id: NodeId,
    label: String,
}

#[derive(Clone, PartialEq)]
pub struct UiBranchNode {
    meta: UiNodeMeta,
    config: BranchConfig,
    children: Vec<UiLayerNode>,
}

#[derive(Clone, PartialEq)]
pub struct UiLeafNode {
    meta: UiNodeMeta,
    config: LeafConfig,
    content: UiLeafContent,
}

#[derive(Clone, PartialEq)]
pub enum UiLeafContent {
    Raster { image: Image },
    Special(SpecialLayer),
}

#[derive(Clone, PartialEq)]
pub enum SpecialLayer {
    SolidColor(SolidColorLayer),
}

impl SpecialLayer {
    fn to_parametric_mesh(&self, layout: ImageLayout) -> ParametricMesh {
        match self {
            Self::SolidColor(layer) => layer.to_parametric_mesh(layout),
        }
    }

    fn set_solid_color(&mut self, color: [f32; 4]) -> bool {
        match self {
            Self::SolidColor(layer) => {
                layer.color = color;
                true
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub struct SolidColorLayer {
    color: [f32; 4],
}

impl SolidColorLayer {
    fn to_parametric_mesh(&self, layout: ImageLayout) -> ParametricMesh {
        let width = layout.size_x() as f32;
        let height = layout.size_y() as f32;
        let color = self.color;
        ParametricMesh {
            vertices: vec![
                ParametricVertex {
                    position: CanvasVec2::new(0.0, 0.0),
                    color,
                },
                ParametricVertex {
                    position: CanvasVec2::new(width, 0.0),
                    color,
                },
                ParametricVertex {
                    position: CanvasVec2::new(0.0, height),
                    color,
                },
                ParametricVertex {
                    position: CanvasVec2::new(width, height),
                    color,
                },
            ],
            indices: vec![0, 1, 2, 2, 1, 3],
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum RenderLayerNode {
    Branch(RenderBranchNode),
    Leaf(RenderLeafNode),
}

#[derive(Clone, PartialEq)]
pub struct RenderBranchNode {
    id: NodeId,
    config: BranchConfig,
    children: Vec<RenderLayerNode>,
    render_cache: Image,
}

#[derive(Clone, PartialEq)]
pub struct RenderLeafNode {
    id: NodeId,
    config: LeafConfig,
    content: RenderLeafContent,
}

#[derive(Clone, PartialEq)]
pub enum RenderLeafContent {
    Raster {
        image: Image,
    },
    Parametric {
        mesh: ParametricMesh,
        render_cache: Image,
    },
}

#[derive(Clone, PartialEq)]
pub struct LeafConfig {
    opacity: f32,
    blend_mode: LeafBlendMode,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LeafBlendMode {
    Normal,
    Multiply,
}

#[derive(Clone, PartialEq)]
pub struct BranchConfig {
    opacity: f32,
    blend_mode: BranchBlendMode,
}

#[derive(Clone, Copy, PartialEq)]
pub enum BranchBlendMode {
    Base(LeafBlendMode),
    Penetrate,
}

#[cfg(test)]
mod tests {
    use super::*;
    use glaphica_core::{BackendId, NodeId, RenderTreeGeneration, TileKey};
    use images::layout::ImageLayout;
    use std::sync::Arc;

    fn test_meta(id: NodeId, label: &str) -> UiNodeMeta {
        UiNodeMeta {
            id,
            label: label.to_string(),
        }
    }

    #[test]
    fn test_sync_tile_keys_partial_update_preserves_untouched_tiles() {
        // Create a document with a leaf node that has 2 tiles
        let layout = ImageLayout::new(256, 128); // 2 tiles horizontally

        let mut leaf_image = Image::new(layout, BackendId::new(1)).unwrap();
        leaf_image
            .set_tile_key(0, TileKey::from_parts(1, 1, 100))
            .unwrap();
        leaf_image
            .set_tile_key(1, TileKey::from_parts(1, 1, 101))
            .unwrap();

        let leaf_node = UiLeafNode {
            meta: test_meta(NodeId(0), "Layer 1"),
            config: LeafConfig {
                opacity: 1.0,
                blend_mode: LeafBlendMode::Normal,
            },
            content: UiLeafContent::Raster { image: leaf_image },
        };

        let ui_tree = UiLayerTree {
            root: UiLayerNode::Leaf(leaf_node),
            layout,
        };

        let mut doc = Document {
            layer_tree: ui_tree,
            layout,
            leaf_backend: BackendId::new(1),
            render_cache_backend: BackendId::new(2),
            next_node_id: NodeId(1),
            next_layer_label_index: 2,
            next_group_label_index: 1,
            active_node: None,
            metadata: Metadata {
                name: "test".to_string(),
            },
        };

        // Create initial flat render tree
        let mut nodes = std::collections::HashMap::new();
        nodes.insert(
            NodeId(0),
            FlatRenderNode {
                parent_id: None,
                config: NodeConfig {
                    opacity: 1.0,
                    blend_mode: LeafBlendMode::Normal,
                },
                kind: FlatNodeKind::Leaf {
                    content: FlatLeafContent::Raster {
                        image: {
                            let mut img = Image::new(layout, BackendId::new(1)).unwrap();
                            img.set_tile_key(0, TileKey::from_parts(1, 1, 100)).unwrap();
                            img.set_tile_key(1, TileKey::from_parts(1, 1, 101)).unwrap();
                            img
                        },
                    },
                },
            },
        );

        let old_tree = FlatRenderTree {
            generation: RenderTreeGeneration(0),
            nodes: Arc::new(nodes),
            root_id: Some(NodeId(0)),
        };

        // Simulate: update tile 1 to a new key in the document
        doc.get_leaf_image_mut(NodeId(0))
            .unwrap()
            .set_tile_key(1, TileKey::from_parts(1, 1, 201))
            .unwrap();

        // Partial update: only sync tile 1
        let updates = vec![(NodeId(0), 1)];

        let new_tree =
            doc.sync_tile_keys_to_flat_tree(&old_tree, &updates, RenderTreeGeneration(1));

        // Verify that both tiles are preserved correctly
        let new_node = new_tree.nodes.get(&NodeId(0)).unwrap();
        let new_image = match &new_node.kind {
            FlatNodeKind::Leaf {
                content: FlatLeafContent::Raster { image },
            } => image,
            FlatNodeKind::Leaf {
                content: FlatLeafContent::Parametric { .. },
            }
            | FlatNodeKind::Branch { .. } => panic!("Expected raster leaf node"),
        };

        // Tile 0 should keep its original key (from old_tree)
        assert_eq!(
            new_image.tile_key(0),
            Some(TileKey::from_parts(1, 1, 100)),
            "Tile 0 should preserve original key from old tree"
        );

        // Tile 1 should have the updated key from doc.layer_tree
        assert_eq!(
            new_image.tile_key(1),
            Some(TileKey::from_parts(1, 1, 201)),
            "Tile 1 should have updated key from document"
        );
    }

    #[test]
    fn test_flatten_preserves_parametric_mesh() {
        let layout = ImageLayout::new(128, 128);
        let render_cache = Image::new(layout, BackendId::new(2)).unwrap();
        let mesh = ParametricMesh {
            vertices: vec![
                ParametricVertex {
                    position: glaphica_core::CanvasVec2::new(0.0, 0.0),
                    color: [1.0, 0.0, 0.0, 1.0],
                },
                ParametricVertex {
                    position: glaphica_core::CanvasVec2::new(128.0, 0.0),
                    color: [0.0, 1.0, 0.0, 1.0],
                },
                ParametricVertex {
                    position: glaphica_core::CanvasVec2::new(0.0, 128.0),
                    color: [0.0, 0.0, 1.0, 1.0],
                },
            ],
            indices: vec![0, 1, 2],
        };

        let tree = RenderLayerTree {
            root: RenderLayerNode::Leaf(RenderLeafNode {
                id: NodeId(7),
                config: LeafConfig {
                    opacity: 1.0,
                    blend_mode: LeafBlendMode::Normal,
                },
                content: RenderLeafContent::Parametric {
                    mesh: mesh.clone(),
                    render_cache,
                },
            }),
            layout,
        };

        let flat = tree.flatten(RenderTreeGeneration(1));
        let node = flat.nodes.get(&NodeId(7)).unwrap();
        let flat_mesh = match &node.kind {
            FlatNodeKind::Leaf {
                content: FlatLeafContent::Parametric { mesh, .. },
            } => mesh,
            FlatNodeKind::Leaf {
                content: FlatLeafContent::Raster { .. },
            }
            | FlatNodeKind::Branch { .. } => panic!("Expected parametric leaf node"),
        };

        assert_eq!(flat_mesh.vertices.len(), 3);
        assert_eq!(flat_mesh.indices, vec![0, 1, 2]);
        assert_eq!(flat_mesh, &mesh);
    }

    #[test]
    fn test_solid_color_leaf_infers_parametric_mesh() {
        let layout = ImageLayout::new(128, 64);
        let tree = UiLayerTree {
            root: UiLayerNode::Leaf(UiLeafNode {
                meta: test_meta(NodeId(9), "Layer 1"),
                config: LeafConfig {
                    opacity: 1.0,
                    blend_mode: LeafBlendMode::Normal,
                },
                content: UiLeafContent::Special(SpecialLayer::SolidColor(SolidColorLayer {
                    color: [0.2, 0.4, 0.6, 1.0],
                })),
            }),
            layout,
        };

        let render_tree = tree
            .infer_render_tree(BackendId::new(1), BackendId::new(2))
            .unwrap();
        let flat = render_tree.flatten(RenderTreeGeneration(2));
        let node = flat.nodes.get(&NodeId(9)).unwrap();

        let (mesh, render_cache) = match &node.kind {
            FlatNodeKind::Leaf {
                content: FlatLeafContent::Parametric { mesh, render_cache },
            } => (mesh, render_cache),
            FlatNodeKind::Leaf {
                content: FlatLeafContent::Raster { .. },
            }
            | FlatNodeKind::Branch { .. } => {
                panic!("expected solid color leaf to lower to parametric")
            }
        };

        assert_eq!(mesh.vertices.len(), 4);
        assert_eq!(mesh.indices, vec![0, 1, 2, 2, 1, 3]);
        assert_eq!(mesh.vertices[0].position, CanvasVec2::new(0.0, 0.0));
        assert_eq!(mesh.vertices[3].position, CanvasVec2::new(128.0, 64.0));
        assert_eq!(mesh.vertices[0].color, [0.2, 0.4, 0.6, 1.0]);
        assert_eq!(*render_cache.layout(), layout);
    }

    #[test]
    fn test_set_solid_color_marks_all_tiles_dirty() {
        let layout = ImageLayout::new(256, 128);
        let mut doc = Document::new_solid_color(
            "solid".to_string(),
            layout,
            BackendId::new(1),
            BackendId::new(2),
            [0.1, 0.2, 0.3, 1.0],
        )
        .unwrap();

        let dirty = doc
            .set_solid_color(NodeId(0), [0.7, 0.6, 0.5, 1.0])
            .expect("solid color node should exist");

        let mut keys = dirty.iter().collect::<Vec<_>>();
        keys.sort_by_key(|key| key.tile_index);

        assert_eq!(doc.get_solid_color(NodeId(0)), Some([0.7, 0.6, 0.5, 1.0]));
        assert_eq!(keys.len(), layout.total_tiles() as usize);
        assert_eq!(keys[0].node_id, NodeId(0));
        assert_eq!(keys[0].tile_index, 0);
        assert_eq!(keys[1].tile_index, 1);
    }

    #[test]
    fn test_rebuild_after_solid_color_change_updates_mesh_color() {
        let layout = ImageLayout::new(64, 64);
        let mut doc = Document::new_solid_color(
            "solid".to_string(),
            layout,
            BackendId::new(1),
            BackendId::new(2),
            [0.1, 0.2, 0.3, 1.0],
        )
        .unwrap();
        doc.set_solid_color(NodeId(0), [0.8, 0.1, 0.4, 1.0]);

        let flat = doc.build_flat_render_tree(RenderTreeGeneration(3)).unwrap();
        let node = flat.nodes.get(&NodeId(0)).unwrap();
        let mesh = match &node.kind {
            FlatNodeKind::Leaf {
                content: FlatLeafContent::Parametric { mesh, .. },
            } => mesh,
            FlatNodeKind::Leaf {
                content: FlatLeafContent::Raster { .. },
            }
            | FlatNodeKind::Branch { .. } => panic!("expected parametric leaf node"),
        };

        assert_eq!(mesh.vertices[0].color, [0.8, 0.1, 0.4, 1.0]);
        assert!(
            mesh.vertices
                .iter()
                .all(|vertex| vertex.color == [0.8, 0.1, 0.4, 1.0])
        );
    }

    #[test]
    fn test_default_document_has_white_background_and_active_raster_layer() {
        let layout = ImageLayout::new(64, 64);
        let doc = Document::new(
            "default".to_string(),
            layout,
            BackendId::new(1),
            BackendId::new(2),
        )
        .unwrap();

        assert_eq!(doc.active_node(), Some(NodeId(1)));
        assert_eq!(doc.get_solid_color(NodeId(0)), Some([1.0, 1.0, 1.0, 1.0]));
        assert!(doc.get_leaf_image(NodeId(1)).is_some());
        assert_eq!(
            doc.layer_tree().get_node(NodeId(1)).map(UiLayerNode::label),
            Some("Layer 2")
        );
    }

    #[test]
    fn test_branch_can_be_selected_but_not_painted() {
        let layout = ImageLayout::new(64, 64);
        let mut doc = Document::new(
            "default".to_string(),
            layout,
            BackendId::new(1),
            BackendId::new(2),
        )
        .unwrap();
        let group_id = doc.allocate_node_id();
        let root_id = doc.layer_tree().root_id();
        doc.layer_tree
            .insert_node_above(
                NodeId(1),
                UiLayerNode::Branch(UiBranchNode {
                    meta: test_meta(group_id, "Group 1"),
                    config: BranchConfig {
                        opacity: 1.0,
                        blend_mode: BranchBlendMode::Base(LeafBlendMode::Normal),
                    },
                    children: Vec::new(),
                }),
            )
            .unwrap();

        assert!(!doc.set_active_node(root_id));
        assert!(doc.set_active_node(group_id));
        assert_eq!(doc.active_node(), Some(group_id));
        assert_eq!(doc.active_paint_node(), None);
        assert!(!doc.can_paint_to_node(group_id));
    }

    #[test]
    fn test_create_layer_above_selected_branch_inserts_as_sibling() {
        let layout = ImageLayout::new(64, 64);
        let mut doc = Document::new(
            "default".to_string(),
            layout,
            BackendId::new(1),
            BackendId::new(2),
        )
        .unwrap();
        let group_id = doc.allocate_node_id();
        doc.layer_tree
            .insert_node_above(
                NodeId(1),
                UiLayerNode::Branch(UiBranchNode {
                    meta: test_meta(group_id, "Group 1"),
                    config: BranchConfig {
                        opacity: 1.0,
                        blend_mode: BranchBlendMode::Base(LeafBlendMode::Normal),
                    },
                    children: Vec::new(),
                }),
            )
            .unwrap();
        assert!(doc.set_active_node(group_id));

        let new_layer_id = doc.create_layer_above_active(NewLayerKind::Raster).unwrap();
        let root = match doc.layer_tree().get_node(doc.layer_tree().root_id()) {
            Some(UiLayerNode::Branch(branch)) => branch,
            Some(UiLayerNode::Leaf(_)) | None => panic!("expected branch root"),
        };

        assert_eq!(root.children.len(), 4);
        assert_eq!(root.children[2].id(), group_id);
        assert_eq!(root.children[3].id(), new_layer_id);
        assert_eq!(root.children[3].label(), "Layer 3");
        assert_eq!(doc.active_node(), Some(new_layer_id));
        assert_eq!(doc.active_paint_node(), Some(new_layer_id));
    }

    #[test]
    fn test_create_group_above_active_selects_new_group() {
        let layout = ImageLayout::new(64, 64);
        let mut doc = Document::new(
            "default".to_string(),
            layout,
            BackendId::new(1),
            BackendId::new(2),
        )
        .unwrap();

        let group_id = doc.create_group_above_active().unwrap();
        let root = match doc.layer_tree().get_node(doc.layer_tree().root_id()) {
            Some(UiLayerNode::Branch(branch)) => branch,
            Some(UiLayerNode::Leaf(_)) | None => panic!("expected branch root"),
        };

        assert_eq!(root.children.len(), 3);
        assert_eq!(root.children[2].id(), group_id);
        assert_eq!(root.children[2].label(), "Group 1");
        assert_eq!(doc.active_node(), Some(group_id));
        assert_eq!(doc.active_paint_node(), None);
    }

    #[test]
    fn test_move_active_node_up_and_down_reorders_siblings() {
        let layout = ImageLayout::new(64, 64);
        let mut doc = Document::new(
            "default".to_string(),
            layout,
            BackendId::new(1),
            BackendId::new(2),
        )
        .unwrap();

        let group_id = doc.create_group_above_active().unwrap();
        assert!(doc.set_active_node(NodeId(1)));
        doc.move_active_node_up().unwrap();

        let root = match doc.layer_tree().get_node(doc.layer_tree().root_id()) {
            Some(UiLayerNode::Branch(branch)) => branch,
            Some(UiLayerNode::Leaf(_)) | None => panic!("expected branch root"),
        };
        assert_eq!(root.children[0].id(), NodeId(0));
        assert_eq!(root.children[1].id(), group_id);
        assert_eq!(root.children[2].id(), NodeId(1));

        doc.move_active_node_down().unwrap();
        let root = match doc.layer_tree().get_node(doc.layer_tree().root_id()) {
            Some(UiLayerNode::Branch(branch)) => branch,
            Some(UiLayerNode::Leaf(_)) | None => panic!("expected branch root"),
        };
        assert_eq!(root.children[0].id(), NodeId(0));
        assert_eq!(root.children[1].id(), NodeId(1));
        assert_eq!(root.children[2].id(), group_id);
    }

    #[test]
    fn test_move_node_to_reparents_leaf_into_group() {
        let layout = ImageLayout::new(64, 64);
        let mut doc = Document::new(
            "default".to_string(),
            layout,
            BackendId::new(1),
            BackendId::new(2),
        )
        .unwrap();

        let group_id = doc.create_group_above_active().unwrap();
        doc.move_node_to(
            NodeId(1),
            LayerMoveTarget {
                parent_id: group_id,
                index: 0,
            },
        )
        .unwrap();

        let root = match doc.layer_tree().get_node(doc.layer_tree().root_id()) {
            Some(UiLayerNode::Branch(branch)) => branch,
            Some(UiLayerNode::Leaf(_)) | None => panic!("expected branch root"),
        };
        assert_eq!(root.children[0].id(), NodeId(0));
        assert_eq!(root.children[1].id(), group_id);

        let group = match doc.layer_tree().get_node(group_id) {
            Some(UiLayerNode::Branch(branch)) => branch,
            Some(UiLayerNode::Leaf(_)) | None => panic!("expected branch group"),
        };
        assert_eq!(group.children.len(), 1);
        assert_eq!(group.children[0].id(), NodeId(1));
        assert_eq!(doc.active_node(), Some(NodeId(1)));
    }

    #[test]
    fn test_move_node_to_cannot_move_group_into_descendant() {
        let layout = ImageLayout::new(64, 64);
        let mut doc = Document::new(
            "default".to_string(),
            layout,
            BackendId::new(1),
            BackendId::new(2),
        )
        .unwrap();

        let parent_group_id = doc.create_group_above_active().unwrap();
        let child_group_id = doc.allocate_node_id();
        doc.layer_tree
            .move_node_to(
                NodeId(1),
                LayerMoveTarget {
                    parent_id: parent_group_id,
                    index: 0,
                },
            )
            .unwrap();
        doc.layer_tree
            .move_node_to(
                parent_group_id,
                LayerMoveTarget {
                    parent_id: doc.layer_tree().root_id(),
                    index: 1,
                },
            )
            .unwrap();
        if let Some(UiLayerNode::Branch(branch)) =
            get_node_from_node_mut(&mut doc.layer_tree.root, parent_group_id)
        {
            branch.children.push(UiLayerNode::Branch(UiBranchNode {
                meta: test_meta(child_group_id, "Child"),
                config: BranchConfig {
                    opacity: 1.0,
                    blend_mode: BranchBlendMode::Base(LeafBlendMode::Normal),
                },
                children: Vec::new(),
            }));
        } else {
            panic!("expected mutable branch group");
        }

        let result = doc.move_node_to(
            parent_group_id,
            LayerMoveTarget {
                parent_id: child_group_id,
                index: 0,
            },
        );
        assert!(matches!(result, Err(LayerEditError::InvalidNode)));
    }

    #[test]
    fn test_layer_tree_items_preserve_labels_and_hierarchy() {
        let layout = ImageLayout::new(64, 64);
        let doc = Document::new(
            "default".to_string(),
            layout,
            BackendId::new(1),
            BackendId::new(2),
        )
        .unwrap();

        let items = doc.layer_tree_items();

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, UiNodeKind::Branch);
        assert_eq!(items[0].label, "Root");
        assert_eq!(items[0].children.len(), 2);
        assert_eq!(items[0].children[0].label, "Layer 1");
        assert_eq!(items[0].children[0].kind, UiNodeKind::SpecialLayer);
        assert_eq!(items[0].children[1].label, "Layer 2");
        assert_eq!(items[0].children[1].kind, UiNodeKind::RasterLayer);
    }
}
