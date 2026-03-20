use glaphica_core::{BackendId, ImageDirtyTracker, NodeId, RenderTreeGeneration};
use images::Image;
use images::ImageCreateError;
use images::layout::ImageLayout;

use crate::layer_tree::{UiLayerTree, collect_raster_tile_keys_from_node};
use crate::node::{
    BranchBlendMode, BranchConfig, LayerMoveTarget, LeafBlendMode, LeafConfig, NewLayerKind,
    SolidColorLayer, SpecialLayer, UiBlendMode, UiBranchNode, UiLayerNode, UiLayerTreeItem,
    UiLeafContent, UiLeafNode, UiNodeMeta,
};
use crate::render_lowering::{RenderLayerTree, infer_render_nodes};
use crate::shared_tree::{FlatLeafContent, FlatNodeKind, FlatRenderTree};

pub struct Document {
    pub(crate) layer_tree: UiLayerTree,
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
            crate::node::RenderLayerNode::Leaf(crate::node::RenderLeafNode {
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
                        FlatLeafContent::Parametric { .. } => continue,
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer_tree::UiLayerTree;
    use crate::node::{
        BranchBlendMode, BranchConfig, LeafBlendMode, LeafConfig, RenderLayerNode,
        RenderLeafContent, RenderLeafNode, SolidColorLayer, SpecialLayer, UiBranchNode,
        UiLayerNode, UiLeafContent, UiLeafNode, UiNodeKind, UiNodeMeta,
    };
    use crate::render_lowering::RenderLayerTree;
    use crate::shared_tree::{
        FlatLeafContent, FlatNodeKind, FlatRenderNode, FlatRenderTree, NodeConfig,
    };
    use crate::{ParametricMesh, ParametricVertex};
    use glaphica_core::{BackendId, NodeId, RenderTreeGeneration, TileKey};
    use images::Image;
    use images::layout::ImageLayout;
    use std::sync::Arc;

    fn test_meta(id: NodeId, label: &str) -> UiNodeMeta {
        UiNodeMeta {
            id,
            label: label.to_string(),
            visible: true,
        }
    }

    #[test]
    fn test_sync_tile_keys_partial_update_preserves_untouched_tiles() {
        let layout = ImageLayout::new(256, 128);

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

        let root = UiLayerNode::Leaf(leaf_node);
        let layer_tree = UiLayerTree { root };

        let mut doc = Document {
            layer_tree,
            layout,
            metadata: Metadata::new("test".to_string()),
            leaf_backend: BackendId::new(1),
            render_cache_backend: BackendId::new(2),
            next_node_id: NodeId(1),
            next_layer_label_index: 2,
            next_group_label_index: 1,
            active_node: None,
        };

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

        doc.get_leaf_image_mut(NodeId(0))
            .unwrap()
            .set_tile_key(1, TileKey::from_parts(1, 1, 201))
            .unwrap();

        let updates = vec![(NodeId(0), 1)];

        let new_tree =
            doc.sync_tile_keys_to_flat_tree(&old_tree, &updates, RenderTreeGeneration(1));

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

        assert_eq!(
            new_image.tile_key(0),
            Some(TileKey::from_parts(1, 1, 100)),
            "Tile 0 should preserve original key from old tree"
        );

        assert_eq!(
            new_image.tile_key(1),
            Some(TileKey::from_parts(1, 1, 201)),
            "Tile 1 should have updated key from document"
        );
    }

    #[test]
    fn test_flatten_preserves_parametric_mesh() {
        let mesh = Arc::new(ParametricMesh {
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
        });

        let tree = RenderLayerTree {
            root: RenderLayerNode::Leaf(RenderLeafNode {
                id: NodeId(7),
                config: LeafConfig {
                    opacity: 1.0,
                    blend_mode: LeafBlendMode::Normal,
                },
                content: RenderLeafContent::Parametric { mesh: mesh.clone() },
            }),
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
        let root = UiLayerNode::Leaf(UiLeafNode {
            meta: test_meta(NodeId(9), "Layer 1"),
            config: LeafConfig {
                opacity: 1.0,
                blend_mode: LeafBlendMode::Normal,
            },
            content: UiLeafContent::Special(SpecialLayer::SolidColor(SolidColorLayer {
                color: [0.2, 0.4, 0.6, 1.0],
            })),
        });
        let tree = UiLayerTree { root };

        let rendered_nodes = infer_render_nodes(
            &tree.root,
            1.0,
            true,
            BackendId::new(1),
            BackendId::new(2),
            layout,
        )
        .unwrap();
        let root = rendered_nodes.into_iter().next().unwrap();
        let flat = RenderLayerTree { root }.flatten(RenderTreeGeneration(2));
        let node = flat.nodes.get(&NodeId(9)).unwrap();

        let mesh = match &node.kind {
            FlatNodeKind::Leaf {
                content: FlatLeafContent::Parametric { mesh },
            } => mesh,
            FlatNodeKind::Leaf {
                content: FlatLeafContent::Raster { .. },
            }
            | FlatNodeKind::Branch { .. } => {
                panic!("expected solid color leaf to lower to parametric")
            }
        };

        assert_eq!(mesh.vertices.len(), 4);
        assert_eq!(mesh.indices, vec![0, 1, 2, 2, 1, 3]);
        assert_eq!(
            mesh.vertices[0].position,
            glaphica_core::CanvasVec2::new(0.0, 0.0)
        );
        assert_eq!(
            mesh.vertices[3].position,
            glaphica_core::CanvasVec2::new(128.0, 64.0)
        );
        assert_eq!(mesh.vertices[0].color, [0.2, 0.4, 0.6, 1.0]);
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
        use crate::layer_tree::get_node_from_node_mut;

        let layout = ImageLayout::new(64, 64);
        let mut doc = Document::new(
            "default".to_string(),
            layout,
            BackendId::new(1),
            BackendId::new(2),
        )
        .unwrap();

        let parent_group_id = doc.create_group_above_active().unwrap();
        let child_group_id = NodeId(4);
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
        assert!(items[0].visible);
        assert_eq!(items[0].children.len(), 2);
        assert_eq!(items[0].children[0].label, "Layer 1");
        assert!(items[0].children[0].visible);
        assert_eq!(items[0].children[0].kind, UiNodeKind::SpecialLayer);
        assert_eq!(items[0].children[1].label, "Layer 2");
        assert!(items[0].children[1].visible);
        assert_eq!(items[0].children[1].kind, UiNodeKind::RasterLayer);
    }

    #[test]
    fn test_hidden_node_is_omitted_from_render_tree() {
        let layout = ImageLayout::new(64, 64);
        let mut doc = Document::new(
            "default".to_string(),
            layout,
            BackendId::new(1),
            BackendId::new(2),
        )
        .unwrap();

        let dirty = doc
            .set_node_visibility(NodeId(1), false)
            .expect("layer should exist");
        let flat = doc.build_flat_render_tree(RenderTreeGeneration(4)).unwrap();

        assert_eq!(dirty.iter().count(), layout.total_tiles() as usize);
        assert!(!flat.nodes.contains_key(&NodeId(1)));
        assert!(flat.nodes.contains_key(&NodeId(0)));
    }

    #[test]
    fn test_set_node_opacity_updates_tree_items_and_flat_render_tree() {
        let layout = ImageLayout::new(64, 64);
        let mut doc = Document::new(
            "default".to_string(),
            layout,
            BackendId::new(1),
            BackendId::new(2),
        )
        .unwrap();

        doc.set_node_opacity(NodeId(1), 0.35).unwrap();
        assert_eq!(doc.node_opacity(NodeId(1)), Some(0.35));

        let items = doc.layer_tree_items();
        assert_eq!(items[0].children[1].opacity, 0.35);

        let flat = doc.build_flat_render_tree(RenderTreeGeneration(5)).unwrap();
        let node = flat.nodes.get(&NodeId(1)).unwrap();
        assert_eq!(node.config.opacity, 0.35);
    }

    #[test]
    fn test_set_node_blend_mode_updates_tree_items_and_flat_render_tree() {
        let layout = ImageLayout::new(64, 64);
        let mut doc = Document::new(
            "default".to_string(),
            layout,
            BackendId::new(1),
            BackendId::new(2),
        )
        .unwrap();

        doc.set_node_blend_mode(NodeId(1), UiBlendMode::Multiply)
            .unwrap();
        assert_eq!(doc.node_blend_mode(NodeId(1)), Some(UiBlendMode::Multiply));

        let items = doc.layer_tree_items();
        assert_eq!(items[0].children[1].blend_mode, UiBlendMode::Multiply);

        let flat = doc.build_flat_render_tree(RenderTreeGeneration(6)).unwrap();
        let node = flat.nodes.get(&NodeId(1)).unwrap();
        assert_eq!(node.config.blend_mode, LeafBlendMode::Multiply);
    }

    #[test]
    fn test_set_node_blend_mode_reports_invalid_leaf_mode() {
        let layout = ImageLayout::new(64, 64);
        let mut doc = Document::new(
            "default".to_string(),
            layout,
            BackendId::new(1),
            BackendId::new(2),
        )
        .unwrap();

        let result = doc.set_node_blend_mode(NodeId(1), UiBlendMode::Penetrate);

        assert!(matches!(
            result,
            Err(LayerEditError::InvalidBlendModeForLeaf(
                UiBlendMode::Penetrate
            ))
        ));
    }
}
