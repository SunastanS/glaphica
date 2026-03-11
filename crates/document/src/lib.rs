mod shared_tree;
mod view;

use std::collections::HashMap;
use std::sync::Arc;

use glaphica_core::{BackendId, NodeId, RenderTreeGeneration};
use images::Image;
use images::layout::ImageLayout;

pub use images::ImageCreateError;
pub use shared_tree::{
    FlatNodeKind, FlatRenderNode, FlatRenderTree, NodeConfig, RenderCmd, RenderSource,
    SharedRenderTree,
};
pub use view::View;

pub struct Document {
    layer_tree: UiLayerTree,
    layout: ImageLayout,
    metadata: Metadata,
    leaf_backend: BackendId,
    branch_cache_backend: BackendId,
    next_node_id: NodeId,
    active_node: Option<NodeId>,
}

pub struct Metadata {
    name: String,
}

impl Document {
    pub fn new(
        name: String,
        layout: ImageLayout,
        leaf_backend: BackendId,
        branch_cache_backend: BackendId,
    ) -> Result<Self, ImageCreateError> {
        let initial_id = NodeId(0);
        let image = Image::new(layout, leaf_backend)?;
        let layer_tree = UiLayerTree {
            root: UiLayerNode::Leaf(UiLeafNode {
                id: initial_id,
                config: LeafConfig {
                    opacity: 1.0,
                    blend_mode: LeafBlendMode::Normal,
                },
                image,
            }),
            layout,
        };
        Ok(Self {
            layer_tree,
            layout,
            metadata: Metadata { name },
            leaf_backend,
            branch_cache_backend,
            next_node_id: NodeId(initial_id.0 + 1),
            active_node: Some(initial_id),
        })
    }

    pub fn leaf_backend(&self) -> BackendId {
        self.leaf_backend
    }

    pub fn branch_cache_backend(&self) -> BackendId {
        self.branch_cache_backend
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

    pub fn build_flat_render_tree(
        &self,
        generation: RenderTreeGeneration,
    ) -> Result<FlatRenderTree, ImageCreateError> {
        let render_tree = self
            .layer_tree
            .infer_render_tree(self.leaf_backend, self.branch_cache_backend)?;
        Ok(render_tree.flatten(generation))
    }

    pub fn active_node(&self) -> Option<NodeId> {
        self.active_node
    }

    pub fn set_active_node(&mut self, id: NodeId) {
        self.active_node = Some(id);
    }

    pub fn clear_active_node(&mut self) {
        self.active_node = None;
    }

    pub fn get_leaf_image(&self, node_id: NodeId) -> Option<&Image> {
        self.layer_tree.get_leaf_image(node_id)
    }

    pub fn get_leaf_image_mut(&mut self, node_id: NodeId) -> Option<&mut Image> {
        self.layer_tree.get_leaf_image_mut(node_id)
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
                    FlatNodeKind::Leaf { image } => image,
                    FlatNodeKind::Branch { cache, .. } => cache,
                };
                let _ = image.set_tile_key(*tile_index, new_tile_key);
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
}

pub struct UiLayerTree {
    root: UiLayerNode,
    layout: ImageLayout,
}

impl UiLayerTree {
    pub fn get_leaf_image(&self, node_id: NodeId) -> Option<&Image> {
        get_leaf_image_from_node(&self.root, node_id)
    }

    pub fn get_leaf_image_mut(&mut self, node_id: NodeId) -> Option<&mut Image> {
        get_leaf_image_from_node_mut(&mut self.root, node_id)
    }

    pub fn infer_render_tree(
        &self,
        leaf_backend: BackendId,
        branch_cache_backend: BackendId,
    ) -> Result<RenderLayerTree, ImageCreateError> {
        let rendered_nodes = infer_render_nodes(
            &self.root,
            1.0,
            true,
            leaf_backend,
            branch_cache_backend,
            self.layout,
        )?;
        let root = rendered_nodes.into_iter().next().unwrap_or_else(|| {
            RenderLayerNode::Leaf(RenderLeafNode {
                id: NodeId(u64::MAX),
                config: LeafConfig {
                    opacity: 1.0,
                    blend_mode: LeafBlendMode::Normal,
                },
                image: Image::new(self.layout, leaf_backend)
                    .expect("fallback image creation should succeed with valid layout and backend"),
            })
        });
        Ok(RenderLayerTree {
            root,
            layout: self.layout,
        })
    }
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
            if leaf.id == node_id {
                Some(&leaf.image)
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
            if leaf.id == node_id {
                Some(&mut leaf.image)
            } else {
                None
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
                        cache: branch.cache.clone(),
                    },
                },
            );
            Some(id)
        }
        RenderLayerNode::Leaf(leaf) => {
            let id = leaf.id;
            nodes.insert(
                id,
                FlatRenderNode {
                    parent_id,
                    config: shared_tree::NodeConfig {
                        opacity: leaf.config.opacity,
                        blend_mode: leaf.config.blend_mode,
                    },
                    kind: FlatNodeKind::Leaf {
                        image: leaf.image.clone(),
                    },
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
    branch_cache_backend: BackendId,
    layout: ImageLayout,
) -> Result<Vec<RenderLayerNode>, ImageCreateError> {
    match node {
        UiLayerNode::Branch(branch) => infer_render_branch(
            branch,
            parent_opacity,
            is_bottom,
            leaf_backend,
            branch_cache_backend,
            layout,
        ),
        UiLayerNode::Leaf(leaf) => Ok(infer_render_leaf(leaf, parent_opacity, is_bottom)),
    }
}

fn infer_render_branch(
    branch: &UiBranchNode,
    parent_opacity: f32,
    is_bottom: bool,
    leaf_backend: BackendId,
    branch_cache_backend: BackendId,
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
                branch_cache_backend,
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
            branch_cache_backend,
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
            branch_cache_backend,
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

    let cache = Image::new(layout, branch_cache_backend)?;

    Ok(vec![RenderLayerNode::Branch(RenderBranchNode {
        id: branch.id,
        config: BranchConfig {
            opacity: combined_opacity,
            blend_mode: BranchBlendMode::Base(blend_mode),
        },
        children: rendered_children,
        cache,
    })])
}

fn infer_render_leaf(
    leaf: &UiLeafNode,
    parent_opacity: f32,
    is_bottom: bool,
) -> Vec<RenderLayerNode> {
    let blend_mode = if is_bottom {
        LeafBlendMode::Normal
    } else {
        leaf.config.blend_mode
    };

    vec![RenderLayerNode::Leaf(RenderLeafNode {
        id: leaf.id,
        config: LeafConfig {
            opacity: parent_opacity * leaf.config.opacity,
            blend_mode,
        },
        image: leaf.image.clone(),
    })]
}

#[derive(Clone, PartialEq)]
pub enum UiLayerNode {
    Branch(UiBranchNode),
    Leaf(UiLeafNode),
}

#[derive(Clone, PartialEq)]
pub struct UiBranchNode {
    id: NodeId,
    config: BranchConfig,
    children: Vec<UiLayerNode>,
}

#[derive(Clone, PartialEq)]
pub struct UiLeafNode {
    id: NodeId,
    config: LeafConfig,
    image: Image,
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
    cache: Image,
}

#[derive(Clone, PartialEq)]
pub struct RenderLeafNode {
    id: NodeId,
    config: LeafConfig,
    image: Image,
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
            id: NodeId(0),
            config: LeafConfig {
                opacity: 1.0,
                blend_mode: LeafBlendMode::Normal,
            },
            image: leaf_image,
        };

        let ui_tree = UiLayerTree {
            root: UiLayerNode::Leaf(leaf_node),
            layout,
        };

        let mut doc = Document {
            layer_tree: ui_tree,
            layout,
            leaf_backend: BackendId::new(1),
            branch_cache_backend: BackendId::new(2),
            next_node_id: NodeId(1),
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
                    image: {
                        let mut img = Image::new(layout, BackendId::new(1)).unwrap();
                        img.set_tile_key(0, TileKey::from_parts(1, 1, 100)).unwrap();
                        img.set_tile_key(1, TileKey::from_parts(1, 1, 101)).unwrap();
                        img
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
            FlatNodeKind::Leaf { image } => image,
            FlatNodeKind::Branch { .. } => panic!("Expected leaf node"),
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
}
