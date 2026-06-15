use gla_color::PremultipliedRgbaF32;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tool {
    Brush(BrushId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActiveTool {
    Brush(BrushId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSet {
    tools: Vec<Tool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrushSettings {
    pub radius_px: f32,
    pub color: PremultipliedRgbaF32,
    pub spacing_ratio: f32,
    pub hardness: f32,
    pub flow: f32,
    pub opacity: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoundBrushSettings {
    pub base_radius_px: f32,
    pub spacing_ratio: f32,
    pub base_hardness: f32,
    pub base_flow: f32,
    pub base_opacity: f32,
    pub tint: [f32; 3],
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

impl Default for RoundBrushSettings {
    fn default() -> Self {
        Self {
            base_radius_px: 5.0,
            spacing_ratio: 1.0,
            base_hardness: 0.7,
            base_flow: 1.0,
            base_opacity: 1.0,
            tint: [0.0, 0.0, 1.0],
        }
    }
}

impl From<RoundBrushSettings> for BrushSettings {
    fn from(settings: RoundBrushSettings) -> Self {
        Self {
            radius_px: settings.base_radius_px,
            color: PremultipliedRgbaF32::new(
                settings.tint[0],
                settings.tint[1],
                settings.tint[2],
                1.0,
            ),
            spacing_ratio: settings.spacing_ratio,
            hardness: settings.base_hardness,
            flow: settings.base_flow,
            opacity: settings.base_opacity,
        }
    }
}

impl BrushSettings {
    pub fn from_round_brush(settings: RoundBrushSettings) -> Self {
        settings.into()
    }
}

#[cfg(test)]
mod tests {
    use super::{ActiveTool, BrushId, BrushSettings, RoundBrushSettings, Tool, ToolSet};

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

    #[test]
    fn round_brush_settings_match_dev_field_contract() {
        let settings = RoundBrushSettings::default();

        assert_eq!(settings.base_radius_px, 5.0);
        assert_eq!(settings.spacing_ratio, 1.0);
        assert_eq!(settings.base_hardness, 0.7);
        assert_eq!(settings.base_flow, 1.0);
        assert_eq!(settings.base_opacity, 1.0);
        assert_eq!(settings.tint, [0.0, 0.0, 1.0]);
    }

    #[test]
    fn round_brush_settings_convert_to_runtime_brush_settings() {
        let settings = BrushSettings::from_round_brush(RoundBrushSettings {
            base_radius_px: 12.0,
            spacing_ratio: 0.5,
            base_hardness: 0.4,
            base_flow: 0.75,
            base_opacity: 0.6,
            tint: [0.1, 0.2, 0.3],
        });

        assert_eq!(settings.radius_px, 12.0);
        assert_eq!(
            settings.color,
            gla_color::PremultipliedRgbaF32::new(0.1, 0.2, 0.3, 1.0)
        );
        assert_eq!(settings.spacing_ratio, 0.5);
        assert_eq!(settings.hardness, 0.4);
        assert_eq!(settings.flow, 0.75);
        assert_eq!(settings.opacity, 0.6);
    }
}
