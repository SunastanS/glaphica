use crate::{GlaImageLayout, NodeKey};

#[derive(Debug, Clone)]
pub struct GlaGroup {
    pub layout: GlaImageLayout,
    pub children: Vec<NodeKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlaGroupError {
    ChildNotFound,
}

impl GlaGroup {
    pub fn switch_child(&self, from: NodeKey, to: NodeKey) -> Result<Self, GlaGroupError> {
        let mut new = self.clone();
        if let Some(index) = new.children.iter().position(|key| *key == from) {
            new.children[index] = to;
            Ok(new)
        } else {
            Err(GlaGroupError::ChildNotFound)
        }
    }
}
