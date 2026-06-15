use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};

use gla_ir::ImageId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct DocumentNodeId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocumentNodeKind {
    Root,
    Group,
    Layer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocumentBlendMode {
    Normal,
    Overlay,
    Multiply,
    MaskAlpha,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentLayerNode {
    id: DocumentNodeId,
    kind: DocumentNodeKind,
    parent: Option<DocumentNodeId>,
    image: ImageId,
    opacity: f32,
    blend_mode: DocumentBlendMode,
    children: Vec<DocumentNodeId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentLayerTree {
    root_id: DocumentNodeId,
    active_node_id: DocumentNodeId,
    active_ancestor_chain: Vec<DocumentNodeId>,
    next_id: u64,
    nodes: BTreeMap<DocumentNodeId, DocumentLayerNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DocumentLayerTreeError {
    InvalidNodeId(DocumentNodeId),
    CannotInsertIntoLayer(DocumentNodeId),
    ChildIndexOutOfBounds {
        parent_id: DocumentNodeId,
        child_count: usize,
        index: usize,
    },
    CannotMoveRoot,
    CannotDeleteRoot,
    CannotMoveNodeIntoDescendant {
        node_id: DocumentNodeId,
        parent_id: DocumentNodeId,
    },
    BrokenParentChildLink {
        parent_id: DocumentNodeId,
        child_id: DocumentNodeId,
    },
    InvalidOpacity(f32),
}

impl DocumentNodeId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

impl DocumentBlendMode {
    pub fn as_renderer_blend_mode(self) -> Option<gla_color::BlendMode> {
        match self {
            Self::Normal => Some(gla_color::BlendMode::Normal),
            Self::Overlay => Some(gla_color::BlendMode::Overlay),
            Self::Multiply => Some(gla_color::BlendMode::Multiply),
            Self::MaskAlpha => Some(gla_color::BlendMode::MaskAlpha),
        }
    }
}

impl DocumentLayerNode {
    fn new(
        id: DocumentNodeId,
        kind: DocumentNodeKind,
        parent: Option<DocumentNodeId>,
        image: ImageId,
    ) -> Self {
        Self {
            id,
            kind,
            parent,
            image,
            opacity: 1.0,
            blend_mode: DocumentBlendMode::Normal,
            children: Vec::new(),
        }
    }

    pub fn id(&self) -> DocumentNodeId {
        self.id
    }

    pub fn kind(&self) -> DocumentNodeKind {
        self.kind
    }

    pub fn parent(&self) -> Option<DocumentNodeId> {
        self.parent
    }

    pub fn image(&self) -> ImageId {
        self.image
    }

    pub fn opacity(&self) -> f32 {
        self.opacity
    }

    pub fn blend_mode(&self) -> DocumentBlendMode {
        self.blend_mode
    }

    pub fn children(&self) -> Option<&[DocumentNodeId]> {
        if self.kind == DocumentNodeKind::Layer {
            None
        } else {
            Some(&self.children)
        }
    }

    fn children_mut(&mut self) -> Option<&mut Vec<DocumentNodeId>> {
        if self.kind == DocumentNodeKind::Layer {
            None
        } else {
            Some(&mut self.children)
        }
    }
}

impl DocumentLayerTree {
    pub fn new(root_image: ImageId) -> Self {
        let root_id = DocumentNodeId::new(1);
        let mut nodes = BTreeMap::new();
        nodes.insert(
            root_id,
            DocumentLayerNode::new(root_id, DocumentNodeKind::Root, None, root_image),
        );
        Self {
            root_id,
            active_node_id: root_id,
            active_ancestor_chain: vec![root_id],
            next_id: 2,
            nodes,
        }
    }

    pub fn root_id(&self) -> DocumentNodeId {
        self.root_id
    }

    pub fn active_node_id(&self) -> DocumentNodeId {
        self.active_node_id
    }

    pub fn active_ancestor_chain(&self) -> &[DocumentNodeId] {
        &self.active_ancestor_chain
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn contains_node(&self, node_id: DocumentNodeId) -> bool {
        self.nodes.contains_key(&node_id)
    }

    pub fn node(
        &self,
        node_id: DocumentNodeId,
    ) -> Result<&DocumentLayerNode, DocumentLayerTreeError> {
        self.nodes
            .get(&node_id)
            .ok_or(DocumentLayerTreeError::InvalidNodeId(node_id))
    }

    pub fn child_ids(
        &self,
        parent_id: DocumentNodeId,
    ) -> Result<&[DocumentNodeId], DocumentLayerTreeError> {
        self.node(parent_id)?
            .children()
            .ok_or(DocumentLayerTreeError::CannotInsertIntoLayer(parent_id))
    }

    pub fn child_index(
        &self,
        parent_id: DocumentNodeId,
        child_id: DocumentNodeId,
    ) -> Result<usize, DocumentLayerTreeError> {
        self.find_child_index(parent_id, child_id)
    }

    pub fn set_active_node(
        &mut self,
        node_id: DocumentNodeId,
    ) -> Result<(), DocumentLayerTreeError> {
        let ancestor_chain = self.build_path_to_root(node_id)?;
        self.active_node_id = node_id;
        self.active_ancestor_chain = ancestor_chain;
        Ok(())
    }

    pub fn set_opacity(
        &mut self,
        node_id: DocumentNodeId,
        opacity: f32,
    ) -> Result<(), DocumentLayerTreeError> {
        let opacity = validate_opacity(opacity)?;
        self.node_mut(node_id)?.opacity = opacity;
        Ok(())
    }

    pub fn set_blend_mode(
        &mut self,
        node_id: DocumentNodeId,
        blend_mode: DocumentBlendMode,
    ) -> Result<(), DocumentLayerTreeError> {
        self.node_mut(node_id)?.blend_mode = blend_mode;
        Ok(())
    }

    pub fn append_layer(
        &mut self,
        parent_id: DocumentNodeId,
        image: ImageId,
    ) -> Result<DocumentNodeId, DocumentLayerTreeError> {
        let child_count = self.child_ids(parent_id)?.len();
        self.insert_layer(parent_id, child_count, image)
    }

    pub fn append_group(
        &mut self,
        parent_id: DocumentNodeId,
        image: ImageId,
    ) -> Result<DocumentNodeId, DocumentLayerTreeError> {
        let child_count = self.child_ids(parent_id)?.len();
        self.insert_group(parent_id, child_count, image)
    }

    pub fn insert_layer(
        &mut self,
        parent_id: DocumentNodeId,
        index: usize,
        image: ImageId,
    ) -> Result<DocumentNodeId, DocumentLayerTreeError> {
        self.insert_node(parent_id, index, DocumentNodeKind::Layer, image)
    }

    pub fn insert_group(
        &mut self,
        parent_id: DocumentNodeId,
        index: usize,
        image: ImageId,
    ) -> Result<DocumentNodeId, DocumentLayerTreeError> {
        self.insert_node(parent_id, index, DocumentNodeKind::Group, image)
    }

    pub fn move_node(
        &mut self,
        node_id: DocumentNodeId,
        new_parent_id: DocumentNodeId,
        new_index: usize,
    ) -> Result<(), DocumentLayerTreeError> {
        if node_id == self.root_id {
            return Err(DocumentLayerTreeError::CannotMoveRoot);
        }

        let old_parent_id = self
            .node(node_id)?
            .parent()
            .ok_or(DocumentLayerTreeError::CannotMoveRoot)?;
        let moved_active_subtree =
            old_parent_id != new_parent_id && self.is_ancestor(node_id, self.active_node_id)?;
        self.validate_insert_target(new_parent_id, new_index)?;

        if self.is_ancestor(node_id, new_parent_id)? {
            return Err(DocumentLayerTreeError::CannotMoveNodeIntoDescendant {
                node_id,
                parent_id: new_parent_id,
            });
        }

        let old_index = self.find_child_index(old_parent_id, node_id)?;
        if old_parent_id == new_parent_id {
            let parent = self.node_mut(old_parent_id)?;
            let children = parent
                .children_mut()
                .ok_or(DocumentLayerTreeError::CannotInsertIntoLayer(old_parent_id))?;
            let child_id = children.remove(old_index);
            let adjusted_index = if old_index < new_index {
                new_index.saturating_sub(1)
            } else {
                new_index
            };
            children.insert(adjusted_index, child_id);
            return Ok(());
        }

        {
            let old_parent = self.node_mut(old_parent_id)?;
            let children = old_parent
                .children_mut()
                .ok_or(DocumentLayerTreeError::CannotInsertIntoLayer(old_parent_id))?;
            children.remove(old_index);
        }

        {
            let new_parent = self.node_mut(new_parent_id)?;
            let children = new_parent
                .children_mut()
                .ok_or(DocumentLayerTreeError::CannotInsertIntoLayer(new_parent_id))?;
            children.insert(new_index, node_id);
        }

        self.node_mut(node_id)?.parent = Some(new_parent_id);
        if moved_active_subtree {
            self.refresh_active_ancestor_chain()?;
        }
        Ok(())
    }

    pub fn delete_node(&mut self, node_id: DocumentNodeId) -> Result<(), DocumentLayerTreeError> {
        if node_id == self.root_id {
            return Err(DocumentLayerTreeError::CannotDeleteRoot);
        }

        let parent_id = self
            .node(node_id)?
            .parent()
            .ok_or(DocumentLayerTreeError::CannotDeleteRoot)?;
        let child_index = self.find_child_index(parent_id, node_id)?;
        let subtree = self.collect_subtree_postorder(node_id)?;
        let active_node_in_deleted_subtree = subtree.contains(&self.active_node_id);

        {
            let parent = self.node_mut(parent_id)?;
            let children = parent
                .children_mut()
                .ok_or(DocumentLayerTreeError::CannotInsertIntoLayer(parent_id))?;
            children.remove(child_index);
        }

        for descendant_id in subtree {
            self.nodes
                .remove(&descendant_id)
                .ok_or(DocumentLayerTreeError::InvalidNodeId(descendant_id))?;
        }

        if active_node_in_deleted_subtree {
            self.active_node_id = parent_id;
            self.refresh_active_ancestor_chain()?;
        }
        Ok(())
    }

    pub fn collect_subtree_preorder(
        &self,
        root_id: DocumentNodeId,
        output: &mut Vec<DocumentNodeId>,
    ) -> Result<(), DocumentLayerTreeError> {
        self.node(root_id)?;
        output.clear();
        let mut stack = vec![root_id];
        while let Some(node_id) = stack.pop() {
            output.push(node_id);
            if let Some(children) = self.node(node_id)?.children() {
                for &child_id in children.iter().rev() {
                    stack.push(child_id);
                }
            }
        }
        Ok(())
    }

    pub fn collect_path_to_root(
        &self,
        start_id: DocumentNodeId,
        output: &mut Vec<DocumentNodeId>,
    ) -> Result<(), DocumentLayerTreeError> {
        let path = self.build_path_to_root(start_id)?;
        output.clear();
        output.extend(path);
        Ok(())
    }

    fn insert_node(
        &mut self,
        parent_id: DocumentNodeId,
        index: usize,
        kind: DocumentNodeKind,
        image: ImageId,
    ) -> Result<DocumentNodeId, DocumentLayerTreeError> {
        self.validate_insert_target(parent_id, index)?;
        let node_id = self.alloc_node_id();
        self.nodes.insert(
            node_id,
            DocumentLayerNode::new(node_id, kind, Some(parent_id), image),
        );
        let parent = self.node_mut(parent_id)?;
        let children = parent
            .children_mut()
            .ok_or(DocumentLayerTreeError::CannotInsertIntoLayer(parent_id))?;
        children.insert(index, node_id);
        Ok(node_id)
    }

    fn alloc_node_id(&mut self) -> DocumentNodeId {
        let node_id = DocumentNodeId::new(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        node_id
    }

    fn validate_insert_target(
        &self,
        parent_id: DocumentNodeId,
        index: usize,
    ) -> Result<(), DocumentLayerTreeError> {
        let children = self
            .node(parent_id)?
            .children()
            .ok_or(DocumentLayerTreeError::CannotInsertIntoLayer(parent_id))?;
        if index > children.len() {
            return Err(DocumentLayerTreeError::ChildIndexOutOfBounds {
                parent_id,
                child_count: children.len(),
                index,
            });
        }
        Ok(())
    }

    fn node_mut(
        &mut self,
        node_id: DocumentNodeId,
    ) -> Result<&mut DocumentLayerNode, DocumentLayerTreeError> {
        self.nodes
            .get_mut(&node_id)
            .ok_or(DocumentLayerTreeError::InvalidNodeId(node_id))
    }

    fn build_path_to_root(
        &self,
        start_id: DocumentNodeId,
    ) -> Result<Vec<DocumentNodeId>, DocumentLayerTreeError> {
        self.node(start_id)?;
        let mut output = Vec::new();
        let mut current = Some(start_id);
        while let Some(node_id) = current {
            output.push(node_id);
            current = self.node(node_id)?.parent();
        }
        Ok(output)
    }

    fn refresh_active_ancestor_chain(&mut self) -> Result<(), DocumentLayerTreeError> {
        self.active_ancestor_chain = self.build_path_to_root(self.active_node_id)?;
        Ok(())
    }

    fn collect_subtree_postorder(
        &self,
        root_id: DocumentNodeId,
    ) -> Result<Vec<DocumentNodeId>, DocumentLayerTreeError> {
        self.node(root_id)?;
        let mut output = Vec::new();
        let mut stack = vec![(root_id, false)];
        while let Some((node_id, expanded)) = stack.pop() {
            if expanded {
                output.push(node_id);
                continue;
            }
            stack.push((node_id, true));
            if let Some(children) = self.node(node_id)?.children() {
                for &child_id in children.iter().rev() {
                    stack.push((child_id, false));
                }
            }
        }
        Ok(output)
    }

    fn find_child_index(
        &self,
        parent_id: DocumentNodeId,
        child_id: DocumentNodeId,
    ) -> Result<usize, DocumentLayerTreeError> {
        let children = self
            .node(parent_id)?
            .children()
            .ok_or(DocumentLayerTreeError::CannotInsertIntoLayer(parent_id))?;
        children
            .iter()
            .position(|candidate| *candidate == child_id)
            .ok_or(DocumentLayerTreeError::BrokenParentChildLink {
                parent_id,
                child_id,
            })
    }

    fn is_ancestor(
        &self,
        ancestor_id: DocumentNodeId,
        node_id: DocumentNodeId,
    ) -> Result<bool, DocumentLayerTreeError> {
        let mut current = Some(node_id);
        while let Some(candidate_id) = current {
            if candidate_id == ancestor_id {
                return Ok(true);
            }
            current = self.node(candidate_id)?.parent();
        }
        Ok(false)
    }
}

impl Default for DocumentBlendMode {
    fn default() -> Self {
        Self::Normal
    }
}

impl Display for DocumentLayerTreeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidNodeId(node_id) => write!(f, "invalid document node id {node_id:?}"),
            Self::CannotInsertIntoLayer(node_id) => {
                write!(f, "cannot insert children into layer node {node_id:?}")
            }
            Self::ChildIndexOutOfBounds {
                parent_id,
                child_count,
                index,
            } => write!(
                f,
                "child index {index} is out of bounds for parent {parent_id:?} with {child_count} children"
            ),
            Self::CannotMoveRoot => f.write_str("cannot move the root document node"),
            Self::CannotDeleteRoot => f.write_str("cannot delete the root document node"),
            Self::CannotMoveNodeIntoDescendant { node_id, parent_id } => write!(
                f,
                "cannot move document node {node_id:?} into its descendant {parent_id:?}"
            ),
            Self::BrokenParentChildLink {
                parent_id,
                child_id,
            } => write!(
                f,
                "parent {parent_id:?} does not reference child {child_id:?}"
            ),
            Self::InvalidOpacity(opacity) => {
                write!(f, "opacity {opacity} must be finite and within [0, 1]")
            }
        }
    }
}

impl Error for DocumentLayerTreeError {}

fn validate_opacity(opacity: f32) -> Result<f32, DocumentLayerTreeError> {
    if opacity.is_finite() && (0.0..=1.0).contains(&opacity) {
        return Ok(opacity);
    }
    Err(DocumentLayerTreeError::InvalidOpacity(opacity))
}

#[cfg(test)]
mod tests {
    use super::{DocumentBlendMode, DocumentLayerTree, DocumentLayerTreeError, DocumentNodeKind};
    use gla_ir::ImageId;

    #[test]
    fn root_is_active_document_node() {
        let tree = DocumentLayerTree::new(ImageId::new(7));
        let root = tree.node(tree.root_id()).unwrap();

        assert_eq!(tree.active_node_id(), tree.root_id());
        assert_eq!(tree.active_ancestor_chain(), &[tree.root_id()]);
        assert_eq!(root.kind(), DocumentNodeKind::Root);
        assert_eq!(root.image(), ImageId::new(7));
        assert_eq!(root.opacity(), 1.0);
        assert_eq!(root.blend_mode(), DocumentBlendMode::Normal);
    }

    #[test]
    fn appends_groups_and_layers_in_order() {
        let mut tree = DocumentLayerTree::new(ImageId::new(1));
        let group = tree.append_group(tree.root_id(), ImageId::new(2)).unwrap();
        let layer = tree.append_layer(tree.root_id(), ImageId::new(3)).unwrap();

        assert_eq!(tree.node(group).unwrap().kind(), DocumentNodeKind::Group);
        assert_eq!(tree.node(layer).unwrap().kind(), DocumentNodeKind::Layer);
        assert_eq!(tree.child_ids(tree.root_id()).unwrap(), &[group, layer]);
        assert_eq!(
            tree.append_layer(layer, ImageId::new(4)),
            Err(DocumentLayerTreeError::CannotInsertIntoLayer(layer))
        );
    }

    #[test]
    fn move_reorders_children_and_rejects_descendant_parenting() {
        let mut tree = DocumentLayerTree::new(ImageId::new(1));
        let first = tree.append_layer(tree.root_id(), ImageId::new(2)).unwrap();
        let second = tree.append_group(tree.root_id(), ImageId::new(3)).unwrap();
        let third = tree.append_layer(tree.root_id(), ImageId::new(4)).unwrap();
        let nested = tree.append_group(second, ImageId::new(5)).unwrap();

        assert_eq!(
            tree.move_node(second, nested, 0),
            Err(DocumentLayerTreeError::CannotMoveNodeIntoDescendant {
                node_id: second,
                parent_id: nested,
            })
        );
        tree.move_node(first, tree.root_id(), 3).unwrap();

        assert_eq!(
            tree.child_ids(tree.root_id()).unwrap(),
            &[second, third, first]
        );
    }

    #[test]
    fn active_ancestor_chain_tracks_moves_and_delete_fallback() {
        let mut tree = DocumentLayerTree::new(ImageId::new(1));
        let group_a = tree.append_group(tree.root_id(), ImageId::new(2)).unwrap();
        let group_b = tree.append_group(tree.root_id(), ImageId::new(3)).unwrap();
        let layer = tree.append_layer(group_a, ImageId::new(4)).unwrap();

        tree.set_active_node(layer).unwrap();
        assert_eq!(
            tree.active_ancestor_chain(),
            &[layer, group_a, tree.root_id()]
        );

        tree.move_node(group_a, group_b, 0).unwrap();
        assert_eq!(
            tree.active_ancestor_chain(),
            &[layer, group_a, group_b, tree.root_id()]
        );

        tree.delete_node(group_a).unwrap();
        assert_eq!(tree.active_node_id(), group_b);
        assert_eq!(tree.active_ancestor_chain(), &[group_b, tree.root_id()]);
        assert!(!tree.contains_node(layer));
    }

    #[test]
    fn preorder_and_path_walk_match_tree_shape() {
        let mut tree = DocumentLayerTree::new(ImageId::new(1));
        let group = tree.append_group(tree.root_id(), ImageId::new(2)).unwrap();
        let nested = tree.append_layer(group, ImageId::new(3)).unwrap();
        let sibling = tree.append_layer(tree.root_id(), ImageId::new(4)).unwrap();
        let mut output = Vec::new();

        tree.collect_subtree_preorder(tree.root_id(), &mut output)
            .unwrap();
        assert_eq!(output, vec![tree.root_id(), group, nested, sibling]);

        tree.collect_path_to_root(nested, &mut output).unwrap();
        assert_eq!(output, vec![nested, group, tree.root_id()]);
    }

    #[test]
    fn opacity_and_blend_mode_are_validated() {
        let mut tree = DocumentLayerTree::new(ImageId::new(1));
        let layer = tree.append_layer(tree.root_id(), ImageId::new(2)).unwrap();

        assert!(matches!(
            tree.set_opacity(layer, f32::NAN),
            Err(DocumentLayerTreeError::InvalidOpacity(opacity)) if opacity.is_nan()
        ));
        tree.set_opacity(layer, 0.25).unwrap();
        tree.set_blend_mode(layer, DocumentBlendMode::Multiply)
            .unwrap();

        let node = tree.node(layer).unwrap();
        assert_eq!(node.opacity(), 0.25);
        assert_eq!(node.blend_mode(), DocumentBlendMode::Multiply);
        assert_eq!(
            node.blend_mode().as_renderer_blend_mode(),
            Some(gla_color::BlendMode::Multiply)
        );
        assert_eq!(
            DocumentBlendMode::Normal.as_renderer_blend_mode(),
            Some(gla_color::BlendMode::Normal)
        );
    }
}
