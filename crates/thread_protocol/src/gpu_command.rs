use glaphica_core::{BrushId, TileKey};
#[derive(Debug, Clone, PartialEq)]
pub struct DrawOp {
    pub tile_key: TileKey,
    pub input: Vec<f32>,
    pub brush_id: BrushId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyOp {
    pub src_tile_key: TileKey,
    pub dst_tile_key: TileKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClearOp {
    pub tile_key: TileKey,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GpuCmdMsg {
    Notify,
    DrawOp(DrawOp),
    CopyOp(CopyOp),
    ClearOp(ClearOp),
}
