mod view;

use glaphica_core::BackendId;
use images::layout::ImageLayout;
use images::Image;
use images::ImageCreateError;

pub use view::View;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerId(u64);

impl LayerId {
    const fn initial() -> Self {
        Self(0)
    }

    fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

pub struct Document {
    layer_tree: UiLayerTree,
    render_layer_tree: RenderLayerTree,
    layout: ImageLayout,
    metadata: Metadata,
    leaf_backend: BackendId,
    branch_cache_backend: BackendId,
    next_layer_id: LayerId,
    active_layer: Option<LayerId>,
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
        let initial_id = LayerId::initial();
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
        let render_layer_tree = layer_tree.infer_render_tree(leaf_backend, branch_cache_backend)?;
        Ok(Self {
            layer_tree,
            render_layer_tree,
            layout,
            metadata: Metadata { name },
            leaf_backend,
            branch_cache_backend,
            next_layer_id: initial_id.next(),
            active_layer: Some(initial_id),
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

    pub fn render_layer_tree(&self) -> &RenderLayerTree {
        &self.render_layer_tree
    }

    pub fn rebuild_render_tree(&mut self) -> Result<(), ImageCreateError> {
        self.render_layer_tree = self
            .layer_tree
            .infer_render_tree(self.leaf_backend, self.branch_cache_backend)?;
        Ok(())
    }

    pub fn active_layer(&self) -> Option<LayerId> {
        self.active_layer
    }

    pub fn set_active_layer(&mut self, id: LayerId) {
        self.active_layer = Some(id);
    }

    pub fn clear_active_layer(&mut self) {
        self.active_layer = None;
    }

    fn allocate_layer_id(&mut self) -> LayerId {
        let id = self.next_layer_id;
        self.next_layer_id = id.next();
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
    id: LayerId,
    config: BranchConfig,
    children: Vec<UiLayerNode>,
}

#[derive(Clone, PartialEq)]
pub struct UiLeafNode {
    id: LayerId,
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
    config: BranchConfig,
    children: Vec<RenderLayerNode>,
    cache: Image,
}

#[derive(Clone, PartialEq)]
pub struct RenderLeafNode {
    config: LeafConfig,
    image: Image,
}

#[derive(Clone, PartialEq)]
pub struct LeafConfig {
    opacity: f32,
    blend_mode: LeafBlendMode,
}

#[derive(Clone, Copy, PartialEq)]
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
