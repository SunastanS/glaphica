use glaphica_core::{BackendId, ImageDirtyTracker, NodeId, RenderTreeGeneration};
use images::layout::ImageLayout;
use images::Image;
use images::ImageCreateError;

use crate::layer_tree::{collect_raster_tile_keys_from_node, UiLayerTree};
use crate::node::{
    BranchBlendMode, BranchConfig, LayerMoveTarget, LeafBlendMode, LeafConfig, NewLayerKind,
    SolidColorLayer, SpecialLayer, UiBlendMode, UiBranchNode, UiLayerNode, UiLayerTreeItem,
    UiLeafContent, UiLeafNode, UiNodeMeta,
};
use crate::render_lowering::{infer_render_nodes, RenderLayerTree};
use crate::shared_tree::{FlatLeafContent, FlatNodeKind, FlatRenderTree};

pub struct Document {
    pub layer_tree: UiLayerTree,
    pub(crate) layout: ImageLayout,
    pub(crate) metadata: Metadata,
    pub(crate) leaf_backend: BackendId,
    pub(crate) render_cache_backend: BackendId,
    pub(crate) next_node_id: NodeId,
    pub(crate) next_layer_label_index: u64,
    pub(crate) next_group_label_index: u64,
    pub(crate) active_node: Option<NodeId>,
}

pub struct Metadata {
    name: String,
}

impl Metadata {
    pub fn new(name: String) -> Self {
        Self { name }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Debug)]
pub enum LayerEditError {
    NoActiveNode,
    InvalidNode,
    InvalidBlendModeForLeaf(UiBlendMode),
    RootSelectionNotAllowed,
    MoveOutOfBounds,
    ImageCreate(ImageCreateError),
}

impl From<ImageCreateError> for LayerEditError {
    fn from(err: ImageCreateError) -> Self {
        Self::ImageCreate(err)
    }
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
        let root = UiLayerNode::Branch(UiBranchNode {
            meta: UiNodeMeta {
                id: root_id,
                label: "Root".to_string(),
                visible: true,
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
                        visible: true,
                    },
                    config: LeafConfig {
                        opacity: 1.0,
                        blend_mode: LeafBlendMode::Normal,
                    },
                    content: UiLeafContent::Special(SpecialLayer::SolidColor(SolidColorLayer {
                        color: [1.0, 1.0, 1.0, 1.0],
                    })),
                }),
                UiLayerNode::Leaf(UiLeafNode {
                    meta: UiNodeMeta {
                        id: paint_layer_id,
                        label: "Layer 2".to_string(),
                        visible: true,
                    },
                    config: LeafConfig {
                        opacity: 1.0,
                        blend_mode: LeafBlendMode::Normal,
                    },
                    content: UiLeafContent::Raster { image },
                }),
            ],
        });
        let layer_tree = UiLayerTree { root };
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
        let root = UiLayerNode::Leaf(UiLeafNode {
            meta: UiNodeMeta {
                id: initial_id,
                label: "Layer 1".to_string(),
                visible: true,
            },
            config: LeafConfig {
                opacity: 1.0,
                blend_mode: LeafBlendMode::Normal,
            },
            content: UiLeafContent::Special(SpecialLayer::SolidColor(SolidColorLayer { color })),
        });
        let layer_tree = UiLayerTree { root };
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
        let layout = self.layout;
        let rendered_nodes = infer_render_nodes(
            &self.layer_tree.root,
            1.0,
            true,
            self.leaf_backend,
            self.render_cache_backend,
            layout,
        )?;
        let root = rendered_nodes.into_iter().next().unwrap_or_else(|| {
            crate::RenderLayerNode::Leaf(crate::node::RenderLeafNode {
                id: NodeId(u64::MAX),
                config: LeafConfig {
                    opacity: 1.0,
                    blend_mode: LeafBlendMode::Normal,
                },
                content: crate::node::RenderLeafContent::Raster {
                    image: Image::new(layout, self.leaf_backend)
                        .expect("fallback image creation should succeed"),
                },
            })
        });
        let render_tree = RenderLayerTree { root };
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
            meta: UiNodeMeta {
                id: node_id,
                label,
                visible: true,
            },
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

    pub fn node_opacity(&self, node_id: NodeId) -> Option<f32> {
        self.layer_tree.node_opacity(node_id)
    }

    pub fn node_blend_mode(&self, node_id: NodeId) -> Option<UiBlendMode> {
        self.layer_tree.node_blend_mode(node_id)
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

    pub fn set_node_visibility(
        &mut self,
        node_id: NodeId,
        visible: bool,
    ) -> Result<ImageDirtyTracker, LayerEditError> {
        self.layer_tree.set_node_visibility(node_id, visible)?;

        let mut dirty = ImageDirtyTracker::default();
        for tile_index in 0..self.layout.total_tiles() as usize {
            dirty.mark(node_id, tile_index);
        }
        Ok(dirty)
    }

    pub fn set_node_opacity(
        &mut self,
        node_id: NodeId,
        opacity: f32,
    ) -> Result<(), LayerEditError> {
        self.layer_tree.set_node_opacity(node_id, opacity)
    }

    pub fn set_node_blend_mode(
        &mut self,
        node_id: NodeId,
        blend_mode: UiBlendMode,
    ) -> Result<(), LayerEditError> {
        self.layer_tree.set_node_blend_mode(node_id, blend_mode)
    }

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
                    FlatNodeKind::Branch { render_cache, .. } => render_cache,
                };
                if image.set_tile_key(*tile_index, new_tile_key).is_err() {
                    continue;
                }
            }
        }

        FlatRenderTree {
            generation: new_generation,
            nodes: std::sync::Arc::new(new_nodes),
            root_id: tree.root_id,
        }
    }

    pub fn allocate_node_id(&mut self) -> NodeId {
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
            meta: UiNodeMeta {
                id,
                label,
                visible: true,
            },
            config: LeafConfig {
                opacity: 1.0,
                blend_mode: LeafBlendMode::Normal,
            },
            content,
        }))
    }
}
