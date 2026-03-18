use glaphica_core::NodeId;
use images::Image;

use crate::node::{
    branch_blend_mode_from_ui, leaf_blend_mode_from_ui, ui_blend_mode_from_branch,
    ui_blend_mode_from_leaf, BranchBlendMode, LayerMoveTarget, LeafBlendMode, SpecialLayer,
    UiBlendMode, UiBranchNode, UiLayerNode, UiLayerTreeItem, UiLeafContent, UiLeafNode, UiNodeKind,
};
use crate::LayerEditError;

pub struct UiLayerTree {
    pub root: UiLayerNode,
}

impl UiLayerTree {
    pub fn items(&self) -> Vec<UiLayerTreeItem> {
        vec![build_layer_tree_item(&self.root)]
    }

    pub fn root_id(&self) -> NodeId {
        self.root.id()
    }

    pub fn get_node(&self, node_id: NodeId) -> Option<&UiLayerNode> {
        get_node_from_node(&self.root, node_id)
    }

    pub fn contains_node(&self, node_id: NodeId) -> bool {
        self.get_node(node_id).is_some()
    }

    pub fn can_select_node(&self, node_id: NodeId) -> bool {
        match self.get_node(node_id) {
            Some(UiLayerNode::Branch(_)) => node_id != self.root_id(),
            Some(UiLayerNode::Leaf(_)) => true,
            None => false,
        }
    }

    pub fn can_paint_to_node(&self, node_id: NodeId) -> bool {
        matches!(
            self.get_node(node_id),
            Some(UiLayerNode::Leaf(UiLeafNode {
                content: UiLeafContent::Raster { .. },
                ..
            }))
        )
    }

    pub fn insert_node_above(
        &mut self,
        anchor_id: NodeId,
        new_node: UiLayerNode,
    ) -> Result<(), LayerEditError> {
        if anchor_id == self.root_id() {
            return Err(LayerEditError::RootSelectionNotAllowed);
        }
        if insert_node_above_in_branch(&mut self.root, anchor_id, new_node) {
            return Ok(());
        }
        Err(LayerEditError::InvalidNode)
    }

    pub fn move_node(&mut self, node_id: NodeId, offset: isize) -> Result<(), LayerEditError> {
        if node_id == self.root_id() {
            return Err(LayerEditError::RootSelectionNotAllowed);
        }
        match move_node_in_branch(&mut self.root, node_id, offset) {
            Some(true) => Ok(()),
            Some(false) => Err(LayerEditError::MoveOutOfBounds),
            None => Err(LayerEditError::InvalidNode),
        }
    }

    pub fn move_node_to(
        &mut self,
        node_id: NodeId,
        target: LayerMoveTarget,
    ) -> Result<(), LayerEditError> {
        if node_id == self.root_id() {
            return Err(LayerEditError::RootSelectionNotAllowed);
        }
        let moving_node = self.get_node(node_id).ok_or(LayerEditError::InvalidNode)?;
        let target_parent = self
            .get_node(target.parent_id)
            .ok_or(LayerEditError::InvalidNode)?;
        if !matches!(target_parent, UiLayerNode::Branch(_)) {
            return Err(LayerEditError::InvalidNode);
        }
        if subtree_contains_node(moving_node, target.parent_id) {
            return Err(LayerEditError::InvalidNode);
        }

        let (moved_node, source_parent_id, source_index) =
            remove_node_from_branch(&mut self.root, node_id).ok_or(LayerEditError::InvalidNode)?;
        let adjusted_index = adjust_move_index(
            source_parent_id,
            source_index,
            target.parent_id,
            target.index,
        );
        insert_node_at_parent(&mut self.root, target.parent_id, adjusted_index, moved_node)
    }

    pub fn get_leaf_image(&self, node_id: NodeId) -> Option<&Image> {
        get_leaf_image_from_node(&self.root, node_id)
    }

    pub fn get_leaf_image_mut(&mut self, node_id: NodeId) -> Option<&mut Image> {
        get_leaf_image_from_node_mut(&mut self.root, node_id)
    }

    pub fn get_solid_color(&self, node_id: NodeId) -> Option<[f32; 4]> {
        get_solid_color_from_node(&self.root, node_id)
    }

    pub fn node_opacity(&self, node_id: NodeId) -> Option<f32> {
        get_node_opacity_from_node(&self.root, node_id)
    }

    pub fn node_blend_mode(&self, node_id: NodeId) -> Option<UiBlendMode> {
        get_node_blend_mode_from_node(&self.root, node_id)
    }

    pub fn set_solid_color(&mut self, node_id: NodeId, color: [f32; 4]) -> bool {
        set_solid_color_from_node(&mut self.root, node_id, color)
    }

    pub fn set_node_visibility(
        &mut self,
        node_id: NodeId,
        visible: bool,
    ) -> Result<(), LayerEditError> {
        set_node_visibility_from_node(&mut self.root, node_id, visible)
    }

    pub fn set_node_opacity(
        &mut self,
        node_id: NodeId,
        opacity: f32,
    ) -> Result<(), LayerEditError> {
        set_node_opacity_from_node(&mut self.root, node_id, opacity)
    }

    pub fn set_node_blend_mode(
        &mut self,
        node_id: NodeId,
        blend_mode: UiBlendMode,
    ) -> Result<(), LayerEditError> {
        set_node_blend_mode_from_node(&mut self.root, node_id, blend_mode)
    }
}

fn build_layer_tree_item(node: &UiLayerNode) -> UiLayerTreeItem {
    match node {
        UiLayerNode::Branch(branch) => UiLayerTreeItem {
            id: branch.meta.id,
            label: branch.meta.label.clone(),
            visible: branch.meta.visible,
            opacity: branch.config.opacity,
            blend_mode: ui_blend_mode_from_branch(branch.config.blend_mode),
            kind: UiNodeKind::Branch,
            solid_color: None,
            children: branch.children.iter().map(build_layer_tree_item).collect(),
        },
        UiLayerNode::Leaf(leaf) => UiLayerTreeItem {
            id: leaf.meta.id,
            label: leaf.meta.label.clone(),
            visible: leaf.meta.visible,
            opacity: leaf.config.opacity,
            blend_mode: ui_blend_mode_from_leaf(leaf.config.blend_mode),
            kind: match &leaf.content {
                UiLeafContent::Raster { .. } => UiNodeKind::RasterLayer,
                UiLeafContent::Special(_) => UiNodeKind::SpecialLayer,
            },
            solid_color: match &leaf.content {
                UiLeafContent::Raster { .. } => None,
                UiLeafContent::Special(SpecialLayer::SolidColor(layer)) => Some(layer.color),
            },
            children: Vec::new(),
        },
    }
}

fn get_node_from_node(node: &UiLayerNode, node_id: NodeId) -> Option<&UiLayerNode> {
    if node.id() == node_id {
        return Some(node);
    }
    match node {
        UiLayerNode::Branch(branch) => branch
            .children
            .iter()
            .find_map(|child| get_node_from_node(child, node_id)),
        UiLayerNode::Leaf(_) => None,
    }
}

pub(crate) fn get_node_from_node_mut(
    node: &mut UiLayerNode,
    node_id: NodeId,
) -> Option<&mut UiLayerNode> {
    if node.id() == node_id {
        return Some(node);
    }
    match node {
        UiLayerNode::Branch(branch) => branch
            .children
            .iter_mut()
            .find_map(|child| get_node_from_node_mut(child, node_id)),
        UiLayerNode::Leaf(_) => None,
    }
}

fn insert_node_above_in_branch(
    node: &mut UiLayerNode,
    anchor_id: NodeId,
    new_node: UiLayerNode,
) -> bool {
    let UiLayerNode::Branch(branch) = node else {
        return false;
    };

    if let Some(index) = branch
        .children
        .iter()
        .position(|child| child.id() == anchor_id)
    {
        branch.children.insert(index + 1, new_node);
        return true;
    }

    for child in &mut branch.children {
        if insert_node_above_in_branch(child, anchor_id, new_node.clone()) {
            return true;
        }
    }

    false
}

fn move_node_in_branch(node: &mut UiLayerNode, target_id: NodeId, offset: isize) -> Option<bool> {
    let UiLayerNode::Branch(branch) = node else {
        return None;
    };

    if let Some(index) = branch
        .children
        .iter()
        .position(|child| child.id() == target_id)
    {
        let next_index = index as isize + offset;
        if !(0..branch.children.len() as isize).contains(&next_index) {
            return Some(false);
        }
        branch.children.swap(index, next_index as usize);
        return Some(true);
    }

    for child in &mut branch.children {
        if let Some(moved) = move_node_in_branch(child, target_id, offset) {
            return Some(moved);
        }
    }

    None
}

fn subtree_contains_node(node: &UiLayerNode, target_id: NodeId) -> bool {
    if node.id() == target_id {
        return true;
    }
    match node {
        UiLayerNode::Branch(branch) => branch
            .children
            .iter()
            .any(|child| subtree_contains_node(child, target_id)),
        UiLayerNode::Leaf(_) => false,
    }
}

pub(crate) fn remove_node_from_branch(
    node: &mut UiLayerNode,
    target_id: NodeId,
) -> Option<(UiLayerNode, NodeId, usize)> {
    let UiLayerNode::Branch(branch) = node else {
        return None;
    };

    if let Some(index) = branch
        .children
        .iter()
        .position(|child| child.id() == target_id)
    {
        let removed = branch.children.remove(index);
        return Some((removed, branch.meta.id, index));
    }

    for child in &mut branch.children {
        if let Some(removed) = remove_node_from_branch(child, target_id) {
            return Some(removed);
        }
    }

    None
}

fn adjust_move_index(
    source_parent_id: NodeId,
    source_index: usize,
    target_parent_id: NodeId,
    target_index: usize,
) -> usize {
    if source_parent_id == target_parent_id && source_index < target_index {
        target_index.saturating_sub(1)
    } else {
        target_index
    }
}

pub(crate) fn insert_node_at_parent(
    node: &mut UiLayerNode,
    parent_id: NodeId,
    index: usize,
    new_node: UiLayerNode,
) -> Result<(), LayerEditError> {
    let Some(parent) = get_node_from_node_mut(node, parent_id) else {
        return Err(LayerEditError::InvalidNode);
    };
    let UiLayerNode::Branch(branch) = parent else {
        return Err(LayerEditError::InvalidNode);
    };
    if index > branch.children.len() {
        return Err(LayerEditError::MoveOutOfBounds);
    }
    branch.children.insert(index, new_node);
    Ok(())
}

fn get_leaf_image_from_node(node: &UiLayerNode, node_id: NodeId) -> Option<&Image> {
    match node {
        UiLayerNode::Branch(branch) => {
            for child in &branch.children {
                if let Some(image) = get_leaf_image_from_node(child, node_id) {
                    return Some(image);
                }
            }
            None
        }
        UiLayerNode::Leaf(leaf) => {
            if leaf.meta.id == node_id {
                match &leaf.content {
                    UiLeafContent::Raster { image } => Some(image),
                    UiLeafContent::Special(_) => None,
                }
            } else {
                None
            }
        }
    }
}

fn get_leaf_image_from_node_mut(node: &mut UiLayerNode, node_id: NodeId) -> Option<&mut Image> {
    match node {
        UiLayerNode::Branch(branch) => {
            for child in &mut branch.children {
                if let Some(image) = get_leaf_image_from_node_mut(child, node_id) {
                    return Some(image);
                }
            }
            None
        }
        UiLayerNode::Leaf(leaf) => {
            if leaf.meta.id == node_id {
                match &mut leaf.content {
                    UiLeafContent::Raster { image } => Some(image),
                    UiLeafContent::Special(_) => None,
                }
            } else {
                None
            }
        }
    }
}

fn get_solid_color_from_node(node: &UiLayerNode, node_id: NodeId) -> Option<[f32; 4]> {
    match node {
        UiLayerNode::Branch(branch) => {
            for child in &branch.children {
                if let Some(color) = get_solid_color_from_node(child, node_id) {
                    return Some(color);
                }
            }
            None
        }
        UiLayerNode::Leaf(leaf) => {
            if leaf.meta.id != node_id {
                return None;
            }
            match leaf.content {
                UiLeafContent::Raster { .. } => None,
                UiLeafContent::Special(SpecialLayer::SolidColor(layer)) => Some(layer.color),
            }
        }
    }
}

fn get_node_opacity_from_node(node: &UiLayerNode, node_id: NodeId) -> Option<f32> {
    match node {
        UiLayerNode::Branch(branch) => {
            if branch.meta.id == node_id {
                return Some(branch.config.opacity);
            }
            branch
                .children
                .iter()
                .find_map(|child| get_node_opacity_from_node(child, node_id))
        }
        UiLayerNode::Leaf(leaf) => {
            if leaf.meta.id == node_id {
                Some(leaf.config.opacity)
            } else {
                None
            }
        }
    }
}

fn get_node_blend_mode_from_node(node: &UiLayerNode, node_id: NodeId) -> Option<UiBlendMode> {
    match node {
        UiLayerNode::Branch(branch) => {
            if branch.meta.id == node_id {
                return Some(ui_blend_mode_from_branch(branch.config.blend_mode));
            }
            branch
                .children
                .iter()
                .find_map(|child| get_node_blend_mode_from_node(child, node_id))
        }
        UiLayerNode::Leaf(leaf) => {
            if leaf.meta.id == node_id {
                Some(ui_blend_mode_from_leaf(leaf.config.blend_mode))
            } else {
                None
            }
        }
    }
}

fn set_solid_color_from_node(node: &mut UiLayerNode, node_id: NodeId, color: [f32; 4]) -> bool {
    match node {
        UiLayerNode::Branch(branch) => branch
            .children
            .iter_mut()
            .any(|child| set_solid_color_from_node(child, node_id, color)),
        UiLayerNode::Leaf(leaf) => {
            if leaf.meta.id != node_id {
                return false;
            }
            match &mut leaf.content {
                UiLeafContent::Raster { .. } => false,
                UiLeafContent::Special(layer) => layer.set_solid_color(color),
            }
        }
    }
}

fn set_node_visibility_from_node(
    node: &mut UiLayerNode,
    node_id: NodeId,
    visible: bool,
) -> Result<(), LayerEditError> {
    match node {
        UiLayerNode::Branch(branch) => {
            if branch.meta.id == node_id {
                branch.meta.visible = visible;
                return Ok(());
            }
            for child in &mut branch.children {
                if set_node_visibility_from_node(child, node_id, visible).is_ok() {
                    return Ok(());
                }
            }
            Err(LayerEditError::InvalidNode)
        }
        UiLayerNode::Leaf(leaf) => {
            if leaf.meta.id != node_id {
                return Err(LayerEditError::InvalidNode);
            }
            leaf.meta.visible = visible;
            Ok(())
        }
    }
}

fn set_node_opacity_from_node(
    node: &mut UiLayerNode,
    node_id: NodeId,
    opacity: f32,
) -> Result<(), LayerEditError> {
    let opacity = opacity.clamp(0.0, 1.0);
    match node {
        UiLayerNode::Branch(branch) => {
            if branch.meta.id == node_id {
                branch.config.opacity = opacity;
                return Ok(());
            }
            for child in &mut branch.children {
                if set_node_opacity_from_node(child, node_id, opacity).is_ok() {
                    return Ok(());
                }
            }
            Err(LayerEditError::InvalidNode)
        }
        UiLayerNode::Leaf(leaf) => {
            if leaf.meta.id != node_id {
                return Err(LayerEditError::InvalidNode);
            }
            leaf.config.opacity = opacity;
            Ok(())
        }
    }
}

fn set_node_blend_mode_from_node(
    node: &mut UiLayerNode,
    node_id: NodeId,
    blend_mode: UiBlendMode,
) -> Result<(), LayerEditError> {
    match node {
        UiLayerNode::Branch(branch) => {
            if branch.meta.id == node_id {
                branch.config.blend_mode = branch_blend_mode_from_ui(blend_mode);
                return Ok(());
            }
            for child in &mut branch.children {
                match set_node_blend_mode_from_node(child, node_id, blend_mode) {
                    Ok(()) => return Ok(()),
                    Err(LayerEditError::InvalidNode) => {}
                    Err(error) => return Err(error),
                }
            }
            Err(LayerEditError::InvalidNode)
        }
        UiLayerNode::Leaf(leaf) => {
            if leaf.meta.id != node_id {
                return Err(LayerEditError::InvalidNode);
            }
            let Some(leaf_blend_mode) = leaf_blend_mode_from_ui(blend_mode) else {
                return Err(LayerEditError::InvalidBlendModeForLeaf(blend_mode));
            };
            leaf.config.blend_mode = leaf_blend_mode;
            Ok(())
        }
    }
}

pub fn collect_raster_tile_keys_from_node(
    node: &UiLayerNode,
    output: &mut Vec<glaphica_core::TileKey>,
) {
    match node {
        UiLayerNode::Branch(branch) => {
            for child in &branch.children {
                collect_raster_tile_keys_from_node(child, output);
            }
        }
        UiLayerNode::Leaf(leaf) => {
            let UiLeafContent::Raster { image } = &leaf.content else {
                return;
            };
            for tile_key in image.tile_keys().iter().copied() {
                if tile_key != glaphica_core::TileKey::EMPTY {
                    output.push(tile_key);
                }
            }
        }
    }
}
