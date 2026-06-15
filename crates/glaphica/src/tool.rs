#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct BrushId(u64);

impl BrushId {
    pub const DEFAULT: Self = Self::new(1);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Brush(BrushId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveTool {
    Brush(BrushId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
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

    pub fn default_brush() -> Self {
        Self::new(vec![Tool::Brush(BrushId::DEFAULT)])
    }

    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    pub fn contains(&self, tool: Tool) -> bool {
        self.tools.contains(&tool)
    }
}

impl Default for ToolSet {
    fn default() -> Self {
        Self::default_brush()
    }
}

#[cfg(test)]
mod tests {
    use super::{ActiveTool, BrushId, Tool, ToolSet};

    #[test]
    fn active_tool_round_trips_into_tool_set_membership() {
        let active_tool = ActiveTool::Brush(BrushId::new(5));
        let tool_set = ToolSet::new(vec![Tool::Brush(BrushId::new(5))]);

        assert!(tool_set.contains(active_tool.as_tool()));
        assert_eq!(active_tool.brush_id(), Some(BrushId::new(5)));
    }

    #[test]
    fn default_tool_set_contains_default_brush() {
        let tool_set = ToolSet::default();

        assert_eq!(tool_set.tools(), &[Tool::Brush(BrushId::DEFAULT)]);
    }
}
