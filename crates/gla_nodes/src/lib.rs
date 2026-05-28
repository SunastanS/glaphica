use gla_color::BlendMode;
use gla_core::{Pool, PoolError};
use std::fmt::{Display, Formatter};

pub type NodeId = u32;

#[derive(Debug, Clone, Copy)]
pub struct RenderConfig {
    pub opacity: f32, // [0,1]
    pub blend_mode: BlendMode,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
        }
    }
}

pub mod gla_layout;
pub use gla_layout::GlaImageLayout;

#[derive(Debug, Clone)]
pub struct Node {
    // different from NodeKey, Key will change for every modify
    pub id: NodeId,
    pub config: RenderConfig,
    pub content: NodeContent,
}

pub mod gla_image;
pub use gla_image::GlaImage;

pub mod gla_group;
pub use gla_group::{GlaGroup, GlaGroupError, GlaGroupError::ChildNotFound};

#[derive(Debug, Clone)]
pub enum NodeContent {
    GlaImage(GlaImage),
    GlaGroup(GlaGroup),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeContentError {
    TypeMismatch,
    GlaGroupError(GlaGroupError),
}

impl NodeContent {
    pub fn modify_child(&self, target: NodeKey, new: NodeKey) -> Result<Self, NodeContentError> {
        match self {
            Self::GlaGroup(group) => group
                .switch_child(target, new)
                .map(Self::GlaGroup)
                .map_err(NodeContentError::GlaGroupError),
            Self::GlaImage(_) => Err(NodeContentError::TypeMismatch),
        }
    }
}

pub enum DirtyRange {
    Full,
    Partial(Vec<usize>), // vec of indices
}

pub trait NodeContentExt {
    fn transmiss_input(&self, _input: &mut gla_core::CanvasInput) {}
    fn transmiss_dirty(&self, _dirty: &mut DirtyRange) {}
}

impl NodeContentExt for NodeContent {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeKey {
    pub index: u32,
    pub generation: u32,
}

#[derive(Debug)]
pub struct Nodes {
    node_key_pool: Pool,
    pub nodes: Vec<Node>, //key as index
    next_id: NodeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodesError {
    KeyPoolFull,
    InvalidKey,
}

impl Display for NodesError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KeyPoolFull => f.write_str("node key pool is full"),
            Self::InvalidKey => f.write_str("invalid node key"),
        }
    }
}

impl Nodes {
    pub fn new() -> Self {
        Self {
            node_key_pool: Pool::new(u32::MAX),
            nodes: Vec::new(),
            next_id: 0,
        }
    }

    pub fn add_node(&mut self, content: NodeContent) -> Result<NodeKey, NodesError> {
        let id = self.next_id;
        self.next_id += 1;
        let (index, generation) = self.node_key_pool.alloc()?;
        let key = NodeKey { index, generation };
        self.nodes[index as usize] = Node {
            id,
            config: RenderConfig::default(),
            content,
        };
        Ok(key)
    }

    pub fn ensure_key(&self, key: NodeKey) -> Result<(), NodesError> {
        (key.index as usize <= self.nodes.len()
            && self.node_key_pool.check(key.index, key.generation))
        .then_some(())
        .ok_or(NodesError::InvalidKey)
    }

    pub fn get_node(&self, key: NodeKey) -> Result<&Node, NodesError> {
        self.ensure_key(key)?;
        Ok(&self.nodes[key.index as usize])
    }

    pub fn get_node_mut(&mut self, key: NodeKey) -> Result<&mut Node, NodesError> {
        self.ensure_key(key)?;
        Ok(&mut self.nodes[key.index as usize])
    }

    pub fn modify(&mut self, key: NodeKey, content: NodeContent) -> Result<NodeKey, NodesError> {
        self.ensure_key(key)?;
        let (index, generation) = self.node_key_pool.alloc()?;
        self.nodes[index as usize] = Node {
            id: self.nodes[key.index as usize].id,
            config: self.nodes[key.index as usize].config,
            content,
        };
        Ok(NodeKey { index, generation })
    }

    pub fn discard_node(&mut self, key: NodeKey) -> Result<(), NodesError> {
        self.ensure_key(key)?;
        self.node_key_pool.free(key.index);
        Ok(())
    }

    pub fn clone(&mut self, key: NodeKey) -> Result<NodeKey, NodesError> {
        self.ensure_key(key)?;
        let (index, generation) = self.node_key_pool.alloc()?;
        self.nodes[index as usize] = self.nodes[key.index as usize].clone();
        Ok(NodeKey { index, generation })
    }
}

pub struct NodesSession<'a> {
    nodes: &'a mut Nodes,
    pub allocated: Vec<NodeKey>,
    pub discarded: Vec<NodeKey>,
}

impl<'a> NodesSession<'a> {
    pub fn new(nodes: &'a mut Nodes) -> Self {
        Self {
            nodes,
            allocated: Vec::new(),
            discarded: Vec::new(),
        }
    }

    pub fn add_node(&mut self, content: NodeContent) -> Result<NodeKey, NodesError> {
        let key = self.nodes.add_node(content)?;
        self.allocated.push(key);
        Ok(key)
    }

    pub fn discard_node(&mut self, key: NodeKey) -> Result<(), NodesError> {
        self.discarded.push(key);
        Ok(())
    }

    pub fn get_node(&self, key: NodeKey) -> Result<&Node, NodesError> {
        self.nodes.get_node(key)
    }

    pub fn get_node_mut(&mut self, key: NodeKey) -> Result<&mut Node, NodesError> {
        self.nodes.get_node_mut(key)
    }

    pub fn modify(&mut self, key: NodeKey, content: NodeContent) -> Result<NodeKey, NodesError> {
        let new_key = self.nodes.modify(key, content)?;
        self.allocated.push(new_key);
        self.discarded.push(key);
        Ok(new_key)
    }

    pub fn record(&self) -> NodesSessionRecord {
        NodesSessionRecord {
            allocated: self.allocated.clone().into_boxed_slice(),
            discarded: self.discarded.clone().into_boxed_slice(),
        }
    }
}

pub struct NodesSessionRecord {
    pub allocated: Box<[NodeKey]>,
    pub discarded: Box<[NodeKey]>,
}

impl From<PoolError> for NodesError {
    fn from(error: PoolError) -> Self {
        match error {
            PoolError::Full => Self::KeyPoolFull,
        }
    }
}
