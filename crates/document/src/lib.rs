use std::cell::{Cell, RefCell};
use std::sync::Arc;

use render_protocol::{BlendMode, ImageHandle, RenderStepEntry, RenderStepSnapshot};
use slotmap::SlotMap;
use tiles::{TileKey, VirtualImage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerNodeId(u64);

impl LayerNodeId {
    pub const ROOT: Self = Self(0);
}

pub struct Document {
    layer_tree: LayerTreeNode,
    images: SlotMap<ImageHandle, Arc<VirtualImage<TileKey>>>,
    size_x: u32,
    size_y: u32,
    next_layer_id: u64,
    render_step_cache: RefCell<Arc<[RenderStepEntry]>>,
    render_step_cache_dirty: Cell<bool>,
}

pub enum LayerTreeNode {
    Root {
        children: Vec<LayerTreeNode>,
    },
    Branch {
        id: LayerNodeId,
        blend: BlendMode,
        children: Vec<LayerTreeNode>,
    },
    Leaf {
        id: LayerNodeId,
        blend: BlendMode,
        image_handle: ImageHandle,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupError {
    NotSameLevel,
}

impl Document {
    pub fn new(size_x: u32, size_y: u32) -> Self {
        Self {
            layer_tree: LayerTreeNode::Root {
                children: Vec::new(),
            },
            images: SlotMap::with_key(),
            size_x,
            size_y,
            next_layer_id: 1,
            render_step_cache: RefCell::new(Arc::from(Vec::<RenderStepEntry>::new())),
            render_step_cache_dirty: Cell::new(true),
        }
    }

    pub fn size_x(&self) -> u32 {
        self.size_x
    }

    pub fn size_y(&self) -> u32 {
        self.size_y
    }

    pub fn render_step_snapshot(&self, revision: u64) -> RenderStepSnapshot {
        if self.render_step_cache_dirty.get() {
            let mut steps = Vec::new();
            self.layer_tree.build_render_step_entries(&mut steps);
            self.render_step_cache
                .replace(steps.into_boxed_slice().into());
            self.render_step_cache_dirty.set(false);
        }
        RenderStepSnapshot {
            revision,
            steps: self.render_step_cache.borrow().clone(),
        }
    }

    pub fn root(&self) -> &[LayerTreeNode] {
        match &self.layer_tree {
            LayerTreeNode::Root { children } => children,
            _ => unreachable!("document must always store a root node at top-level"),
        }
    }

    fn root_mut(&mut self) -> &mut Vec<LayerTreeNode> {
        match &mut self.layer_tree {
            LayerTreeNode::Root { children } => children,
            _ => unreachable!("document must always store a root node at top-level"),
        }
    }

    pub fn image(&self, image_handle: ImageHandle) -> Option<Arc<VirtualImage<TileKey>>> {
        self.images.get(image_handle).cloned()
    }

    pub fn new_layer_root(&mut self) -> LayerNodeId {
        let (id, layer) = self.new_empty_leaf();
        self.root_mut().push(layer);
        self.mark_render_steps_dirty();
        id
    }

    pub fn new_layer_root_with_image(
        &mut self,
        image: VirtualImage<TileKey>,
        blend: BlendMode,
    ) -> LayerNodeId {
        let id = self.alloc_layer_id();
        let image_handle = self.images.insert(Arc::new(image));
        self.root_mut().push(LayerTreeNode::Leaf {
            id,
            blend,
            image_handle,
        });
        self.mark_render_steps_dirty();
        id
    }

    pub fn new_layer_above(&mut self, active_layer_id: LayerNodeId) -> LayerNodeId {
        let id = self.insert_new_empty_leaf_above(active_layer_id);
        self.mark_render_steps_dirty();
        id
    }

    pub fn group_layers(
        &mut self,
        first: LayerNodeId,
        second: LayerNodeId,
    ) -> Result<LayerNodeId, GroupError> {
        let next_layer_id = &mut self.next_layer_id;
        let mut alloc = || {
            let id = LayerNodeId(*next_layer_id);
            *next_layer_id = next_layer_id
                .checked_add(1)
                .expect("layer id space exhausted");
            id
        };

        let grouped = self.layer_tree.group_layers(first, second, &mut alloc)?;
        if grouped.is_some() {
            self.mark_render_steps_dirty();
        }
        grouped.ok_or(GroupError::NotSameLevel)
    }

    fn mark_render_steps_dirty(&self) {
        self.render_step_cache_dirty.set(true);
    }

    fn alloc_layer_id(&mut self) -> LayerNodeId {
        let id = LayerNodeId(self.next_layer_id);
        self.next_layer_id = self
            .next_layer_id
            .checked_add(1)
            .expect("layer id space exhausted");
        id
    }

    fn new_empty_leaf(&mut self) -> (LayerNodeId, LayerTreeNode) {
        let id = self.alloc_layer_id();
        let image = VirtualImage::new(self.size_x, self.size_y)
            .unwrap_or_else(|error| panic!("failed to create empty layer image: {error:?}"));
        let image_handle = self.images.insert(Arc::new(image));
        (
            id,
            LayerTreeNode::Leaf {
                id,
                blend: BlendMode::Normal,
                image_handle,
            },
        )
    }

    fn insert_new_empty_leaf_above(&mut self, active_layer_id: LayerNodeId) -> LayerNodeId {
        let (id, layer) = self.new_empty_leaf();
        let remaining = self.layer_tree.insert_leaf_above(active_layer_id, layer);
        assert!(
            remaining.is_none(),
            "active layer not found: {:?}",
            active_layer_id
        );
        id
    }
}

impl LayerTreeNode {
    fn id(&self) -> Option<LayerNodeId> {
        match self {
            LayerTreeNode::Branch { id, .. } | LayerTreeNode::Leaf { id, .. } => Some(*id),
            LayerTreeNode::Root { .. } => None,
        }
    }

    fn insert_leaf_above(
        &mut self,
        active_layer_id: LayerNodeId,
        mut new_layer: LayerTreeNode,
    ) -> Option<LayerTreeNode> {
        let children = match self {
            LayerTreeNode::Root { children } | LayerTreeNode::Branch { children, .. } => children,
            LayerTreeNode::Leaf { .. } => return Some(new_layer),
        };

        let mut index = 0;
        while index < children.len() {
            let is_active_layer = matches!(&children[index], LayerTreeNode::Leaf { id, .. } if *id == active_layer_id)
                || matches!(&children[index], LayerTreeNode::Branch { id, .. } if *id == active_layer_id);
            if is_active_layer {
                children.insert(index + 1, new_layer);
                return None;
            }

            if let Some(layer) = children[index].insert_leaf_above(active_layer_id, new_layer) {
                new_layer = layer;
                index += 1;
                continue;
            }

            return None;
        }

        Some(new_layer)
    }

    fn group_layers(
        &mut self,
        first: LayerNodeId,
        second: LayerNodeId,
        alloc_layer_id: &mut impl FnMut() -> LayerNodeId,
    ) -> Result<Option<LayerNodeId>, GroupError> {
        let children = match self {
            LayerTreeNode::Root { children } | LayerTreeNode::Branch { children, .. } => children,
            LayerTreeNode::Leaf { .. } => return Ok(None),
        };

        let mut first_index = None;
        let mut second_index = None;
        for (index, child) in children.iter().enumerate() {
            let Some(id) = child.id() else { continue };
            if id == first {
                first_index = Some(index);
            }
            if id == second {
                second_index = Some(index);
            }
        }

        match (first_index, second_index) {
            (Some(first_index), Some(second_index)) => {
                let start = first_index.min(second_index);
                let end = first_index.max(second_index);
                let branch_id = alloc_layer_id();

                let branch_children: Vec<LayerTreeNode> = children.drain(start..=end).collect();
                children.insert(
                    start,
                    LayerTreeNode::Branch {
                        id: branch_id,
                        blend: BlendMode::Normal,
                        children: branch_children,
                    },
                );
                Ok(Some(branch_id))
            }
            (Some(_), None) | (None, Some(_)) => Err(GroupError::NotSameLevel),
            (None, None) => {
                for child in children.iter_mut() {
                    if let Some(id) = child.group_layers(first, second, alloc_layer_id)? {
                        return Ok(Some(id));
                    }
                }
                Ok(None)
            }
        }
    }

    fn build_render_step_entries(&self, steps: &mut Vec<RenderStepEntry>) {
        match self {
            LayerTreeNode::Root { children } => {
                for child in children {
                    child.build_render_step_entries(steps);
                }
                steps.push(RenderStepEntry::Group {
                    group_id: LayerNodeId::ROOT.0,
                    child_count: children
                        .len()
                        .try_into()
                        .expect("root group child count exceeds u32"),
                    blend: BlendMode::Normal,
                });
            }
            LayerTreeNode::Branch {
                id,
                blend,
                children,
            } => {
                for child in children {
                    child.build_render_step_entries(steps);
                }
                steps.push(RenderStepEntry::Group {
                    group_id: id.0,
                    child_count: children
                        .len()
                        .try_into()
                        .expect("group child count exceeds u32"),
                    blend: *blend,
                });
            }
            LayerTreeNode::Leaf {
                id,
                blend,
                image_handle,
            } => steps.push(RenderStepEntry::Leaf {
                layer_id: id.0,
                blend: *blend,
                image_handle: *image_handle,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum StepRepr {
        Leaf(u64, BlendMode),
        Group(u64, u32, BlendMode),
    }

    fn steps_repr(document: &Document) -> Vec<StepRepr> {
        document
            .render_step_snapshot(7)
            .steps
            .iter()
            .copied()
            .map(|step| match step {
                RenderStepEntry::Leaf {
                    layer_id, blend, ..
                } => StepRepr::Leaf(layer_id, blend),
                RenderStepEntry::Group {
                    group_id,
                    child_count,
                    blend,
                } => StepRepr::Group(group_id, child_count, blend),
            })
            .collect()
    }

    fn leaf_id(node: &LayerTreeNode) -> LayerNodeId {
        match node {
            LayerTreeNode::Leaf { id, .. } => *id,
            _ => panic!("expected leaf"),
        }
    }

    fn branch_id(node: &LayerTreeNode) -> LayerNodeId {
        match node {
            LayerTreeNode::Branch { id, .. } => *id,
            _ => panic!("expected branch"),
        }
    }

    fn branch_blend(node: &LayerTreeNode) -> BlendMode {
        match node {
            LayerTreeNode::Branch { blend, .. } => *blend,
            _ => panic!("expected branch"),
        }
    }

    fn branch_children(node: &LayerTreeNode) -> &[LayerTreeNode] {
        match node {
            LayerTreeNode::Branch { children, .. } => children,
            _ => panic!("expected branch"),
        }
    }

    fn set_leaf_blend(node: &mut LayerTreeNode, target: LayerNodeId, blend: BlendMode) -> bool {
        match node {
            LayerTreeNode::Leaf {
                id,
                blend: leaf_blend,
                ..
            } if *id == target => {
                *leaf_blend = blend;
                true
            }
            LayerTreeNode::Root { children } | LayerTreeNode::Branch { children, .. } => {
                for child in children {
                    if set_leaf_blend(child, target, blend) {
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    fn set_branch_blend(node: &mut LayerTreeNode, target: LayerNodeId, blend: BlendMode) -> bool {
        match node {
            LayerTreeNode::Branch {
                id,
                blend: branch_blend,
                ..
            } if *id == target => {
                *branch_blend = blend;
                true
            }
            LayerTreeNode::Root { children } | LayerTreeNode::Branch { children, .. } => {
                for child in children {
                    if set_branch_blend(child, target, blend) {
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    #[test]
    fn new_document_has_empty_root_and_single_root_group_step() {
        let document = Document::new(64, 32);
        assert_eq!(document.root().len(), 0);
        assert_eq!(
            steps_repr(&document),
            vec![StepRepr::Group(0, 0, BlendMode::Normal)]
        );
    }

    #[test]
    fn new_layer_root_appends_leaf_with_default_properties() {
        let mut document = Document::new(17, 23);
        let id = document.new_layer_root();
        assert_ne!(id, LayerNodeId::ROOT);
        assert_eq!(document.root().len(), 1);

        let node = &document.root()[0];
        match node {
            LayerTreeNode::Leaf {
                id: leaf_id,
                blend,
                image_handle,
            } => {
                assert_eq!(*leaf_id, id);
                assert_eq!(*blend, BlendMode::Normal);
                let image = document
                    .image(*image_handle)
                    .expect("image handle should resolve");
                assert_eq!(image.size_x(), 17);
                assert_eq!(image.size_y(), 23);
            }
            _ => panic!("expected root child to be leaf"),
        }
    }

    #[test]
    fn new_layer_root_with_image_uses_provided_image_and_blend() {
        let mut document = Document::new(17, 23);
        let image = VirtualImage::<TileKey>::new(9, 11).expect("new image");
        let id = document.new_layer_root_with_image(image, BlendMode::Multiply);

        let node = &document.root()[0];
        match node {
            LayerTreeNode::Leaf {
                id: leaf_id,
                blend,
                image_handle,
            } => {
                assert_eq!(*leaf_id, id);
                assert_eq!(*blend, BlendMode::Multiply);
                let image = document
                    .image(*image_handle)
                    .expect("image handle should resolve");
                assert_eq!(image.size_x(), 9);
                assert_eq!(image.size_y(), 11);
            }
            _ => panic!("expected root child to be leaf"),
        }
    }

    #[test]
    fn new_layer_above_inserts_next_to_active_leaf_in_same_parent() {
        let mut document = Document::new(10, 10);
        let a = document.new_layer_root();
        let b = document.new_layer_root();
        let c = document.new_layer_root();

        let branch = document.group_layers(a, b).expect("group must succeed");

        let inserted = document.new_layer_above(a);

        assert_eq!(document.root().len(), 2);
        let branch_node = &document.root()[0];
        assert_eq!(branch_id(branch_node), branch);
        let children = branch_children(branch_node);
        assert_eq!(children.len(), 3);
        assert_eq!(leaf_id(&children[0]), a);
        assert_eq!(leaf_id(&children[1]), inserted);
        assert_eq!(leaf_id(&children[2]), b);

        assert!(matches!(&document.root()[1], LayerTreeNode::Leaf { id, .. } if *id == c));
    }

    #[test]
    fn new_layer_above_inserts_next_to_active_branch_in_parent() {
        let mut document = Document::new(10, 10);
        let a = document.new_layer_root();
        let b = document.new_layer_root();
        let c = document.new_layer_root();

        let branch = document.group_layers(a, b).expect("group must succeed");
        let inserted = document.new_layer_above(branch);

        assert_eq!(document.root().len(), 3);
        assert!(matches!(&document.root()[0], LayerTreeNode::Branch { id, .. } if *id == branch));
        assert!(matches!(&document.root()[1], LayerTreeNode::Leaf { id, .. } if *id == inserted));
        assert!(matches!(&document.root()[2], LayerTreeNode::Leaf { id, .. } if *id == c));
    }

    #[test]
    #[should_panic(expected = "active layer not found")]
    fn new_layer_above_panics_if_active_not_found() {
        let mut document = Document::new(10, 10);
        let _ = document.new_layer_above(LayerNodeId(123));
    }

    #[test]
    fn group_layers_wraps_range_in_new_branch_and_preserves_order() {
        let mut document = Document::new(1, 1);
        let a = document.new_layer_root();
        let b = document.new_layer_root();
        let c = document.new_layer_root();
        let d = document.new_layer_root();

        let branch = document.group_layers(b, d).expect("group must succeed");

        assert_eq!(document.root().len(), 2);
        assert!(matches!(&document.root()[0], LayerTreeNode::Leaf { id, .. } if *id == a));

        let branch_node = &document.root()[1];
        assert_eq!(branch_id(branch_node), branch);
        let children = branch_children(branch_node);
        assert_eq!(children.len(), 3);
        assert_eq!(leaf_id(&children[0]), b);
        assert_eq!(leaf_id(&children[1]), c);
        assert_eq!(leaf_id(&children[2]), d);
        assert_eq!(branch_blend(branch_node), BlendMode::Normal);
    }

    #[test]
    fn group_layers_allows_same_id_and_wraps_single_node() {
        let mut document = Document::new(1, 1);
        let a = document.new_layer_root();
        let b = document.new_layer_root();

        let branch = document.group_layers(a, a).expect("group must succeed");
        assert_eq!(document.root().len(), 2);

        let branch_node = &document.root()[0];
        assert_eq!(branch_id(branch_node), branch);
        let children = branch_children(branch_node);
        assert_eq!(children.len(), 1);
        assert_eq!(leaf_id(&children[0]), a);

        assert!(matches!(&document.root()[1], LayerTreeNode::Leaf { id, .. } if *id == b));
    }

    #[test]
    fn group_layers_returns_err_if_not_same_level() {
        let mut document = Document::new(1, 1);
        let a = document.new_layer_root();
        let b = document.new_layer_root();
        let c = document.new_layer_root();
        let _branch = document.group_layers(a, b).expect("group must succeed");

        let error = document
            .group_layers(a, c)
            .expect_err("must be different level");
        assert_eq!(error, GroupError::NotSameLevel);
    }

    #[test]
    fn render_steps_are_postorder_and_include_group_ids() {
        let mut document = Document::new(1, 1);
        let a = document.new_layer_root();
        let b = document.new_layer_root();
        let c = document.new_layer_root();
        let branch = document.group_layers(a, b).expect("group must succeed");

        let steps = steps_repr(&document);
        assert_eq!(
            steps,
            vec![
                StepRepr::Leaf(a.0, BlendMode::Normal),
                StepRepr::Leaf(b.0, BlendMode::Normal),
                StepRepr::Group(branch.0, 2, BlendMode::Normal),
                StepRepr::Leaf(c.0, BlendMode::Normal),
                StepRepr::Group(LayerNodeId::ROOT.0, 2, BlendMode::Normal),
            ]
        );
    }

    #[test]
    fn render_step_snapshot_leaf_includes_blend_mode() {
        let mut document = Document::new(1, 1);
        let a = document.new_layer_root();
        assert!(set_leaf_blend(
            &mut document.layer_tree,
            a,
            BlendMode::Multiply
        ));

        let steps = steps_repr(&document);
        assert_eq!(
            steps,
            vec![
                StepRepr::Leaf(a.0, BlendMode::Multiply),
                StepRepr::Group(LayerNodeId::ROOT.0, 1, BlendMode::Normal),
            ]
        );
    }

    #[test]
    fn render_step_snapshot_group_includes_branch_blend_mode() {
        let mut document = Document::new(1, 1);
        let a = document.new_layer_root();
        let b = document.new_layer_root();
        let branch = document.group_layers(a, b).expect("group must succeed");

        assert!(set_branch_blend(
            &mut document.layer_tree,
            branch,
            BlendMode::Multiply
        ));

        let steps = steps_repr(&document);
        assert_eq!(
            steps,
            vec![
                StepRepr::Leaf(a.0, BlendMode::Normal),
                StepRepr::Leaf(b.0, BlendMode::Normal),
                StepRepr::Group(branch.0, 2, BlendMode::Multiply),
                StepRepr::Group(LayerNodeId::ROOT.0, 1, BlendMode::Normal),
            ]
        );
    }

    #[test]
    fn render_step_snapshot_leaf_image_handle_resolves() {
        let mut document = Document::new(8, 4);
        let _ = document.new_layer_root();

        let snapshot = document.render_step_snapshot(1);
        let image_handle = snapshot
            .steps
            .iter()
            .find_map(|entry| match entry {
                RenderStepEntry::Leaf { image_handle, .. } => Some(*image_handle),
                RenderStepEntry::Group { .. } => None,
            })
            .expect("snapshot should contain one leaf");

        let image = document
            .image(image_handle)
            .expect("leaf image handle should resolve");
        assert_eq!(image.size_x(), 8);
        assert_eq!(image.size_y(), 4);
    }
}
