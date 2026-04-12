use gla_image::GlaImage;
use slotmap::new_key_type;
use smallvec::SmallVec;

new_key_type! {
    pub struct GlaNodeId;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GlaBlendMode {
    #[default]
    Normal,
    Multiply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlaNodeKind {
    Root,
    Branch,
    Leaf,
}

struct GlaNodeShared {
    parent: Option<GlaNodeId>,
    opacity: f32,
    blend_mode: GlaBlendMode,
    image: GlaImage,
}

pub struct GlaBranchNode {
    shared: GlaNodeShared,
    children: SmallVec<[GlaNodeId; 8]>,
}

pub struct GlaLeafNode {
    shared: GlaNodeShared,
}

pub enum GlaNode {
    Root(GlaBranchNode),
    Branch(GlaBranchNode),
    Leaf(GlaLeafNode),
}

impl GlaNodeShared {
    fn new(
        parent: Option<GlaNodeId>,
        image: GlaImage,
        opacity: f32,
        blend_mode: GlaBlendMode,
    ) -> Self {
        Self {
            parent,
            opacity,
            blend_mode,
            image,
        }
    }
}

impl GlaBranchNode {
    pub(crate) fn new(
        parent: Option<GlaNodeId>,
        image: GlaImage,
        opacity: f32,
        blend_mode: GlaBlendMode,
    ) -> Self {
        Self {
            shared: GlaNodeShared::new(parent, image, opacity, blend_mode),
            children: SmallVec::new(),
        }
    }
}

impl GlaLeafNode {
    pub(crate) fn new(
        parent: Option<GlaNodeId>,
        image: GlaImage,
        opacity: f32,
        blend_mode: GlaBlendMode,
    ) -> Self {
        Self {
            shared: GlaNodeShared::new(parent, image, opacity, blend_mode),
        }
    }
}

impl GlaNode {
    pub(crate) fn new_root(image: GlaImage, opacity: f32, blend_mode: GlaBlendMode) -> Self {
        Self::Root(GlaBranchNode::new(None, image, opacity, blend_mode))
    }

    pub(crate) fn new_branch(
        parent: GlaNodeId,
        image: GlaImage,
        opacity: f32,
        blend_mode: GlaBlendMode,
    ) -> Self {
        Self::Branch(GlaBranchNode::new(Some(parent), image, opacity, blend_mode))
    }

    pub(crate) fn new_leaf(
        parent: GlaNodeId,
        image: GlaImage,
        opacity: f32,
        blend_mode: GlaBlendMode,
    ) -> Self {
        Self::Leaf(GlaLeafNode::new(Some(parent), image, opacity, blend_mode))
    }

    pub fn kind(&self) -> GlaNodeKind {
        match self {
            Self::Root(_) => GlaNodeKind::Root,
            Self::Branch(_) => GlaNodeKind::Branch,
            Self::Leaf(_) => GlaNodeKind::Leaf,
        }
    }

    pub fn parent(&self) -> Option<GlaNodeId> {
        match self {
            Self::Root(branch) | Self::Branch(branch) => branch.shared.parent,
            Self::Leaf(leaf) => leaf.shared.parent,
        }
    }

    pub fn opacity(&self) -> f32 {
        match self {
            Self::Root(branch) | Self::Branch(branch) => branch.shared.opacity,
            Self::Leaf(leaf) => leaf.shared.opacity,
        }
    }

    pub fn blend_mode(&self) -> GlaBlendMode {
        match self {
            Self::Root(branch) | Self::Branch(branch) => branch.shared.blend_mode,
            Self::Leaf(leaf) => leaf.shared.blend_mode,
        }
    }

    pub fn image(&self) -> &GlaImage {
        match self {
            Self::Root(branch) | Self::Branch(branch) => &branch.shared.image,
            Self::Leaf(leaf) => &leaf.shared.image,
        }
    }

    pub fn children(&self) -> Option<&[GlaNodeId]> {
        match self {
            Self::Root(branch) | Self::Branch(branch) => Some(branch.children.as_slice()),
            Self::Leaf(_) => None,
        }
    }

    pub(crate) fn image_mut(&mut self) -> &mut GlaImage {
        match self {
            Self::Root(branch) | Self::Branch(branch) => &mut branch.shared.image,
            Self::Leaf(leaf) => &mut leaf.shared.image,
        }
    }

    pub(crate) fn children_mut(&mut self) -> Option<&mut SmallVec<[GlaNodeId; 8]>> {
        match self {
            Self::Root(branch) | Self::Branch(branch) => Some(&mut branch.children),
            Self::Leaf(_) => None,
        }
    }

    pub(crate) fn set_parent(&mut self, parent: Option<GlaNodeId>) {
        match self {
            Self::Root(branch) | Self::Branch(branch) => branch.shared.parent = parent,
            Self::Leaf(leaf) => leaf.shared.parent = parent,
        }
    }

    pub(crate) fn set_opacity(&mut self, opacity: f32) {
        match self {
            Self::Root(branch) | Self::Branch(branch) => branch.shared.opacity = opacity,
            Self::Leaf(leaf) => leaf.shared.opacity = opacity,
        }
    }

    pub(crate) fn set_blend_mode(&mut self, blend_mode: GlaBlendMode) {
        match self {
            Self::Root(branch) | Self::Branch(branch) => branch.shared.blend_mode = blend_mode,
            Self::Leaf(leaf) => leaf.shared.blend_mode = blend_mode,
        }
    }
}
