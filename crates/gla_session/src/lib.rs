use std::error::Error;
use std::fmt::{Display, Formatter};

use gla_core::CanvasInput;
use gla_nodes::gla_layout::HitMode;
use gla_nodes::{
    NodeContent, NodeContentError, NodeContentExt, NodeId, NodeKey, NodesError, NodesSession,
    NodesSessionRecord,
};
use tile_commands::TileOpRecorder;
use tile_key::{TilesError, TilesSession, TilesSessionRecord};

pub struct GlaSession<'a> {
    pub tiles: TilesSession<'a>,
    pub nodes: NodesSession<'a>,
    pub recorder: TileOpRecorder,
    pub path: Vec<NodeKey>,
    pub old_root_key: NodeKey,
    pub dirty_tiles: Vec<usize>,
}

pub struct SessionRecord {
    pub tiles: TilesSessionRecord,
    pub nodes: NodesSessionRecord,
    pub dirty_tiles: Vec<usize>,
    pub old_root_key: NodeKey,
    pub new_root_key: NodeKey,
}

#[derive(Debug)]
pub enum SessionError {
    Nodes(NodesError),
    Tiles(TilesError),
    NodeContent(NodeContentError),
    TargetNotFound { target_id: NodeId },
}

impl Display for SessionError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nodes(e) => Display::fmt(e, f),
            Self::Tiles(e) => Display::fmt(e, f),
            Self::NodeContent(e) => write!(f, "node content error: {e:?}"),
            Self::TargetNotFound { target_id } => {
                write!(f, "target node {target_id} not found in tree")
            }
        }
    }
}

impl Error for SessionError {}

impl From<NodesError> for SessionError {
    fn from(e: NodesError) -> Self {
        Self::Nodes(e)
    }
}

impl From<TilesError> for SessionError {
    fn from(e: TilesError) -> Self {
        Self::Tiles(e)
    }
}

impl From<NodeContentError> for SessionError {
    fn from(e: NodeContentError) -> Self {
        Self::NodeContent(e)
    }
}

impl<'a> GlaSession<'a> {
    pub fn new(tiles: TilesSession<'a>, nodes: NodesSession<'a>, root_key: NodeKey) -> Self {
        Self {
            tiles,
            nodes,
            recorder: TileOpRecorder::new(),
            path: vec![root_key],
            old_root_key: root_key,
            dirty_tiles: Vec::new(),
        }
    }

    pub fn apply_session_for(&mut self, target_id: NodeId) -> Result<(), SessionError> {
        let root_key = self.path[0];
        let mut old_path = Vec::new();
        if !self.find_route_from(root_key, target_id, &mut old_path) {
            return Err(SessionError::TargetNotFound { target_id });
        }
        let new_path = self.cow_cascade_up(&old_path)?;

        self.old_root_key = old_path[0];
        self.path = new_path;
        Ok(())
    }

    fn find_route_from(&self, from: NodeKey, target_id: NodeId, route: &mut Vec<NodeKey>) -> bool {
        route.push(from);
        let node = self.nodes.get_node(from).unwrap();
        if node.id == target_id {
            return true;
        }
        if let NodeContent::GlaGroup(group) = &node.content {
            for &child_key in &group.children {
                if self.find_route_from(child_key, target_id, route) {
                    return true;
                }
            }
        }
        route.pop();
        false
    }

    fn cow_cascade_up(&mut self, old_path: &[NodeKey]) -> Result<Vec<NodeKey>, SessionError> {
        let len = old_path.len();
        let mut new_path: Vec<Option<NodeKey>> = vec![None; len];

        let leaf_idx = len - 1;
        let leaf_content = self.nodes.get_node(old_path[leaf_idx])?.content.clone();
        let leaf_key = self.nodes.modify(old_path[leaf_idx], leaf_content)?;
        new_path[leaf_idx] = Some(leaf_key);

        for i in (0..leaf_idx).rev() {
            let old_child = old_path[i + 1];
            let new_child = new_path[i + 1].unwrap();
            let content = {
                let node = self.nodes.get_node(old_path[i])?;
                node.content.modify_child(old_child, new_child)?
            };
            let new_key = self.nodes.modify(old_path[i], content)?;
            new_path[i] = Some(new_key);
        }

        Ok(new_path.into_iter().map(Option::unwrap).collect())
    }

    pub fn draw(
        &mut self,
        input: &CanvasInput,
        radius: f32,
        backup_atlas_id: u8,
    ) -> Result<Vec<usize>, SessionError> {
        let len = self.path.len();
        // Walk down: transmiss_input on intermediate nodes
        let mut current_input = *input;
        for i in 0..len - 1 {
            let node = self.nodes.get_node(self.path[i])?;
            node.content.transmiss_input(&mut current_input);
        }

        let leaf_key = self.path[len - 1];
        let is_image = {
            let node = self.nodes.get_node(leaf_key)?;
            matches!(node.content, NodeContent::GlaImage(_))
        };
        if !is_image {
            return Ok(Vec::new());
        }

        let dirty = self.draw_on_image(leaf_key, &current_input, radius, backup_atlas_id)?;

        // Walk up: transmiss_dirty on intermediate nodes
        let mut dirty_range = gla_nodes::DirtyRange::Partial(dirty);
        for i in (0..len - 1).rev() {
            let node = self.nodes.get_node(self.path[i])?;
            node.content.transmiss_dirty(&mut dirty_range);
        }

        let dirty = match dirty_range {
            gla_nodes::DirtyRange::Partial(v) => v,
            gla_nodes::DirtyRange::Full => {
                // Collect all tile indices from root
                let root_node = self.nodes.get_node(self.path[0])?;
                if let NodeContent::GlaImage(img) = &root_node.content {
                    (0..img.tiles.len()).collect()
                } else {
                    Vec::new()
                }
            }
        };

        self.dirty_tiles.extend_from_slice(&dirty);
        Ok(dirty)
    }

    fn draw_on_image(
        &mut self,
        image_key: NodeKey,
        input: &CanvasInput,
        radius: f32,
        backup_atlas_id: u8,
    ) -> Result<Vec<usize>, SessionError> {
        let layout = {
            let node = self.nodes.get_node(image_key)?;
            match &node.content {
                NodeContent::GlaImage(img) => img.layout,
                _ => return Err(SessionError::NodeContent(NodeContentError::TypeMismatch)),
            }
        };

        let mut dirty = Vec::new();
        layout.for_each_affected_tile_index(
            input.position,
            radius,
            HitMode::Circle,
            |tile_index| -> Result<(), SessionError> {
                self.copy_on_write_tile(image_key, tile_index, backup_atlas_id)?;
                // TODO: gather draw commands here
                dirty.push(tile_index);
                Ok(())
            },
        )?;

        Ok(dirty)
    }

    fn copy_on_write_tile(
        &mut self,
        image_key: NodeKey,
        tile_index: usize,
        backup_atlas_id: u8,
    ) -> Result<(), SessionError> {
        let old_key = {
            let node = self.nodes.get_node(image_key)?;
            match &node.content {
                NodeContent::GlaImage(img) => img.tiles[tile_index],
                _ => return Err(SessionError::NodeContent(NodeContentError::TypeMismatch)),
            }
        };
        let new_key = self
            .tiles
            .copy_on_write(old_key, backup_atlas_id, &mut self.recorder)?;
        let node = self.nodes.get_node_mut(image_key)?;
        match &mut node.content {
            NodeContent::GlaImage(img) => img.tiles[tile_index] = new_key,
            _ => return Err(SessionError::NodeContent(NodeContentError::TypeMismatch)),
        }
        Ok(())
    }

    pub fn finish(self) -> SessionRecord {
        SessionRecord {
            tiles: self.tiles.record(),
            nodes: self.nodes.record(),
            dirty_tiles: self.dirty_tiles,
            old_root_key: self.old_root_key,
            new_root_key: self.path[0],
        }
    }
}
