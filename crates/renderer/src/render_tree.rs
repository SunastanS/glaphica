//! Render-tree reconstruction and traversal utilities.
//!
//! This module rebuilds hierarchical render nodes from step snapshots and
//! provides traversal helpers used by dirty propagation.

use std::collections::HashMap;

use render_protocol::RenderNodeSnapshot;

use super::{DirtyRectMask, RenderNodeKey};

pub(super) type RenderTreeNode = RenderNodeSnapshot;

fn merge_rect_masks(base: &mut DirtyRectMask, incoming: DirtyRectMask) {
    match (base, incoming) {
        (DirtyRectMask::Full, _) => {}
        (slot @ DirtyRectMask::Rects(_), DirtyRectMask::Full) => {
            *slot = DirtyRectMask::Full;
        }
        (DirtyRectMask::Rects(existing_rects), DirtyRectMask::Rects(incoming_rects)) => {
            existing_rects.extend(incoming_rects);
        }
    }
}

pub(super) fn collect_node_dirty_rects(
    node: &RenderTreeNode,
    layer_dirty_rect_masks: &HashMap<u64, DirtyRectMask>,
    dirty_nodes: &mut HashMap<RenderNodeKey, DirtyRectMask>,
) -> Option<DirtyRectMask> {
    match node {
        RenderTreeNode::Leaf { layer_id, .. } => {
            let dirty_rect_mask = layer_dirty_rect_masks.get(layer_id)?.clone();
            dirty_nodes.insert(RenderNodeKey::Leaf(*layer_id), dirty_rect_mask.clone());
            Some(dirty_rect_mask)
        }
        RenderTreeNode::Group {
            group_id, children, ..
        } => {
            let mut group_dirty: Option<DirtyRectMask> = None;
            for child in children.iter() {
                if let Some(child_dirty_rect_mask) =
                    collect_node_dirty_rects(child, layer_dirty_rect_masks, dirty_nodes)
                {
                    if let Some(existing_dirty) = group_dirty.as_mut() {
                        merge_rect_masks(existing_dirty, child_dirty_rect_mask);
                    } else {
                        group_dirty = Some(child_dirty_rect_mask);
                    }
                }
            }
            if let Some(group_dirty_rect_mask) = group_dirty.as_ref() {
                dirty_nodes.insert(
                    RenderNodeKey::Group(*group_id),
                    group_dirty_rect_mask.clone(),
                );
            }
            group_dirty
        }
    }
}
