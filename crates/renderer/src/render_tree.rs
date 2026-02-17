//! Render-tree reconstruction and traversal utilities.
//!
//! This module rebuilds hierarchical render nodes from step snapshots and
//! provides traversal helpers used by dirty propagation.

use std::collections::HashMap;

use render_protocol::{BlendMode, ImageHandle, RenderStepEntry, RenderStepSnapshot};

use super::{DirtyRectMask, RenderNodeKey};

#[derive(Debug, Clone)]
pub(super) enum RenderTreeNode {
    Leaf {
        #[cfg_attr(not(test), allow(dead_code))]
        layer_id: u64,
        blend: BlendMode,
        image_handle: ImageHandle,
    },
    Group {
        group_id: u64,
        blend: BlendMode,
        children: Vec<RenderTreeNode>,
    },
}

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
            for child in children {
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

pub(super) fn build_render_tree_from_snapshot(snapshot: &RenderStepSnapshot) -> RenderTreeNode {
    let mut stack = Vec::new();
    for step in snapshot.steps.iter() {
        match step {
            RenderStepEntry::Leaf {
                layer_id,
                blend,
                image_handle,
                ..
            } => stack.push(RenderTreeNode::Leaf {
                layer_id: *layer_id,
                blend: *blend,
                image_handle: *image_handle,
            }),
            RenderStepEntry::Group {
                group_id,
                child_count,
                blend,
            } => {
                let child_count = *child_count as usize;
                if child_count > stack.len() {
                    panic!("render step group has more children than available nodes");
                }

                let split_index = stack.len() - child_count;
                let children = stack.split_off(split_index);
                stack.push(RenderTreeNode::Group {
                    group_id: *group_id,
                    blend: *blend,
                    children,
                });
            }
        }
    }

    if stack.len() != 1 {
        panic!("render steps must reduce to a single root group");
    }
    let root = stack.pop().expect("render tree stack should contain root");
    assert!(
        matches!(root, RenderTreeNode::Group { group_id: 0, .. }),
        "render tree root must be group 0"
    );
    root
}
