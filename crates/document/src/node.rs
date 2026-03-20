use std::sync::Arc;

use glaphica_core::{CanvasVec2, NodeId};
use images::Image;
use images::layout::ImageLayout;

use crate::shared_tree::{ParametricMesh, ParametricVertex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiNodeKind {
    Branch,
    RasterLayer,
    SpecialLayer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiBlendMode {
    Normal,
    Multiply,
    Penetrate,
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

    pub(crate) fn meta(&self) -> &UiNodeMeta {
        match self {
            Self::Branch(branch) => &branch.meta,
            Self::Leaf(leaf) => &leaf.meta,
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct UiNodeMeta {
    pub(crate) id: NodeId,
    pub(crate) label: String,
    pub(crate) visible: bool,
}

#[derive(Clone, PartialEq)]
pub struct UiBranchNode {
    pub(crate) meta: UiNodeMeta,
    pub(crate) config: BranchConfig,
    pub(crate) children: Vec<UiLayerNode>,
}

#[derive(Clone, PartialEq)]
pub struct UiLeafNode {
    pub(crate) meta: UiNodeMeta,
    pub(crate) config: LeafConfig,
    pub(crate) content: UiLeafContent,
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
    pub(crate) fn to_parametric_mesh(&self, layout: ImageLayout) -> ParametricMesh {
        match self {
            Self::SolidColor(layer) => layer.to_parametric_mesh(layout),
        }
    }

    pub(crate) fn set_solid_color(&mut self, color: [f32; 4]) -> bool {
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
    pub(crate) color: [f32; 4],
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
    pub(crate) id: NodeId,
    pub(crate) config: BranchConfig,
    pub(crate) children: Vec<RenderLayerNode>,
    pub(crate) render_cache: Image,
}

#[derive(Clone, PartialEq)]
pub struct RenderLeafNode {
    pub(crate) id: NodeId,
    pub(crate) config: LeafConfig,
    pub(crate) content: RenderLeafContent,
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
    pub(crate) opacity: f32,
    pub(crate) blend_mode: LeafBlendMode,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LeafBlendMode {
    Normal,
    Multiply,
}

#[derive(Clone, PartialEq)]
pub struct BranchConfig {
    pub(crate) opacity: f32,
    pub(crate) blend_mode: BranchBlendMode,
}

#[derive(Clone, Copy, PartialEq)]
pub enum BranchBlendMode {
    Base(LeafBlendMode),
    Penetrate,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiLayerTreeItem {
    pub id: NodeId,
    pub label: String,
    pub visible: bool,
    pub opacity: f32,
    pub blend_mode: UiBlendMode,
    pub kind: UiNodeKind,
    pub solid_color: Option<[f32; 4]>,
    pub children: Vec<UiLayerTreeItem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayerMoveTarget {
    pub parent_id: NodeId,
    pub index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NewLayerKind {
    Raster,
    SolidColor { color: [f32; 4] },
}

pub fn ui_blend_mode_from_leaf(blend_mode: LeafBlendMode) -> UiBlendMode {
    match blend_mode {
        LeafBlendMode::Normal => UiBlendMode::Normal,
        LeafBlendMode::Multiply => UiBlendMode::Multiply,
    }
}

pub fn ui_blend_mode_from_branch(blend_mode: BranchBlendMode) -> UiBlendMode {
    match blend_mode {
        BranchBlendMode::Base(mode) => ui_blend_mode_from_leaf(mode),
        BranchBlendMode::Penetrate => UiBlendMode::Penetrate,
    }
}

pub fn leaf_blend_mode_from_ui(blend_mode: UiBlendMode) -> Option<LeafBlendMode> {
    match blend_mode {
        UiBlendMode::Normal => Some(LeafBlendMode::Normal),
        UiBlendMode::Multiply => Some(LeafBlendMode::Multiply),
        UiBlendMode::Penetrate => None,
    }
}

pub fn branch_blend_mode_from_ui(blend_mode: UiBlendMode) -> BranchBlendMode {
    match blend_mode {
        UiBlendMode::Normal => BranchBlendMode::Base(LeafBlendMode::Normal),
        UiBlendMode::Multiply => BranchBlendMode::Base(LeafBlendMode::Multiply),
        UiBlendMode::Penetrate => BranchBlendMode::Penetrate,
    }
}
