use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasError, Backend, BackendId};
use gla_image::{GlaImage, GlaImageCreateError, GlaImageLayout, GlaImageTileAccessError};
use gla_undo::{GlaImageUndo, GlaImageUndoError, GlaImageUndoTileRecord};
use glaphica_core::BlendMode;
use slotmap::SlotMap;
use smallvec::SmallVec;

use crate::backup::{DocumentUndoStack, GlaDocUndoRestore};
use crate::node::{GlaNode, GlaNodeId};

pub struct GlaDoc {
    layout: GlaImageLayout,
    image_backend: Backend,
    image_backend_id: BackendId,
    render_backend: Backend,
    render_backend_id: BackendId,
    image_undo: GlaImageUndo,
    undo_stack: DocumentUndoStack,
    root_id: GlaNodeId,
    active_layer_id: GlaNodeId,
    active_layer_ancestor_chain: SmallVec<[GlaNodeId; 8]>,
    nodes: SlotMap<GlaNodeId, GlaNode>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GlaDocError {
    InvalidNodeId(GlaNodeId),
    InvalidTileIndex {
        tile_index: usize,
        tile_count: usize,
    },
    CannotInsertIntoLeaf(GlaNodeId),
    ChildIndexOutOfBounds {
        parent_id: GlaNodeId,
        child_count: usize,
        index: usize,
    },
    CannotMoveRoot,
    CannotDeleteRoot,
    CannotMoveNodeIntoDescendant {
        node_id: GlaNodeId,
        parent_id: GlaNodeId,
    },
    BrokenParentChildLink {
        parent_id: GlaNodeId,
        child_id: GlaNodeId,
    },
    InvalidOpacity(f32),
    Atlas(AtlasError),
    ImageCreate(GlaImageCreateError),
}

#[derive(Debug, Clone, PartialEq)]
pub enum GlaDocUndoError {
    Document(GlaDocError),
    Atlas(AtlasError),
    Image(GlaImageTileAccessError),
    ImageUndo(GlaImageUndoError),
}

impl Display for GlaDocError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidNodeId(node_id) => write!(f, "invalid node id {node_id:?}"),
            Self::InvalidTileIndex {
                tile_index,
                tile_count,
            } => write!(
                f,
                "tile index {tile_index} is out of bounds for image with {tile_count} tiles"
            ),
            Self::CannotInsertIntoLeaf(node_id) => {
                write!(f, "cannot insert children into leaf node {node_id:?}")
            }
            Self::ChildIndexOutOfBounds {
                parent_id,
                child_count,
                index,
            } => write!(
                f,
                "child index {index} is out of bounds for parent {parent_id:?} with {child_count} children"
            ),
            Self::CannotMoveRoot => write!(f, "cannot move the root node"),
            Self::CannotDeleteRoot => write!(f, "cannot delete the root node"),
            Self::CannotMoveNodeIntoDescendant { node_id, parent_id } => write!(
                f,
                "cannot move node {node_id:?} into its descendant {parent_id:?}"
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
            Self::Atlas(err) => Display::fmt(err, f),
            Self::ImageCreate(err) => Display::fmt(err, f),
        }
    }
}

impl Error for GlaDocError {}

impl From<GlaImageCreateError> for GlaDocError {
    fn from(error: GlaImageCreateError) -> Self {
        Self::ImageCreate(error)
    }
}

impl From<AtlasError> for GlaDocError {
    fn from(error: AtlasError) -> Self {
        Self::Atlas(error)
    }
}

impl Display for GlaDocUndoError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Document(error) => Display::fmt(error, f),
            Self::Atlas(error) => Display::fmt(error, f),
            Self::Image(error) => Display::fmt(error, f),
            Self::ImageUndo(error) => Display::fmt(error, f),
        }
    }
}

impl Error for GlaDocUndoError {}

impl From<GlaDocError> for GlaDocUndoError {
    fn from(error: GlaDocError) -> Self {
        Self::Document(error)
    }
}

impl From<AtlasError> for GlaDocUndoError {
    fn from(error: AtlasError) -> Self {
        Self::Atlas(error)
    }
}

impl From<GlaImageTileAccessError> for GlaDocUndoError {
    fn from(error: GlaImageTileAccessError) -> Self {
        Self::Image(error)
    }
}

impl From<GlaImageUndoError> for GlaDocUndoError {
    fn from(error: GlaImageUndoError) -> Self {
        Self::ImageUndo(error)
    }
}

impl GlaDoc {
    pub fn new(
        layout: GlaImageLayout,
        image_backend: Backend,
        render_backend: Backend,
        backup_backend: Backend,
    ) -> Result<Self, GlaDocError> {
        let image_backend_id = image_backend.backend_id();
        let render_backend_id = render_backend.backend_id();
        let mut nodes = SlotMap::with_key();
        let root_image = GlaImage::new(layout, render_backend.clone())?;
        let root_id = nodes.insert(GlaNode::new_root(root_image, 1.0, BlendMode::Normal));
        let mut active_layer_ancestor_chain = SmallVec::new();
        active_layer_ancestor_chain.push(root_id);
        let image_undo = GlaImageUndo::new(image_backend.clone(), backup_backend);
        let undo_stack = DocumentUndoStack::new();

        Ok(Self {
            layout,
            image_backend,
            image_backend_id,
            render_backend,
            render_backend_id,
            image_undo,
            undo_stack,
            root_id,
            active_layer_id: root_id,
            active_layer_ancestor_chain,
            nodes,
        })
    }

    pub fn layout(&self) -> GlaImageLayout {
        self.layout
    }

    pub fn image_backend(&self) -> BackendId {
        self.image_backend_id
    }

    pub fn image_backend_ref(&self) -> &Backend {
        &self.image_backend
    }

    pub fn render_backend(&self) -> BackendId {
        self.render_backend_id
    }

    pub fn render_backend_ref(&self) -> &Backend {
        &self.render_backend
    }

    pub fn image_undo(&self) -> &GlaImageUndo {
        &self.image_undo
    }

    pub fn undo_stack(&self) -> &DocumentUndoStack {
        &self.undo_stack
    }

    pub fn undo_stack_mut(&mut self) -> &mut DocumentUndoStack {
        &mut self.undo_stack
    }

    pub fn root_id(&self) -> GlaNodeId {
        self.root_id
    }

    pub fn active_layer_id(&self) -> GlaNodeId {
        self.active_layer_id
    }

    pub fn active_layer(&self) -> Result<&GlaNode, GlaDocError> {
        self.node(self.active_layer_id)
    }

    pub fn active_layer_image(&self) -> Result<&GlaImage, GlaDocError> {
        self.node_image(self.active_layer_id)
    }

    pub fn active_layer_image_mut(&mut self) -> Result<&mut GlaImage, GlaDocError> {
        self.node_image_mut(self.active_layer_id)
    }

    pub fn active_layer_ancestor_chain(&self) -> &[GlaNodeId] {
        self.active_layer_ancestor_chain.as_slice()
    }

    pub fn set_active_layer(&mut self, node_id: GlaNodeId) -> Result<(), GlaDocError> {
        let ancestor_chain = self.build_path_to_root(node_id)?;
        self.active_layer_id = node_id;
        self.active_layer_ancestor_chain = ancestor_chain;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn contains_node(&self, node_id: GlaNodeId) -> bool {
        self.nodes.contains_key(node_id)
    }

    pub fn node(&self, node_id: GlaNodeId) -> Result<&GlaNode, GlaDocError> {
        self.nodes
            .get(node_id)
            .ok_or(GlaDocError::InvalidNodeId(node_id))
    }

    pub fn node_image(&self, node_id: GlaNodeId) -> Result<&GlaImage, GlaDocError> {
        Ok(self.node(node_id)?.image())
    }

    pub fn node_image_mut(&mut self, node_id: GlaNodeId) -> Result<&mut GlaImage, GlaDocError> {
        Ok(self.node_mut(node_id)?.image_mut())
    }

    pub fn child_ids(&self, parent_id: GlaNodeId) -> Result<&[GlaNodeId], GlaDocError> {
        self.node(parent_id)?
            .children()
            .ok_or(GlaDocError::CannotInsertIntoLeaf(parent_id))
    }

    pub fn collect_path_to_root(
        &self,
        start_id: GlaNodeId,
        output: &mut Vec<GlaNodeId>,
    ) -> Result<(), GlaDocError> {
        let path = self.build_path_to_root(start_id)?;
        output.clear();
        output.extend(path);
        Ok(())
    }

    pub fn set_opacity(&mut self, node_id: GlaNodeId, opacity: f32) -> Result<(), GlaDocError> {
        let opacity = validate_opacity(opacity)?;
        self.node_mut(node_id)?.set_opacity(opacity);
        Ok(())
    }

    pub fn set_blend_mode(
        &mut self,
        node_id: GlaNodeId,
        blend_mode: BlendMode,
    ) -> Result<(), GlaDocError> {
        self.node_mut(node_id)?.set_blend_mode(blend_mode);
        Ok(())
    }

    pub fn append_layer(&mut self, parent_id: GlaNodeId) -> Result<GlaNodeId, GlaDocError> {
        let child_count = self.child_ids(parent_id)?.len();
        self.insert_layer(parent_id, child_count)
    }

    pub fn append_group(&mut self, parent_id: GlaNodeId) -> Result<GlaNodeId, GlaDocError> {
        let child_count = self.child_ids(parent_id)?.len();
        self.insert_group(parent_id, child_count)
    }

    pub fn insert_layer(
        &mut self,
        parent_id: GlaNodeId,
        index: usize,
    ) -> Result<GlaNodeId, GlaDocError> {
        let image = GlaImage::new(self.layout, self.image_backend.clone())?;
        let node = GlaNode::new_leaf(parent_id, image, 1.0, BlendMode::Normal);
        self.insert_node(parent_id, index, node)
    }

    pub fn insert_group(
        &mut self,
        parent_id: GlaNodeId,
        index: usize,
    ) -> Result<GlaNodeId, GlaDocError> {
        let image = GlaImage::new(self.layout, self.render_backend.clone())?;
        let node = GlaNode::new_branch(parent_id, image, 1.0, BlendMode::Normal);
        self.insert_node(parent_id, index, node)
    }

    pub fn move_node(
        &mut self,
        node_id: GlaNodeId,
        new_parent_id: GlaNodeId,
        new_index: usize,
    ) -> Result<(), GlaDocError> {
        if node_id == self.root_id {
            return Err(GlaDocError::CannotMoveRoot);
        }

        let old_parent_id = self
            .node(node_id)?
            .parent()
            .ok_or(GlaDocError::CannotMoveRoot)?;
        let moved_active_subtree =
            old_parent_id != new_parent_id && self.is_ancestor(node_id, self.active_layer_id)?;
        self.validate_insert_target(new_parent_id, new_index)?;

        if self.is_ancestor(node_id, new_parent_id)? {
            return Err(GlaDocError::CannotMoveNodeIntoDescendant {
                node_id,
                parent_id: new_parent_id,
            });
        }

        let old_index = self.find_child_index(old_parent_id, node_id)?;
        if old_parent_id == new_parent_id {
            let parent = self.node_mut(old_parent_id)?;
            let children = parent
                .children_mut()
                .ok_or(GlaDocError::CannotInsertIntoLeaf(old_parent_id))?;
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
                .ok_or(GlaDocError::CannotInsertIntoLeaf(old_parent_id))?;
            children.remove(old_index);
        }

        {
            let new_parent = self.node_mut(new_parent_id)?;
            let children = new_parent
                .children_mut()
                .ok_or(GlaDocError::CannotInsertIntoLeaf(new_parent_id))?;
            children.insert(new_index, node_id);
        }

        self.node_mut(node_id)?.set_parent(Some(new_parent_id));
        if moved_active_subtree {
            self.refresh_active_layer_ancestor_chain()?;
        }
        Ok(())
    }

    pub fn delete_node(&mut self, node_id: GlaNodeId) -> Result<(), GlaDocError> {
        if node_id == self.root_id {
            return Err(GlaDocError::CannotDeleteRoot);
        }

        let parent_id = self
            .node(node_id)?
            .parent()
            .ok_or(GlaDocError::CannotDeleteRoot)?;
        let child_index = self.find_child_index(parent_id, node_id)?;
        let subtree = self.collect_subtree_postorder(node_id)?;
        let active_layer_in_deleted_subtree = subtree.contains(&self.active_layer_id);

        {
            let parent = self.node_mut(parent_id)?;
            let children = parent
                .children_mut()
                .ok_or(GlaDocError::CannotInsertIntoLeaf(parent_id))?;
            children.remove(child_index);
        }

        for descendant_id in subtree {
            self.free_node(descendant_id)?;
        }

        if active_layer_in_deleted_subtree {
            self.active_layer_id = parent_id;
            self.refresh_active_layer_ancestor_chain()?;
        }

        Ok(())
    }

    pub fn collect_subtree_preorder(
        &self,
        root_id: GlaNodeId,
        output: &mut Vec<GlaNodeId>,
    ) -> Result<(), GlaDocError> {
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

    pub fn resize_anchored_top_left(
        &mut self,
        new_layout: GlaImageLayout,
    ) -> Result<(), GlaDocError> {
        if self.layout == new_layout {
            return Ok(());
        }

        for node in self.nodes.values_mut() {
            node.image_mut().resize_anchored_top_left(new_layout)?;
        }
        self.layout = new_layout;
        Ok(())
    }

    pub fn push_undo_entry(
        &mut self,
        node_id: GlaNodeId,
        backup_group: atlas::CachedTileGroup,
        tile_records: Vec<GlaImageUndoTileRecord>,
    ) -> Result<(), GlaDocError> {
        self.node(node_id)?;
        self.undo_stack
            .push_entry(node_id, backup_group, tile_records);
        Ok(())
    }

    pub fn push_active_layer_undo_entry(
        &mut self,
        backup_group: atlas::CachedTileGroup,
        tile_records: Vec<GlaImageUndoTileRecord>,
    ) {
        self.undo_stack
            .push_entry(self.active_layer_id, backup_group, tile_records);
    }

    pub fn restore_last_undo(&mut self) -> Result<Option<GlaDocUndoRestore>, GlaDocUndoError> {
        let Some(entry) = self.undo_stack.pop_entry() else {
            return Ok(None);
        };

        let image_undo = self.image_undo.clone();
        let image_restore = image_undo.restore_tiles(
            self.node_image_mut(entry.node_id())?,
            entry.backup_group().clone(),
            entry.tile_records(),
        )?;

        Ok(Some(GlaDocUndoRestore {
            node_id: entry.node_id(),
            image_restore,
        }))
    }

    fn insert_node(
        &mut self,
        parent_id: GlaNodeId,
        index: usize,
        node: GlaNode,
    ) -> Result<GlaNodeId, GlaDocError> {
        self.validate_insert_target(parent_id, index)?;
        let node_id = self.nodes.insert(node);

        let parent = self.node_mut(parent_id)?;
        let children = parent
            .children_mut()
            .ok_or(GlaDocError::CannotInsertIntoLeaf(parent_id))?;
        children.insert(index, node_id);
        Ok(node_id)
    }

    fn validate_insert_target(
        &self,
        parent_id: GlaNodeId,
        index: usize,
    ) -> Result<(), GlaDocError> {
        let children = self
            .node(parent_id)?
            .children()
            .ok_or(GlaDocError::CannotInsertIntoLeaf(parent_id))?;
        if index > children.len() {
            return Err(GlaDocError::ChildIndexOutOfBounds {
                parent_id,
                child_count: children.len(),
                index,
            });
        }
        Ok(())
    }

    fn node_mut(&mut self, node_id: GlaNodeId) -> Result<&mut GlaNode, GlaDocError> {
        self.nodes
            .get_mut(node_id)
            .ok_or(GlaDocError::InvalidNodeId(node_id))
    }

    fn free_node(&mut self, node_id: GlaNodeId) -> Result<(), GlaDocError> {
        self.nodes
            .remove(node_id)
            .map(|_| ())
            .ok_or(GlaDocError::InvalidNodeId(node_id))
    }

    fn build_path_to_root(
        &self,
        start_id: GlaNodeId,
    ) -> Result<SmallVec<[GlaNodeId; 8]>, GlaDocError> {
        self.node(start_id)?;
        let mut output = SmallVec::new();
        let mut current = Some(start_id);
        while let Some(node_id) = current {
            output.push(node_id);
            current = self.node(node_id)?.parent();
        }
        Ok(output)
    }

    fn refresh_active_layer_ancestor_chain(&mut self) -> Result<(), GlaDocError> {
        self.active_layer_ancestor_chain = self.build_path_to_root(self.active_layer_id)?;
        Ok(())
    }

    fn collect_subtree_postorder(&self, root_id: GlaNodeId) -> Result<Vec<GlaNodeId>, GlaDocError> {
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
        parent_id: GlaNodeId,
        child_id: GlaNodeId,
    ) -> Result<usize, GlaDocError> {
        let children = self
            .node(parent_id)?
            .children()
            .ok_or(GlaDocError::CannotInsertIntoLeaf(parent_id))?;
        children
            .iter()
            .position(|candidate| *candidate == child_id)
            .ok_or(GlaDocError::BrokenParentChildLink {
                parent_id,
                child_id,
            })
    }

    pub(crate) fn is_ancestor(
        &self,
        ancestor_id: GlaNodeId,
        node_id: GlaNodeId,
    ) -> Result<bool, GlaDocError> {
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

fn validate_opacity(opacity: f32) -> Result<f32, GlaDocError> {
    if opacity.is_finite() && (0.0..=1.0).contains(&opacity) {
        return Ok(opacity);
    }
    Err(GlaDocError::InvalidOpacity(opacity))
}

#[cfg(test)]
mod tests {
    use atlas::{AtlasLayout, Backend, BackendId, TileKey};
    use glaphica_core::{CopyTileCommand, IMAGE_TILE_SIZE};

    use crate::{
        GlaDoc, GlaDocError, GlaImageLayout, GlaImageUndoTileAction, GlaImageUndoTileRecord,
        GlaNodeKind,
    };
    use glaphica_core::BlendMode;

    fn new_doc() -> GlaDoc {
        match GlaDoc::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE),
            Backend::new(AtlasLayout::Tiny8, BackendId::new(3)),
            Backend::new(AtlasLayout::Tiny8, BackendId::new(7)),
            Backend::new(AtlasLayout::Tiny8, BackendId::new(11)),
        ) {
            Ok(doc) => doc,
            Err(err) => panic!("failed to build document: {err}"),
        }
    }

    #[test]
    fn root_is_root_node_with_render_backend() {
        let doc = new_doc();
        let root = match doc.node(doc.root_id()) {
            Ok(node) => node,
            Err(err) => panic!("missing root node: {err}"),
        };

        assert_eq!(doc.active_layer_id(), doc.root_id());
        assert_eq!(doc.active_layer_ancestor_chain(), &[doc.root_id()]);
        assert_eq!(root.kind(), GlaNodeKind::Root);
        assert_eq!(root.image().backend_id(), BackendId::new(7));
        assert_eq!(root.opacity(), 1.0);
        assert_eq!(root.blend_mode(), BlendMode::Normal);
    }

    #[test]
    fn inserted_nodes_use_backends_for_their_kind() {
        let mut doc = new_doc();
        let group_id = match doc.append_group(doc.root_id()) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append group: {err}"),
        };
        let layer_id = match doc.append_layer(doc.root_id()) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append layer: {err}"),
        };

        let group = match doc.node(group_id) {
            Ok(node) => node,
            Err(err) => panic!("missing group node: {err}"),
        };
        let layer = match doc.node(layer_id) {
            Ok(node) => node,
            Err(err) => panic!("missing layer node: {err}"),
        };

        assert_eq!(group.kind(), GlaNodeKind::Branch);
        assert_eq!(group.image().backend_id(), BackendId::new(7));
        assert_eq!(layer.kind(), GlaNodeKind::Leaf);
        assert_eq!(layer.image().backend_id(), BackendId::new(3));
    }

    #[test]
    fn cannot_insert_child_into_leaf() {
        let mut doc = new_doc();
        let layer_id = match doc.append_layer(doc.root_id()) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append layer: {err}"),
        };

        let inserted = doc.append_layer(layer_id);

        assert_eq!(inserted, Err(GlaDocError::CannotInsertIntoLeaf(layer_id)));
    }

    #[test]
    fn move_reorders_children_within_same_parent() {
        let mut doc = new_doc();
        let first = match doc.append_layer(doc.root_id()) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append first layer: {err}"),
        };
        let second = match doc.append_layer(doc.root_id()) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append second layer: {err}"),
        };
        let third = match doc.append_layer(doc.root_id()) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append third layer: {err}"),
        };

        let moved = doc.move_node(first, doc.root_id(), 3);
        assert_eq!(moved, Ok(()));

        let children = match doc.child_ids(doc.root_id()) {
            Ok(children) => children,
            Err(err) => panic!("failed to read root children: {err}"),
        };
        assert_eq!(children, &[second, third, first]);
    }

    #[test]
    fn move_rejects_descendant_reparenting() {
        let mut doc = new_doc();
        let group_id = match doc.append_group(doc.root_id()) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append group: {err}"),
        };
        let child_group_id = match doc.append_group(group_id) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append child group: {err}"),
        };

        let moved = doc.move_node(group_id, child_group_id, 0);

        assert_eq!(
            moved,
            Err(GlaDocError::CannotMoveNodeIntoDescendant {
                node_id: group_id,
                parent_id: child_group_id,
            })
        );
    }

    #[test]
    fn delete_invalidates_removed_ids() {
        let mut doc = new_doc();
        let group_id = match doc.append_group(doc.root_id()) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append group: {err}"),
        };
        let layer_id = match doc.append_layer(group_id) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append layer: {err}"),
        };

        let deleted = doc.delete_node(group_id);
        assert_eq!(deleted, Ok(()));
        assert!(!doc.contains_node(group_id));
        assert!(!doc.contains_node(layer_id));
    }

    #[test]
    fn resize_updates_every_node_image_layout() {
        let mut doc = new_doc();
        let group_id = match doc.append_group(doc.root_id()) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append group: {err}"),
        };
        let layer_id = match doc.append_layer(group_id) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append layer: {err}"),
        };
        let new_layout = GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE * 3);

        let resized = doc.resize_anchored_top_left(new_layout);
        assert_eq!(resized, Ok(()));

        assert_eq!(doc.layout(), new_layout);
        for node_id in [doc.root_id(), group_id, layer_id] {
            let node = match doc.node(node_id) {
                Ok(node) => node,
                Err(err) => panic!("missing node after resize: {err}"),
            };
            assert_eq!(*node.image().layout(), new_layout);
        }
    }

    #[test]
    fn opacity_must_be_normalized() {
        let mut doc = new_doc();

        let updated = doc.set_opacity(doc.root_id(), 1.5);

        assert_eq!(updated, Err(GlaDocError::InvalidOpacity(1.5)));
    }

    #[test]
    fn active_layer_can_be_set_to_any_existing_node() {
        let mut doc = new_doc();
        let group = match doc.append_group(doc.root_id()) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append group: {err}"),
        };
        let layer = match doc.append_layer(group) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append layer: {err}"),
        };

        let selected_group = doc.set_active_layer(group);
        assert_eq!(selected_group, Ok(()));
        assert_eq!(doc.active_layer_id(), group);
        assert_eq!(doc.active_layer_ancestor_chain(), &[group, doc.root_id()]);

        let selected_layer = doc.set_active_layer(layer);
        assert_eq!(selected_layer, Ok(()));
        assert_eq!(doc.active_layer_id(), layer);
        assert_eq!(
            doc.active_layer_ancestor_chain(),
            &[layer, group, doc.root_id()]
        );
    }

    #[test]
    fn active_layer_image_helpers_follow_active_layer_selection() {
        let mut doc = new_doc();
        let layer = match doc.append_layer(doc.root_id()) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append layer: {err}"),
        };

        let selected_layer = doc.set_active_layer(layer);
        assert_eq!(selected_layer, Ok(()));
        assert_eq!(
            doc.active_layer_image().map(|image| image.backend_id()),
            Ok(doc.image_backend())
        );
        assert_eq!(
            doc.active_layer_image_mut().map(|image| image.backend_id()),
            Ok(doc.image_backend())
        );
    }

    #[test]
    fn preorder_walk_matches_tree_shape() {
        let mut doc = new_doc();
        let group_a = match doc.append_group(doc.root_id()) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append group_a: {err}"),
        };
        let layer_a = match doc.append_layer(group_a) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append layer_a: {err}"),
        };
        let layer_b = match doc.append_layer(doc.root_id()) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append layer_b: {err}"),
        };
        let mut order = Vec::new();

        let collected = doc.collect_subtree_preorder(doc.root_id(), &mut order);
        assert_eq!(collected, Ok(()));
        assert_eq!(order, vec![doc.root_id(), group_a, layer_a, layer_b]);
    }

    #[test]
    fn path_to_root_walks_upward_from_leaf() {
        let mut doc = new_doc();
        let group = match doc.append_group(doc.root_id()) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append group: {err}"),
        };
        let leaf = match doc.append_layer(group) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append leaf: {err}"),
        };
        let mut path = Vec::new();

        let collected = doc.collect_path_to_root(leaf, &mut path);

        assert_eq!(collected, Ok(()));
        assert_eq!(path, vec![leaf, group, doc.root_id()]);
    }

    #[test]
    fn deleting_active_subtree_falls_back_to_surviving_parent() {
        let mut doc = new_doc();
        let group = match doc.append_group(doc.root_id()) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append group: {err}"),
        };
        let leaf = match doc.append_layer(group) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append leaf: {err}"),
        };

        let selected = doc.set_active_layer(leaf);
        assert_eq!(selected, Ok(()));

        let deleted = doc.delete_node(group);
        assert_eq!(deleted, Ok(()));
        assert_eq!(doc.active_layer_id(), doc.root_id());
        assert_eq!(doc.active_layer_ancestor_chain(), &[doc.root_id()]);
    }

    #[test]
    fn moving_active_subtree_refreshes_cached_ancestor_chain() {
        let mut doc = new_doc();
        let group_a = match doc.append_group(doc.root_id()) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append group_a: {err}"),
        };
        let group_b = match doc.append_group(doc.root_id()) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append group_b: {err}"),
        };
        let leaf = match doc.append_layer(group_a) {
            Ok(node_id) => node_id,
            Err(err) => panic!("failed to append leaf: {err}"),
        };

        let selected = doc.set_active_layer(leaf);
        assert_eq!(selected, Ok(()));

        let moved = doc.move_node(group_a, group_b, 0);
        assert_eq!(moved, Ok(()));
        assert_eq!(
            doc.active_layer_ancestor_chain(),
            &[leaf, group_a, group_b, doc.root_id()]
        );
    }

    #[test]
    fn restore_last_undo_reuses_existing_image_tile_for_backup_restore() {
        let image_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(3));
        let render_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(7));
        let backup_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(11));
        let mut doc = GlaDoc::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE),
            image_backend.clone(),
            render_backend,
            backup_backend.clone(),
        )
        .expect("document should build");
        let layer_id = doc
            .append_layer(doc.root_id())
            .expect("layer should append");
        doc.set_active_layer(layer_id)
            .expect("active layer should update");

        let current_tile = image_backend
            .alloc_active()
            .expect("active tile should allocate");
        let current_tile_key = current_tile.tile_key();
        doc.active_layer_image_mut()
            .expect("active image should exist")
            .replace_tile_owner(0, current_tile)
            .expect("tile owner should install");

        let backup_group = doc
            .image_undo()
            .backup_backend()
            .alloc_cached(1)
            .expect("backup group should allocate");
        let backup_tile_key = backup_group.keys()[0];
        doc.push_active_layer_undo_entry(
            backup_group,
            vec![GlaImageUndoTileRecord::new(0, Some(backup_tile_key))],
        );

        let restore = doc
            .restore_last_undo()
            .expect("undo should restore")
            .expect("undo entry should exist");

        assert_eq!(restore.node_id(), layer_id);
        assert_eq!(
            restore.image_restore().tile_actions(),
            &[GlaImageUndoTileAction::RestoreFromBackup {
                tile_index: 0,
                copy_command: CopyTileCommand {
                    source_tile_key: backup_tile_key,
                    destination_tile_key: current_tile_key,
                },
            }]
        );
    }

    #[test]
    fn restore_last_undo_clears_tile_when_previous_state_was_empty() {
        let image_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(3));
        let render_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(7));
        let backup_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(11));
        let mut doc = GlaDoc::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE),
            image_backend.clone(),
            render_backend,
            backup_backend,
        )
        .expect("document should build");
        let layer_id = doc
            .append_layer(doc.root_id())
            .expect("layer should append");
        doc.set_active_layer(layer_id)
            .expect("active layer should update");

        let current_tile = image_backend
            .alloc_active()
            .expect("active tile should allocate");
        doc.active_layer_image_mut()
            .expect("active image should exist")
            .replace_tile_owner(0, current_tile)
            .expect("tile owner should install");
        let empty_backup_group = doc
            .image_undo()
            .backup_backend()
            .create_cached_group()
            .expect("empty backup group should allocate");
        doc.push_active_layer_undo_entry(
            empty_backup_group,
            vec![GlaImageUndoTileRecord::new(0, None)],
        );

        let restore = doc
            .restore_last_undo()
            .expect("undo should restore")
            .expect("undo entry should exist");

        assert_eq!(
            restore.image_restore().tile_actions(),
            &[GlaImageUndoTileAction::Clear { tile_index: 0 }]
        );
        assert_eq!(
            doc.active_layer_image()
                .expect("active image should exist")
                .tile_key(0),
            Ok(TileKey::empty(doc.image_backend()))
        );
    }
}
