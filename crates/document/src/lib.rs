use std::marker::PhantomData;

use glaphica_core::BackendId;
use images::{layout::ImageLayout, Image};

pub struct Document {
    layer_tree: UiLayerTree,
    render_layer_tree: RenderLayerTree,
    layout: ImageLayout,
    metadata: Metadata,
    leaf_backend: BackendId,
    branch_cache_backend: BackendId,
}

pub struct Metadata {
    name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiLayerTreeTag {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderLayerTreeTag {}

pub type UiLayerTree = LayerTree<UiLayerTreeTag>;
pub type RenderLayerTree = LayerTree<RenderLayerTreeTag>;

#[derive(Clone, PartialEq)]
pub struct LayerTree<T> {
    root: LayerNode<T>,
    layout: ImageLayout,
    _tag: PhantomData<T>,
}

impl UiLayerTree {
    pub fn infer_render_tree(&self, leaf_backend: BackendId) -> RenderLayerTree {
        let rendered_nodes = infer_render_nodes(&self.root, 1.0, true);
        let root = rendered_nodes.into_iter().next().unwrap_or_else(|| {
            LayerNode::Leaf(LeafNode {
                config: LeafConfig {
                    opacity: 1.0,
                    blend_mode: LeafBlendMode::Normal,
                    _tag: PhantomData,
                },
                image: Image::new(self.layout, leaf_backend).unwrap(),
                _tag: PhantomData,
            })
        });
        RenderLayerTree {
            root,
            layout: self.layout,
            _tag: PhantomData,
        }
    }
}

fn infer_render_nodes(
    node: &LayerNode<UiLayerTreeTag>,
    parent_opacity: f32,
    is_bottom: bool,
) -> Vec<LayerNode<RenderLayerTreeTag>> {
    match node {
        LayerNode::Branch(branch) => infer_render_branch(branch, parent_opacity, is_bottom),
        LayerNode::Leaf(leaf) => infer_render_leaf(leaf, parent_opacity, is_bottom),
    }
}

fn infer_render_branch(
    branch: &BranchNode<UiLayerTreeTag>,
    parent_opacity: f32,
    is_bottom: bool,
) -> Vec<LayerNode<RenderLayerTreeTag>> {
    let combined_opacity = parent_opacity * branch.config.opacity;

    // Squash penetrate branches: flatten children into parent, multiply opacity
    if matches!(branch.config.blend_mode, BranchBlendMode::Penetrate) {
        return branch
            .children
            .iter()
            .enumerate()
            .flat_map(|(i, child)| infer_render_nodes(child, combined_opacity, i == 0 && is_bottom))
            .collect();
    }

    // Squash single-child branches: no need for a group cache
    if branch.children.len() == 1 {
        return infer_render_nodes(&branch.children[0], combined_opacity, is_bottom);
    }

    let rendered_children: Vec<_> = branch
        .children
        .iter()
        .enumerate()
        .flat_map(|(i, child)| infer_render_nodes(child, 1.0, i == 0 && is_bottom))
        .collect();

    if rendered_children.is_empty() {
        return vec![];
    }

    let blend_mode = match branch.config.blend_mode {
        BranchBlendMode::Base(mode) => mode,
        BranchBlendMode::Penetrate => unreachable!(),
    };

    vec![LayerNode::Branch(BranchNode {
        config: BranchConfig {
            opacity: combined_opacity,
            blend_mode: BranchBlendMode::Base(blend_mode),
            _tag: PhantomData,
        },
        children: rendered_children,
        _tag: PhantomData,
    })]
}

fn infer_render_leaf(
    leaf: &LeafNode<UiLayerTreeTag>,
    parent_opacity: f32,
    is_bottom: bool,
) -> Vec<LayerNode<RenderLayerTreeTag>> {
    // Bottom layer always renders as Normal regardless of UI blend mode
    let blend_mode = if is_bottom {
        LeafBlendMode::Normal
    } else {
        leaf.config.blend_mode
    };

    vec![LayerNode::Leaf(LeafNode {
        config: LeafConfig {
            opacity: parent_opacity * leaf.config.opacity,
            blend_mode,
            _tag: PhantomData,
        },
        image: leaf.image.clone(),
        _tag: PhantomData,
    })]
}

#[derive(Clone, PartialEq)]
pub enum LayerNode<T> {
    Branch(BranchNode<T>),
    Leaf(LeafNode<T>),
}

#[derive(Clone, PartialEq)]
pub struct BranchNode<T> {
    config: BranchConfig<T>,
    children: Vec<LayerNode<T>>,
    _tag: PhantomData<T>,
}

#[derive(Clone, PartialEq)]
pub struct LeafNode<T> {
    config: LeafConfig<T>,
    image: Image,
    _tag: PhantomData<T>,
}

#[derive(Clone, PartialEq)]
pub struct LeafConfig<T> {
    opacity: f32,
    blend_mode: LeafBlendMode,
    _tag: PhantomData<T>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum LeafBlendMode {
    Normal,
    Multiply,
}

#[derive(Clone, PartialEq)]
pub struct BranchConfig<T> {
    opacity: f32,
    blend_mode: BranchBlendMode,
    _tag: PhantomData<T>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum BranchBlendMode {
    Base(LeafBlendMode),
    Penetrate,
}
