use glaphica_core::{BlendMode, CanvasVec2};

use crate::{GlaDoc, GlaDocError, GlaNodeId, GlaNodeKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlaRenderRefreshKind {
    Full,
    Incremental,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlaRenderTarget {
    RootImage,
    BranchCache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlaRenderSource {
    NodeImage(GlaNodeId),
    NodeCache(GlaNodeId),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlaCompositeCommand {
    pub source: GlaRenderSource,
    pub opacity: f32,
    pub blend_mode: BlendMode,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GlaRenderPass {
    pub node_id: GlaNodeId,
    pub target: GlaRenderTarget,
    pub tile_indices: Vec<usize>,
    pub commands: Vec<GlaCompositeCommand>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GlaRenderRefresh {
    pub kind: GlaRenderRefreshKind,
    pub tile_indices: Vec<usize>,
    pub root_source: GlaRenderSource,
    pub cache_nodes: Vec<GlaNodeId>,
    pub passes: Vec<GlaRenderPass>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct LoweredInput {
    source: GlaRenderSource,
    opacity: f32,
    blend_mode: BlendMode,
}

impl GlaDoc {
    pub fn build_full_render_refresh(&self) -> Result<GlaRenderRefresh, GlaDocError> {
        let tile_indices = (0..self.node(self.root_id())?.image().tile_count()).collect::<Vec<_>>();
        self.build_render_refresh(GlaRenderRefreshKind::Full, tile_indices, None)
    }

    pub fn build_active_layer_incremental_refresh(
        &self,
        dirty_tile_indices: &[usize],
    ) -> Result<Option<GlaRenderRefresh>, GlaDocError> {
        let tile_indices = self.normalize_tile_indices(dirty_tile_indices)?;
        if tile_indices.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.build_render_refresh(
            GlaRenderRefreshKind::Incremental,
            tile_indices,
            Some(self.active_layer_ancestor_chain()),
        )?))
    }

    pub fn build_active_layer_incremental_refresh_for_region(
        &self,
        center: CanvasVec2,
        max_affected_radius_px: u32,
    ) -> Result<Option<GlaRenderRefresh>, GlaDocError> {
        let mut tile_indices = Vec::new();
        self.layout().collect_affected_tile_indices(
            center,
            max_affected_radius_px,
            &mut tile_indices,
        );
        self.build_active_layer_incremental_refresh(&tile_indices)
    }

    fn build_render_refresh(
        &self,
        kind: GlaRenderRefreshKind,
        tile_indices: Vec<usize>,
        incremental_chain: Option<&[GlaNodeId]>,
    ) -> Result<GlaRenderRefresh, GlaDocError> {
        match incremental_chain {
            Some(chain) => self.build_incremental_render_refresh(kind, tile_indices, chain),
            None => self.build_full_render_refresh_from_tiles(kind, tile_indices),
        }
    }

    fn build_full_render_refresh_from_tiles(
        &self,
        kind: GlaRenderRefreshKind,
        tile_indices: Vec<usize>,
    ) -> Result<GlaRenderRefresh, GlaDocError> {
        let mut passes = Vec::new();
        let lowered_root =
            self.lower_full_render_node(self.root_id(), 1.0, true, &tile_indices, &mut passes)?;
        let root_source = if passes
            .last()
            .is_some_and(|pass| pass.node_id == self.root_id())
        {
            GlaRenderSource::NodeImage(self.root_id())
        } else {
            lowered_root
                .first()
                .map(|input| input.source)
                .unwrap_or(GlaRenderSource::NodeImage(self.root_id()))
        };
        let cache_nodes = collect_cache_nodes(&passes);

        Ok(GlaRenderRefresh {
            kind,
            tile_indices,
            root_source,
            cache_nodes,
            passes,
        })
    }

    fn build_incremental_render_refresh(
        &self,
        kind: GlaRenderRefreshKind,
        tile_indices: Vec<usize>,
        incremental_chain: &[GlaNodeId],
    ) -> Result<GlaRenderRefresh, GlaDocError> {
        let active_layer_id = *incremental_chain
            .first()
            .ok_or(GlaDocError::InvalidNodeId(self.active_layer_id()))?;
        let mut passes = Vec::new();
        let root_source = self.lower_incremental_render_node(
            self.root_id(),
            active_layer_id,
            1.0,
            true,
            &tile_indices,
            &mut passes,
        )?;
        let cache_nodes = collect_cache_nodes(&passes);

        Ok(GlaRenderRefresh {
            kind,
            tile_indices,
            root_source,
            cache_nodes,
            passes,
        })
    }

    fn lower_full_render_node(
        &self,
        node_id: GlaNodeId,
        parent_opacity: f32,
        is_bottom: bool,
        tile_indices: &[usize],
        passes: &mut Vec<GlaRenderPass>,
    ) -> Result<Vec<LoweredInput>, GlaDocError> {
        let node = self.node(node_id)?;
        match node.kind() {
            GlaNodeKind::Leaf => Ok(vec![LoweredInput {
                source: GlaRenderSource::NodeImage(node_id),
                opacity: parent_opacity * node.opacity(),
                blend_mode: if is_bottom {
                    BlendMode::Normal
                } else {
                    node.blend_mode()
                },
            }]),
            GlaNodeKind::Root | GlaNodeKind::Branch => {
                let children = node
                    .children()
                    .ok_or(GlaDocError::CannotInsertIntoLeaf(node_id))?;
                let mut lowered_children = Vec::new();
                for (index, &child_id) in children.iter().enumerate() {
                    lowered_children.extend(self.lower_full_render_node(
                        child_id,
                        1.0,
                        index == 0,
                        tile_indices,
                        passes,
                    )?);
                }

                if lowered_children.is_empty() {
                    return Ok(Vec::new());
                }

                let combined = GlaCompositeCommand {
                    source: GlaRenderSource::NodeImage(node_id),
                    opacity: parent_opacity * node.opacity(),
                    blend_mode: if is_bottom {
                        BlendMode::Normal
                    } else {
                        node.blend_mode()
                    },
                };

                if lowered_children.len() == 1 {
                    let mut child = lowered_children[0];
                    child.opacity *= combined.opacity;
                    child.blend_mode = combined.blend_mode;
                    return Ok(vec![child]);
                }

                passes.push(GlaRenderPass {
                    node_id,
                    target: if matches!(node.kind(), GlaNodeKind::Root) {
                        GlaRenderTarget::RootImage
                    } else {
                        GlaRenderTarget::BranchCache
                    },
                    tile_indices: tile_indices.to_vec(),
                    commands: lowered_children
                        .into_iter()
                        .map(|child| GlaCompositeCommand {
                            source: child.source,
                            opacity: child.opacity,
                            blend_mode: child.blend_mode,
                        })
                        .collect(),
                });

                Ok(vec![LoweredInput {
                    source: if matches!(node.kind(), GlaNodeKind::Root) {
                        GlaRenderSource::NodeImage(node_id)
                    } else {
                        GlaRenderSource::NodeCache(node_id)
                    },
                    opacity: combined.opacity,
                    blend_mode: combined.blend_mode,
                }])
            }
        }
    }

    fn lower_incremental_render_node(
        &self,
        node_id: GlaNodeId,
        active_layer_id: GlaNodeId,
        parent_opacity: f32,
        is_bottom: bool,
        tile_indices: &[usize],
        passes: &mut Vec<GlaRenderPass>,
    ) -> Result<GlaRenderSource, GlaDocError> {
        let node = self.node(node_id)?;
        if matches!(node.kind(), GlaNodeKind::Leaf) {
            return Ok(GlaRenderSource::NodeImage(node_id));
        }

        let children = node
            .children()
            .ok_or(GlaDocError::CannotInsertIntoLeaf(node_id))?;
        let active_child_index = children
            .iter()
            .position(|&child_id| self.is_ancestor(child_id, active_layer_id).unwrap_or(false));

        let Some(active_child_index) = active_child_index else {
            return Ok(GlaRenderSource::NodeCache(node_id));
        };

        let mut commands = Vec::new();
        for (index, &child_id) in children.iter().enumerate() {
            let child_source = if index == active_child_index {
                self.lower_incremental_render_node(
                    child_id,
                    active_layer_id,
                    1.0,
                    index == 0,
                    tile_indices,
                    passes,
                )?
            } else {
                self.lower_cached_subtree_source(child_id)?
            };

            let child = self.node(child_id)?;
            commands.push(GlaCompositeCommand {
                source: child_source,
                opacity: child.opacity(),
                blend_mode: if index == 0 {
                    BlendMode::Normal
                } else {
                    child.blend_mode()
                },
            });
        }

        if commands.len() == 1 {
            return Ok(commands[0].source);
        }

        passes.push(GlaRenderPass {
            node_id,
            target: if matches!(node.kind(), GlaNodeKind::Root) {
                GlaRenderTarget::RootImage
            } else {
                GlaRenderTarget::BranchCache
            },
            tile_indices: tile_indices.to_vec(),
            commands,
        });

        if matches!(node.kind(), GlaNodeKind::Root) {
            let _ = parent_opacity;
            let _ = is_bottom;
            Ok(GlaRenderSource::NodeImage(node_id))
        } else {
            Ok(GlaRenderSource::NodeCache(node_id))
        }
    }

    fn lower_cached_subtree_source(
        &self,
        node_id: GlaNodeId,
    ) -> Result<GlaRenderSource, GlaDocError> {
        let node = self.node(node_id)?;
        match node.kind() {
            GlaNodeKind::Leaf => Ok(GlaRenderSource::NodeImage(node_id)),
            GlaNodeKind::Root | GlaNodeKind::Branch => {
                let children = node
                    .children()
                    .ok_or(GlaDocError::CannotInsertIntoLeaf(node_id))?;
                if children.len() == 1 {
                    return self.lower_cached_subtree_source(children[0]);
                }
                Ok(GlaRenderSource::NodeCache(node_id))
            }
        }
    }

    fn normalize_tile_indices(&self, tile_indices: &[usize]) -> Result<Vec<usize>, GlaDocError> {
        let tile_count = self.node(self.root_id())?.image().tile_count();
        let mut normalized = tile_indices.to_vec();
        normalized.sort_unstable();
        normalized.dedup();

        for &tile_index in &normalized {
            if tile_index >= tile_count {
                return Err(GlaDocError::InvalidTileIndex {
                    tile_index,
                    tile_count,
                });
            }
        }

        Ok(normalized)
    }
}

fn collect_cache_nodes(passes: &[GlaRenderPass]) -> Vec<GlaNodeId> {
    let mut cache_nodes = Vec::new();
    for pass in passes {
        if matches!(pass.target, GlaRenderTarget::BranchCache) {
            if !cache_nodes.contains(&pass.node_id) {
                cache_nodes.push(pass.node_id);
            }
        }
        for command in &pass.commands {
            if let GlaRenderSource::NodeCache(node_id) = command.source {
                if !cache_nodes.contains(&node_id) {
                    cache_nodes.push(node_id);
                }
            }
        }
    }
    cache_nodes
}

#[cfg(test)]
mod tests {
    use atlas::BackendId;
    use glaphica_core::{CanvasVec2, IMAGE_TILE_SIZE};

    use crate::{
        BlendMode, GlaDoc, GlaImageLayout, GlaNodeId, GlaRenderRefreshKind, GlaRenderSource,
        GlaRenderTarget,
    };

    fn new_doc(layout: GlaImageLayout) -> GlaDoc {
        GlaDoc::new(layout, BackendId::new(3), BackendId::new(7)).expect("document should build")
    }

    fn pass_node_ids(refresh: &crate::GlaRenderRefresh) -> Vec<GlaNodeId> {
        refresh.passes.iter().map(|pass| pass.node_id).collect()
    }

    #[test]
    fn full_refresh_skips_single_child_branch_cache() {
        let mut doc = new_doc(GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE));
        let group_id = doc
            .append_group(doc.root_id())
            .expect("group should append");
        let layer_id = doc.append_layer(group_id).expect("layer should append");

        let refresh = doc
            .build_full_render_refresh()
            .expect("refresh should build");

        assert_eq!(refresh.kind, GlaRenderRefreshKind::Full);
        assert_eq!(refresh.root_source, GlaRenderSource::NodeImage(layer_id));
        assert!(refresh.passes.is_empty());
        assert!(!pass_node_ids(&refresh).contains(&group_id));
    }

    #[test]
    fn full_refresh_builds_passes_for_multi_child_ancestors() {
        let mut doc = new_doc(GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE));
        let group_id = doc
            .append_group(doc.root_id())
            .expect("group should append");
        let bottom_id = doc
            .append_layer(group_id)
            .expect("bottom layer should append");
        let top_id = doc.append_layer(group_id).expect("top layer should append");

        let refresh = doc
            .build_full_render_refresh()
            .expect("refresh should build");

        assert_eq!(refresh.root_source, GlaRenderSource::NodeCache(group_id));
        assert_eq!(pass_node_ids(&refresh), vec![group_id]);
        assert_eq!(refresh.passes[0].target, GlaRenderTarget::BranchCache);
        assert_eq!(
            refresh.passes[0]
                .commands
                .iter()
                .map(|command| command.source)
                .collect::<Vec<_>>(),
            vec![
                GlaRenderSource::NodeImage(bottom_id),
                GlaRenderSource::NodeImage(top_id),
            ]
        );
        assert_eq!(refresh.passes[0].commands[0].blend_mode, BlendMode::Normal);
        assert_eq!(refresh.passes[0].commands[1].blend_mode, BlendMode::Normal);
    }

    #[test]
    fn incremental_refresh_only_keeps_active_ancestor_passes() {
        let mut doc = new_doc(GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE));
        let left_group_id = doc
            .append_group(doc.root_id())
            .expect("left group should append");
        let right_group_id = doc
            .append_group(doc.root_id())
            .expect("right group should append");
        let active_layer_id = doc
            .append_layer(left_group_id)
            .expect("active layer should append");
        doc.append_layer(left_group_id)
            .expect("sibling should append");
        doc.append_layer(right_group_id)
            .expect("other branch bottom should append");
        doc.append_layer(right_group_id)
            .expect("other branch top should append");
        doc.set_active_layer(active_layer_id)
            .expect("active layer should update");

        let refresh = doc
            .build_active_layer_incremental_refresh(&[1])
            .expect("refresh should build")
            .expect("refresh should exist");

        assert_eq!(refresh.kind, GlaRenderRefreshKind::Incremental);
        assert_eq!(refresh.tile_indices, vec![1]);
        assert_eq!(pass_node_ids(&refresh), vec![left_group_id, doc.root_id()]);
        assert_eq!(refresh.passes[0].target, GlaRenderTarget::BranchCache);
        assert_eq!(refresh.passes[1].target, GlaRenderTarget::RootImage);
    }

    #[test]
    fn incremental_refresh_region_uses_layout_tile_order() {
        let mut doc = new_doc(GlaImageLayout::new(
            IMAGE_TILE_SIZE * 2,
            IMAGE_TILE_SIZE * 2,
        ));
        let active_layer_id = doc
            .append_layer(doc.root_id())
            .expect("layer should append");
        doc.append_layer(doc.root_id())
            .expect("sibling should append");
        doc.set_active_layer(active_layer_id)
            .expect("active layer should update");

        let refresh = doc
            .build_active_layer_incremental_refresh_for_region(
                CanvasVec2::new(IMAGE_TILE_SIZE as f32, IMAGE_TILE_SIZE as f32),
                IMAGE_TILE_SIZE,
            )
            .expect("refresh should build")
            .expect("refresh should exist");

        assert_eq!(refresh.tile_indices, vec![0, 1, 2, 3]);
    }

    #[test]
    fn incremental_refresh_rejects_out_of_bounds_tile_index() {
        let mut doc = new_doc(GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE));
        let layer_id = doc
            .append_layer(doc.root_id())
            .expect("layer should append");
        doc.append_layer(doc.root_id())
            .expect("sibling should append");
        doc.set_active_layer(layer_id)
            .expect("active layer should update");

        let refresh = doc.build_active_layer_incremental_refresh(&[7]);

        assert_eq!(
            refresh,
            Err(crate::GlaDocError::InvalidTileIndex {
                tile_index: 7,
                tile_count: 1,
            })
        );
    }
}
