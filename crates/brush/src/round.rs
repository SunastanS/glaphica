use crate::{BrushId, BrushShaderRegistration};
use renderer::{BrushShaderSource, BrushShaderSpec};

pub const ROUND_BRUSH_ID: BrushId = BrushId::new(1);

pub const ROUND_APPLY_DAB_WGSL: &str = include_str!("round_apply_dab.wgsl");
pub const ROUND_MERGE_TILE_WGSL: &str = include_str!("round_merge_tile.wgsl");

pub const ROUND_SHADER_SPEC: BrushShaderSpec = BrushShaderSpec {
    apply_dab: BrushShaderSource {
        wgsl: ROUND_APPLY_DAB_WGSL,
        entry_point: "fs_apply_dab",
    },
    merge_tile: BrushShaderSource {
        wgsl: ROUND_MERGE_TILE_WGSL,
        entry_point: "fs_merge_tile",
    },
};

pub const ROUND_SHADER_REGISTRATION: BrushShaderRegistration = BrushShaderRegistration {
    brush_id: ROUND_BRUSH_ID,
    shader_spec: ROUND_SHADER_SPEC,
};
