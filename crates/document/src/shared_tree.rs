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

pub struct FlatRenderNode {
    pub parent_id: Option<NodeId>,
    pub config: NodeConfig,
    pub kind: FlatNodeKind,
}

#[derive(Clone, Copy)]
pub struct NodeConfig {
    pub opacity: f32,
    pub blend_mode: LeafBlendMode,
}

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
