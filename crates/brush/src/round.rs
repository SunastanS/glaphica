use crate::{BrushId, BrushShaderRegistration};
use bytemuck::{Pod, Zeroable};
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

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
struct RoundApplyPayload {
    center_local: [f32; 2],
    radius_px: f32,
    hardness: f32,
    opacity: f32,
    _pad1: [u32; 3],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
struct RoundMergePayload {
    tint: [f32; 3],
    _pad2: f32,
}

pub fn encode_round_apply_payload(
    center_local: [f32; 2],
    radius_px: f32,
    hardness: f32,
    opacity: f32,
) -> Vec<u8> {
    bytemuck::bytes_of(&RoundApplyPayload {
        center_local,
        radius_px,
        hardness,
        opacity,
        _pad1: [0; 3],
    })
    .to_vec()
}

pub fn encode_round_merge_payload(tint: [f32; 3]) -> Vec<u8> {
    bytemuck::bytes_of(&RoundMergePayload { tint, _pad2: 0.0 }).to_vec()
}
