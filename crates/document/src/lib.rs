mod shared_tree;
mod view;

use std::collections::HashMap;
use std::sync::Arc;

use glaphica_core::{BackendId, NodeId, RenderTreeGeneration};
use images::layout::ImageLayout;
use images::Image;
use images::ImageCreateError;

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

impl UiLayerTree {
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
                image: Image::new(self.layout, leaf_backend).unwrap(),
            })
        });
        Ok(RenderLayerTree {
            root,
            layout: self.layout,
        })
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
