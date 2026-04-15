use brush::BrushId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Brush(BrushId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveTool {
    Brush(BrushId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSet {
    tools: Vec<Tool>,
}

impl Tool {
    pub fn brush_id(self) -> Option<BrushId> {
        match self {
            Self::Brush(brush_id) => Some(brush_id),
        }
    }
}

impl ActiveTool {
    pub fn brush_id(self) -> Option<BrushId> {
        match self {
            Self::Brush(brush_id) => Some(brush_id),
        }
    }

    pub fn as_tool(self) -> Tool {
        match self {
            Self::Brush(brush_id) => Tool::Brush(brush_id),
        }
    }
}

impl ToolSet {
    pub fn new(tools: Vec<Tool>) -> Self {
        Self { tools }
    }

    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    pub fn contains(&self, tool: Tool) -> bool {
        self.tools.contains(&tool)
    }
}

#[cfg(test)]
mod tests {
    use brush::BrushId;

    use crate::{ActiveTool, Tool, ToolSet};

    #[test]
    fn active_tool_round_trips_into_tool_set_membership() {
        let active_tool = ActiveTool::Brush(BrushId::new(5));
        let tool_set = ToolSet::new(vec![Tool::Brush(BrushId::new(5))]);

        assert!(tool_set.contains(active_tool.as_tool()));
        assert_eq!(active_tool.brush_id(), Some(BrushId::new(5)));
    }
}
