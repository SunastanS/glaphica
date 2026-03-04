use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use glaphica_core::{ImageDirtyTracker, NodeId, RenderTreeGeneration, TileKey};
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

pub struct FlatRenderTree {
    pub generation: RenderTreeGeneration,
    pub nodes: Arc<HashMap<NodeId, FlatRenderNode>>,
    pub root_id: Option<NodeId>,
}

impl FlatRenderTree {
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

    pub fn diff_branch_cache_dirty(&self, old: &FlatRenderTree) -> Vec<NodeId> {
        let mut dirty = Vec::new();

        for (node_id, node) in &*self.nodes {
            let old_node = match old.nodes.get(node_id) {
                Some(n) => n,
                None => {
                    if matches!(node.kind, FlatNodeKind::Branch { .. }) {
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
            (FlatNodeKind::Leaf { .. }, FlatNodeKind::Leaf { .. }) => true,
            _ => false,
        }
    }

    fn build_render_cmd(&self, branch_id: NodeId, tile_indices: &[usize]) -> Option<RenderCmd> {
        let branch = self.nodes.get(&branch_id)?;
        let (children, cache) = match &branch.kind {
            FlatNodeKind::Branch { children, cache } => (children, cache),
            FlatNodeKind::Leaf { .. } => return None,
        };

        let mut from: Vec<RenderSource> = Vec::with_capacity(children.len());
        for &child_id in children {
            let child = self.nodes.get(&child_id)?;
            let image = match &child.kind {
                FlatNodeKind::Leaf { image } => image,
                FlatNodeKind::Branch { cache, .. } => cache,
            };

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
            let key = cache.tile_key(idx).unwrap_or(TileKey::EMPTY);
            to.push(key);
        }

        Some(RenderCmd { from, to })
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
    Leaf { image: Image },
    Branch { children: Vec<NodeId>, cache: Image },
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

        // Branch cache
        let mut cache_image = Image::new(layout, BackendId::new(3)).unwrap();
        cache_image
            .set_tile_key(0, TileKey::from_parts(3, 1, 300))
            .unwrap();
        cache_image
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
                    image: child1_image,
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
                    image: child2_image,
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
                    cache: cache_image,
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
}
