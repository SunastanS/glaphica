use gla_color::PremultipliedRgbaF32;

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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrushSettings {
    pub radius_px: f32,
    pub color: PremultipliedRgbaF32,
    pub spacing_ratio: f32,
    pub hardness: f32,
    pub flow: f32,
    pub opacity: f32,
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

impl Default for BrushSettings {
    fn default() -> Self {
        Self {
            radius_px: 10.0,
            color: PremultipliedRgbaF32::new(0.95, 0.17, 0.10, 1.0),
            spacing_ratio: 1.0,
            hardness: 0.7,
            flow: 1.0,
            opacity: 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ActiveTool, BrushId, BrushSettings, Tool, ToolSet};

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

    #[test]
    fn default_brush_settings_reserve_round_brush_controls() {
        let settings = BrushSettings::default();

        assert_eq!(settings.radius_px, 10.0);
        assert_eq!(
            settings.color,
            gla_color::PremultipliedRgbaF32::new(0.95, 0.17, 0.10, 1.0)
        );
        assert_eq!(settings.spacing_ratio, 1.0);
        assert_eq!(settings.hardness, 0.7);
        assert_eq!(settings.flow, 1.0);
        assert_eq!(settings.opacity, 1.0);
    }
}
