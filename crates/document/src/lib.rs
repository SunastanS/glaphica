mod shared_tree;
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
pub use view::View;

pub struct Document {
    layer_tree: UiLayerTree,
    layout: ImageLayout,
    metadata: Metadata,
    leaf_backend: BackendId,
    render_cache_backend: BackendId,
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
        render_cache_backend: BackendId,
    ) -> Result<Self, ImageCreateError> {
        let background_id = NodeId(0);
        let paint_layer_id = NodeId(1);
        let root_id = NodeId(2);
        let image = Image::new(layout, leaf_backend)?;
        let layer_tree = UiLayerTree {
            root: UiLayerNode::Branch(UiBranchNode {
                id: root_id,
                config: BranchConfig {
                    opacity: 1.0,
                    blend_mode: BranchBlendMode::Base(LeafBlendMode::Normal),
                },
                children: vec![
                    UiLayerNode::Leaf(UiLeafNode {
                        id: background_id,
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
                        id: paint_layer_id,
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
                id: initial_id,
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
            if leaf.id == node_id {
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
            if leaf.id != node_id {
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
            if leaf.id != node_id {
                return false;
            }
            match &mut leaf.content {
                UiLeafContent::Raster { .. } => false,
                UiLeafContent::Special(layer) => layer.set_solid_color(color),
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
        id: branch.id,
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
        id: leaf.id,
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
                id: NodeId(9),
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
    }
}
