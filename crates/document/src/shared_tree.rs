use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use glaphica_core::{CanvasVec2, ImageDirtyTracker, NodeId, RenderTreeGeneration, TileKey};
use images::Image;

use crate::LeafBlendMode;

pub struct RenderSource {
    pub tile_keys: Vec<TileKey>,
    pub config: NodeConfig,
}

pub struct RenderCmd {
    pub from: Vec<RenderSource>,
    pub to: Vec<TileKey>,
}

pub struct MaterializeParametricCmd {
    pub node_id: NodeId,
    pub mesh: ParametricMesh,
    pub tile_indices: Vec<usize>,
    pub tile_origins: Vec<CanvasVec2>,
    pub dst_tile_keys: Vec<TileKey>,
}

pub struct FlatRenderTree {
    pub generation: RenderTreeGeneration,
    pub nodes: Arc<HashMap<NodeId, FlatRenderNode>>,
    pub root_id: Option<NodeId>,
}

impl FlatRenderTree {
    pub fn build_parametric_cmds(
        &self,
        dirty: &ImageDirtyTracker,
    ) -> Vec<MaterializeParametricCmd> {
        let mut groups: HashMap<NodeId, Vec<usize>> = HashMap::new();

        for key in dirty.iter() {
            let Some(node) = self.nodes.get(&key.node_id) else {
                continue;
            };
            let FlatNodeKind::Leaf {
                content: FlatLeafContent::Parametric { .. },
            } = &node.kind
            else {
                continue;
            };
            groups.entry(key.node_id).or_default().push(key.tile_index);
        }

        let mut cmds = Vec::new();
        for (node_id, tile_indices) in groups {
            if let Some(cmd) = self.build_parametric_cmd(node_id, &tile_indices) {
                cmds.push(cmd);
            }
        }
        cmds
    }

    pub fn build_render_cmds(&self, dirty: &ImageDirtyTracker) -> Vec<RenderCmd> {
        let mut groups: HashMap<NodeId, Vec<usize>> = HashMap::new();

        for key in dirty.iter() {
            if let Some(node) = self.nodes.get(&key.node_id) {
                if let Some(parent_id) = node.parent_id {
                    groups.entry(parent_id).or_default().push(key.tile_index);
                }
            }
        }

        let mut cmds = Vec::new();
        for (parent_id, tile_indices) in groups {
            if let Some(cmd) = self.build_render_cmd(parent_id, &tile_indices) {
                cmds.push(cmd);
            }
        }
        cmds
    }

    //TODO: we need somewhat store the render diff while building it.
    pub fn diff_render_cache_dirty(&self, old: &FlatRenderTree) -> Vec<NodeId> {
        let mut dirty = Vec::new();

        for (node_id, node) in &*self.nodes {
            let old_node = match old.nodes.get(node_id) {
                Some(n) => n,
                None => {
                    if node.kind.render_cache().is_some() {
                        dirty.push(*node_id);
                    }
                    continue;
                }
            };

            if !Self::branch_node_equal(node, old_node) {
                dirty.push(*node_id);
            }
        }

        for old_node_id in old.nodes.keys() {
            if !self.nodes.contains_key(old_node_id) {
                dirty.push(*old_node_id);
            }
        }

        dirty
    }

    fn branch_node_equal(a: &FlatRenderNode, b: &FlatRenderNode) -> bool {
        match (&a.kind, &b.kind) {
            (
                FlatNodeKind::Branch {
                    children: a_children,
                    ..
                },
                FlatNodeKind::Branch {
                    children: b_children,
                    ..
                },
            ) => {
                if a_children.len() != b_children.len() {
                    return false;
                }
                if a.config != b.config {
                    return false;
                }
                a_children == b_children
            }
            (
                FlatNodeKind::Leaf {
                    content: FlatLeafContent::Raster { .. },
                },
                FlatNodeKind::Leaf {
                    content: FlatLeafContent::Raster { .. },
                },
            ) => true,
            (
                FlatNodeKind::Leaf {
                    content: FlatLeafContent::Parametric { mesh: a_mesh, .. },
                },
                FlatNodeKind::Leaf {
                    content: FlatLeafContent::Parametric { mesh: b_mesh, .. },
                },
            ) => a_mesh == b_mesh,
            _ => false,
        }
    }

    fn build_render_cmd(&self, branch_id: NodeId, tile_indices: &[usize]) -> Option<RenderCmd> {
        let branch = self.nodes.get(&branch_id)?;
        let (children, render_cache) = match &branch.kind {
            FlatNodeKind::Branch {
                children,
                render_cache,
            } => (children, render_cache),
            FlatNodeKind::Leaf { .. } => return None,
        };

        let mut from: Vec<RenderSource> = Vec::with_capacity(children.len());
        for &child_id in children {
            let child = self.nodes.get(&child_id)?;
            let image = child.kind.render_image()?;

            let mut tile_keys = Vec::with_capacity(tile_indices.len());
            for &idx in tile_indices {
                let key = image.tile_key(idx).unwrap_or(TileKey::EMPTY);
                tile_keys.push(key);
            }

            from.push(RenderSource {
                tile_keys,
                config: child.config,
            });
        }

        let mut to = Vec::with_capacity(tile_indices.len());
        for &idx in tile_indices {
            let key = render_cache.tile_key(idx).unwrap_or(TileKey::EMPTY);
            to.push(key);
        }

        Some(RenderCmd { from, to })
    }

    fn build_parametric_cmd(
        &self,
        node_id: NodeId,
        tile_indices: &[usize],
    ) -> Option<MaterializeParametricCmd> {
        let node = self.nodes.get(&node_id)?;
        let FlatNodeKind::Leaf {
            content: FlatLeafContent::Parametric { mesh, render_cache },
        } = &node.kind
        else {
            return None;
        };

        let mut filtered_indices = Vec::with_capacity(tile_indices.len());
        let mut tile_origins = Vec::with_capacity(tile_indices.len());
        let mut dst_tile_keys = Vec::with_capacity(tile_indices.len());
        for &tile_index in tile_indices {
            let Some(dst_tile_key) = render_cache.tile_key(tile_index) else {
                continue;
            };
            if dst_tile_key == TileKey::EMPTY {
                continue;
            }
            let Some(tile_origin) = render_cache.tile_canvas_origin(tile_index) else {
                continue;
            };
            filtered_indices.push(tile_index);
            tile_origins.push(tile_origin);
            dst_tile_keys.push(dst_tile_key);
        }

        if dst_tile_keys.is_empty() {
            return None;
        }

        Some(MaterializeParametricCmd {
            node_id,
            mesh: mesh.clone(),
            tile_indices: filtered_indices,
            tile_origins,
            dst_tile_keys,
        })
    }
}

#[derive(Clone)]
pub struct FlatRenderNode {
    pub parent_id: Option<NodeId>,
    pub config: NodeConfig,
    pub kind: FlatNodeKind,
}

#[derive(Clone, Copy, PartialEq)]
pub struct NodeConfig {
    pub opacity: f32,
    pub blend_mode: LeafBlendMode,
}

#[derive(Clone)]
pub enum FlatNodeKind {
    Leaf {
        content: FlatLeafContent,
    },
    Branch {
        children: Vec<NodeId>,
        render_cache: Image,
    },
}

#[derive(Clone)]
pub enum FlatLeafContent {
    Raster {
        image: Image,
    },
    Parametric {
        mesh: ParametricMesh,
        render_cache: Image,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParametricMesh {
    pub vertices: Vec<ParametricVertex>,
    pub indices: Vec<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParametricVertex {
    pub position: CanvasVec2,
    pub color: [f32; 4],
}

impl FlatNodeKind {
    pub fn render_image(&self) -> Option<&Image> {
        match self {
            Self::Leaf { content } => content.render_image(),
            Self::Branch { render_cache, .. } => Some(render_cache),
        }
    }

    pub fn render_image_mut(&mut self) -> Option<&mut Image> {
        match self {
            Self::Leaf { content } => content.render_image_mut(),
            Self::Branch { render_cache, .. } => Some(render_cache),
        }
    }

    pub fn render_cache(&self) -> Option<&Image> {
        match self {
            Self::Leaf { content } => content.render_cache(),
            Self::Branch { render_cache, .. } => Some(render_cache),
        }
    }

    pub fn render_cache_mut(&mut self) -> Option<&mut Image> {
        match self {
            Self::Leaf { content } => content.render_cache_mut(),
            Self::Branch { render_cache, .. } => Some(render_cache),
        }
    }
}

impl FlatLeafContent {
    pub fn render_image(&self) -> Option<&Image> {
        match self {
            Self::Raster { image } => Some(image),
            Self::Parametric { render_cache, .. } => Some(render_cache),
        }
    }

    pub fn render_image_mut(&mut self) -> Option<&mut Image> {
        match self {
            Self::Raster { image } => Some(image),
            Self::Parametric { render_cache, .. } => Some(render_cache),
        }
    }

    pub fn render_cache(&self) -> Option<&Image> {
        match self {
            Self::Raster { .. } => None,
            Self::Parametric { render_cache, .. } => Some(render_cache),
        }
    }

    pub fn render_cache_mut(&mut self) -> Option<&mut Image> {
        match self {
            Self::Raster { .. } => None,
            Self::Parametric { render_cache, .. } => Some(render_cache),
        }
    }

    pub fn parametric_mesh(&self) -> Option<&ParametricMesh> {
        match self {
            Self::Raster { .. } => None,
            Self::Parametric { mesh, .. } => Some(mesh),
        }
    }
}

pub struct SharedRenderTree {
    inner: ArcSwap<FlatRenderTree>,
}

impl SharedRenderTree {
    pub fn new(tree: FlatRenderTree) -> Self {
        Self {
            inner: ArcSwap::from_pointee(tree),
        }
    }

    pub fn read(&self) -> Arc<FlatRenderTree> {
        self.inner.load_full()
    }

    pub fn update(&self, tree: FlatRenderTree) {
        self.inner.store(tree.into());
    }

    pub fn generation(&self) -> RenderTreeGeneration {
        self.inner.load().generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glaphica_core::{BackendId, ImageDirtyTracker, RenderTreeGeneration, TileKey};
    use images::layout::ImageLayout;

    #[test]
    fn test_build_render_cmds_with_partial_dirty() {
        // Create a flat render tree with a branch node that has 2 children
        let layout = ImageLayout::new(256, 128); // 2 tiles

        // Child 1 (leaf)
        let mut child1_image = Image::new(layout, BackendId::new(1)).unwrap();
        child1_image
            .set_tile_key(0, TileKey::from_parts(1, 1, 100))
            .unwrap();
        child1_image
            .set_tile_key(1, TileKey::from_parts(1, 1, 101))
            .unwrap();

        // Child 2 (leaf)
        let mut child2_image = Image::new(layout, BackendId::new(2)).unwrap();
        child2_image
            .set_tile_key(0, TileKey::from_parts(2, 1, 200))
            .unwrap();
        child2_image
            .set_tile_key(1, TileKey::from_parts(2, 1, 201))
            .unwrap();

        // Render cache
        let mut render_cache = Image::new(layout, BackendId::new(3)).unwrap();
        render_cache
            .set_tile_key(0, TileKey::from_parts(3, 1, 300))
            .unwrap();
        render_cache
            .set_tile_key(1, TileKey::from_parts(3, 1, 301))
            .unwrap();

        let mut nodes = HashMap::new();

        // Child nodes
        nodes.insert(
            NodeId(1),
            FlatRenderNode {
                parent_id: Some(NodeId(100)),
                config: NodeConfig {
                    opacity: 1.0,
                    blend_mode: LeafBlendMode::Normal,
                },
                kind: FlatNodeKind::Leaf {
                    content: FlatLeafContent::Raster {
                        image: child1_image,
                    },
                },
            },
        );

        nodes.insert(
            NodeId(2),
            FlatRenderNode {
                parent_id: Some(NodeId(100)),
                config: NodeConfig {
                    opacity: 0.5,
                    blend_mode: LeafBlendMode::Normal,
                },
                kind: FlatNodeKind::Leaf {
                    content: FlatLeafContent::Raster {
                        image: child2_image,
                    },
                },
            },
        );

        // Branch node
        nodes.insert(
            NodeId(100),
            FlatRenderNode {
                parent_id: None,
                config: NodeConfig {
                    opacity: 1.0,
                    blend_mode: LeafBlendMode::Normal,
                },
                kind: FlatNodeKind::Branch {
                    children: vec![NodeId(1), NodeId(2)],
                    render_cache,
                },
            },
        );

        let tree = FlatRenderTree {
            generation: RenderTreeGeneration(0),
            nodes: Arc::new(nodes),
            root_id: Some(NodeId(100)),
        };

        // Test 1: Only tile 0 is dirty
        let mut dirty_tracker = ImageDirtyTracker::default();
        dirty_tracker.mark(NodeId(1), 0); // Only child1 tile 0

        let cmds = tree.build_render_cmds(&dirty_tracker);

        assert_eq!(cmds.len(), 1, "Should have 1 render cmd for the branch");
        assert_eq!(cmds[0].to.len(), 1, "Should composite only 1 tile");
        assert_eq!(cmds[0].from.len(), 2, "Should have 2 source layers");
        assert_eq!(
            cmds[0].from[0].tile_keys.len(),
            1,
            "Source 1 should have 1 tile key"
        );
        assert_eq!(
            cmds[0].from[1].tile_keys.len(),
            1,
            "Source 2 should have 1 tile key"
        );

        // Test 2: Both tiles are dirty
        let mut dirty_tracker = ImageDirtyTracker::default();
        dirty_tracker.mark(NodeId(1), 0);
        dirty_tracker.mark(NodeId(1), 1);

        let cmds = tree.build_render_cmds(&dirty_tracker);

        assert_eq!(cmds.len(), 1, "Should have 1 render cmd for the branch");
        assert_eq!(cmds[0].to.len(), 2, "Should composite 2 tiles");
        assert_eq!(
            cmds[0].from[0].tile_keys.len(),
            2,
            "Source 1 should have 2 tile keys"
        );
        assert_eq!(
            cmds[0].from[1].tile_keys.len(),
            2,
            "Source 2 should have 2 tile keys"
        );
    }

    #[test]
    fn test_raster_dirty_under_parametric_background_does_not_materialize_background() {
        let layout = ImageLayout::new(256, 128);

        let mut background_cache = Image::new(layout, BackendId::new(1)).unwrap();
        background_cache
            .set_tile_key(0, TileKey::from_parts(1, 0, 10))
            .unwrap();
        background_cache
            .set_tile_key(1, TileKey::from_parts(1, 0, 11))
            .unwrap();

        let background_mesh = ParametricMesh {
            vertices: vec![
                ParametricVertex {
                    position: glaphica_core::CanvasVec2::new(0.0, 0.0),
                    color: [1.0, 1.0, 1.0, 1.0],
                },
                ParametricVertex {
                    position: glaphica_core::CanvasVec2::new(256.0, 0.0),
                    color: [1.0, 1.0, 1.0, 1.0],
                },
                ParametricVertex {
                    position: glaphica_core::CanvasVec2::new(0.0, 128.0),
                    color: [1.0, 1.0, 1.0, 1.0],
                },
                ParametricVertex {
                    position: glaphica_core::CanvasVec2::new(256.0, 128.0),
                    color: [1.0, 1.0, 1.0, 1.0],
                },
            ],
            indices: vec![0, 1, 2, 2, 1, 3],
        };

        let mut raster_image = Image::new(layout, BackendId::new(2)).unwrap();
        raster_image
            .set_tile_key(0, TileKey::from_parts(2, 0, 20))
            .unwrap();
        raster_image
            .set_tile_key(1, TileKey::from_parts(2, 0, 21))
            .unwrap();

        let mut root_cache = Image::new(layout, BackendId::new(3)).unwrap();
        root_cache
            .set_tile_key(0, TileKey::from_parts(3, 0, 30))
            .unwrap();
        root_cache
            .set_tile_key(1, TileKey::from_parts(3, 0, 31))
            .unwrap();

        let mut nodes = HashMap::new();
        nodes.insert(
            NodeId(1),
            FlatRenderNode {
                parent_id: Some(NodeId(100)),
                config: NodeConfig {
                    opacity: 1.0,
                    blend_mode: LeafBlendMode::Normal,
                },
                kind: FlatNodeKind::Leaf {
                    content: FlatLeafContent::Parametric {
                        mesh: background_mesh,
                        render_cache: background_cache,
                    },
                },
            },
        );
        nodes.insert(
            NodeId(2),
            FlatRenderNode {
                parent_id: Some(NodeId(100)),
                config: NodeConfig {
                    opacity: 1.0,
                    blend_mode: LeafBlendMode::Normal,
                },
                kind: FlatNodeKind::Leaf {
                    content: FlatLeafContent::Raster { image: raster_image },
                },
            },
        );
        nodes.insert(
            NodeId(100),
            FlatRenderNode {
                parent_id: None,
                config: NodeConfig {
                    opacity: 1.0,
                    blend_mode: LeafBlendMode::Normal,
                },
                kind: FlatNodeKind::Branch {
                    children: vec![NodeId(1), NodeId(2)],
                    render_cache: root_cache,
                },
            },
        );

        let tree = FlatRenderTree {
            generation: RenderTreeGeneration(0),
            nodes: Arc::new(nodes),
            root_id: Some(NodeId(100)),
        };

        let mut dirty_tracker = ImageDirtyTracker::default();
        dirty_tracker.mark(NodeId(2), 0);
        dirty_tracker.mark(NodeId(2), 1);

        let parametric_cmds = tree.build_parametric_cmds(&dirty_tracker);
        let render_cmds = tree.build_render_cmds(&dirty_tracker);

        assert!(
            parametric_cmds.is_empty(),
            "raster-only dirty should not re-materialize parametric background"
        );
        assert_eq!(render_cmds.len(), 1, "should only composite the root branch");
        assert_eq!(render_cmds[0].to.len(), 2, "should composite only dirty raster tiles");
        assert_eq!(render_cmds[0].from.len(), 2, "root should still see both child layers");
        assert_eq!(
            render_cmds[0].from[0].tile_keys,
            vec![TileKey::from_parts(1, 0, 10), TileKey::from_parts(1, 0, 11)],
            "background should be read from its stable render cache"
        );
        assert_eq!(
            render_cmds[0].from[1].tile_keys,
            vec![TileKey::from_parts(2, 0, 20), TileKey::from_parts(2, 0, 21)],
            "foreground raster tiles should be read directly from the raster image"
        );
    }
}
