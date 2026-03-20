use std::collections::HashMap;
use std::sync::Arc;

use glaphica_core::{BackendId, NodeId, RenderTreeGeneration};
use images::Image;
use images::ImageCreateError;
use images::layout::ImageLayout;

use crate::node::{
    BranchBlendMode, BranchConfig, LeafBlendMode, LeafConfig, RenderBranchNode, RenderLayerNode,
    RenderLeafContent, RenderLeafNode, UiBranchNode, UiLayerNode, UiLeafContent, UiLeafNode,
};
use crate::shared_tree::{
    FlatLeafContent, FlatNodeKind, FlatRenderNode, FlatRenderTree, NodeConfig,
};

pub struct RenderLayerTree {
    pub(crate) root: RenderLayerNode,
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
                    config: NodeConfig {
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
                RenderLeafContent::Parametric { mesh } => FlatNodeKind::Leaf {
                    content: FlatLeafContent::Parametric { mesh: mesh.clone() },
                },
            };
            nodes.insert(
                id,
                FlatRenderNode {
                    parent_id,
                    config: NodeConfig {
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

pub fn infer_render_nodes(
    node: &UiLayerNode,
    parent_opacity: f32,
    is_bottom: bool,
    leaf_backend: BackendId,
    render_cache_backend: BackendId,
    layout: ImageLayout,
) -> Result<Vec<RenderLayerNode>, ImageCreateError> {
    if !node.meta().visible {
        return Ok(vec![]);
    }
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
    _render_cache_backend: BackendId,
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
            mesh: Arc::new(layer.to_parametric_mesh(layout)),
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
