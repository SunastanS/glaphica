//! Dirty-region tracking and propagation.
//!
//! This module stores layer dirty state, converts dirty rects to tile masks,
//! and propagates dirtiness through render-tree hierarchy.

use std::collections::{HashMap, HashSet};

use super::{
    collect_node_dirty_rects, dirty_rect_to_tile_coords, DirtyRect, RenderDataResolver,
    RenderTreeNode, GROUP_FULL_DIRTY_RATIO_THRESHOLD,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct TileCoord {
    pub(super) tile_x: u32,
    pub(super) tile_y: u32,
}

#[derive(Debug, Clone)]
pub(super) enum DirtyTileMask {
    Full,
    Partial(HashSet<TileCoord>),
}

#[derive(Debug, Clone)]
pub(super) enum DirtyRectMask {
    Full,
    Rects(Vec<DirtyRect>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum RenderNodeKey {
    Leaf(u64),
    Group(u64),
}

#[derive(Debug, Default, Clone)]
pub(super) struct DirtyStateStore {
    layer_dirty_masks: HashMap<u64, DirtyTileMask>,
    layer_dirty_rects: HashMap<u64, DirtyRectMask>,
    document_composite_dirty: bool,
}

impl DirtyStateStore {
    pub(super) fn with_document_dirty(document_composite_dirty: bool) -> Self {
        Self {
            layer_dirty_masks: HashMap::new(),
            layer_dirty_rects: HashMap::new(),
            document_composite_dirty,
        }
    }

    pub(super) fn mark_layer_full(&mut self, layer_id: u64) {
        self.layer_dirty_masks.insert(layer_id, DirtyTileMask::Full);
        self.layer_dirty_rects.insert(layer_id, DirtyRectMask::Full);
    }

    #[allow(dead_code)]
    pub(super) fn mark_layer_rect(&mut self, layer_id: u64, dirty_rect: DirtyRect) -> bool {
        if dirty_rect.min_x >= dirty_rect.max_x || dirty_rect.min_y >= dirty_rect.max_y {
            return false;
        }
        if matches!(
            self.layer_dirty_masks.get(&layer_id),
            Some(DirtyTileMask::Full)
        ) {
            return false;
        }
        match self.layer_dirty_rects.get_mut(&layer_id) {
            Some(DirtyRectMask::Full) => {
                return false;
            }
            Some(DirtyRectMask::Rects(existing_rects)) => {
                existing_rects.push(dirty_rect);
            }
            None => {
                self.layer_dirty_rects
                    .insert(layer_id, DirtyRectMask::Rects(vec![dirty_rect]));
            }
        }
        true
    }

    pub(super) fn is_document_composite_dirty(&self) -> bool {
        self.document_composite_dirty
    }

    pub(super) fn mark_document_composite_dirty(&mut self) {
        self.document_composite_dirty = true;
    }

    pub(super) fn clear_document_composite_dirty(&mut self) {
        self.document_composite_dirty = false;
    }

    pub(super) fn clear_layer_dirty_masks(&mut self) {
        self.layer_dirty_masks.clear();
        self.layer_dirty_rects.clear();
    }

    pub(super) fn retain_layers(&mut self, live_layer_ids: &HashSet<u64>) {
        self.layer_dirty_masks
            .retain(|layer_id, _| live_layer_ids.contains(layer_id));
        self.layer_dirty_rects
            .retain(|layer_id, _| live_layer_ids.contains(layer_id));
    }

    pub(super) fn resolve_layer_dirty_rect_masks(
        &self,
        render_data_resolver: &dyn RenderDataResolver,
    ) -> HashMap<u64, DirtyRectMask> {
        let mut layer_dirty_rect_masks = HashMap::new();
        for (layer_id, dirty_tile_mask) in &self.layer_dirty_masks {
            if matches!(dirty_tile_mask, DirtyTileMask::Full) {
                layer_dirty_rect_masks.insert(*layer_id, DirtyRectMask::Full);
            }
        }

        for (layer_id, dirty_rect_mask) in &self.layer_dirty_rects {
            if matches!(
                layer_dirty_rect_masks.get(layer_id),
                Some(DirtyRectMask::Full)
            ) {
                continue;
            }
            match dirty_rect_mask {
                DirtyRectMask::Full => {
                    layer_dirty_rect_masks.insert(*layer_id, DirtyRectMask::Full);
                }
                DirtyRectMask::Rects(incoming_rects) => {
                    let propagated_rects =
                        render_data_resolver.propagate_layer_dirty_rects(*layer_id, incoming_rects);
                    if propagated_rects.is_empty() {
                        continue;
                    }
                    merge_dirty_rects(&mut layer_dirty_rect_masks, *layer_id, propagated_rects);
                }
            }
        }

        layer_dirty_rect_masks
    }
}

fn merge_dirty_rects(
    layer_dirty_rect_masks: &mut HashMap<u64, DirtyRectMask>,
    layer_id: u64,
    incoming_rects: Vec<DirtyRect>,
) {
    if incoming_rects.is_empty() {
        return;
    }
    match layer_dirty_rect_masks.get_mut(&layer_id) {
        Some(DirtyRectMask::Full) => {}
        Some(DirtyRectMask::Rects(existing_rects)) => {
            existing_rects.extend(incoming_rects);
        }
        None => {
            layer_dirty_rect_masks.insert(layer_id, DirtyRectMask::Rects(incoming_rects));
        }
    }
}

pub(super) fn dirty_rects_to_tile_coords(dirty_rects: &[DirtyRect]) -> HashSet<TileCoord> {
    let mut tiles = HashSet::new();
    for dirty_rect in dirty_rects {
        tiles.extend(dirty_rect_to_tile_coords(*dirty_rect));
    }
    tiles
}

#[derive(Debug)]
pub(super) struct DirtyPropagationEngine {
    group_tile_count: usize,
}

impl DirtyPropagationEngine {
    pub(super) fn new(group_tile_count: usize) -> Self {
        Self { group_tile_count }
    }

    pub(super) fn collect_node_tile_masks(
        &self,
        render_tree: &RenderTreeNode,
        layer_dirty_rect_masks: &HashMap<u64, DirtyRectMask>,
    ) -> HashMap<RenderNodeKey, DirtyTileMask> {
        let mut node_dirty_rects = HashMap::new();
        let _ =
            collect_node_dirty_rects(render_tree, layer_dirty_rect_masks, &mut node_dirty_rects);

        node_dirty_rects
            .into_iter()
            .filter_map(|(node_key, dirty_rect_mask)| {
                let dirty_tile_mask = match dirty_rect_mask {
                    DirtyRectMask::Full => DirtyTileMask::Full,
                    DirtyRectMask::Rects(rects) => {
                        let dirty_tiles = dirty_rects_to_tile_coords(&rects);
                        if dirty_tiles.is_empty() {
                            return None;
                        }
                        if matches!(node_key, RenderNodeKey::Group(_)) {
                            let dirty_ratio =
                                (dirty_tiles.len() as f32) / (self.group_tile_count as f32);
                            if dirty_ratio >= GROUP_FULL_DIRTY_RATIO_THRESHOLD {
                                DirtyTileMask::Full
                            } else {
                                DirtyTileMask::Partial(dirty_tiles)
                            }
                        } else {
                            DirtyTileMask::Partial(dirty_tiles)
                        }
                    }
                };
                Some((node_key, dirty_tile_mask))
            })
            .collect()
    }
}
