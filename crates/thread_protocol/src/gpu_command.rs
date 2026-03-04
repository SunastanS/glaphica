use glaphica_core::{BrushId, NodeId, RenderTreeGeneration, TileKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefImage {
    pub tile_key: TileKey,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DrawOp {
    pub node_id: NodeId,
    pub tile_index: usize,
    pub tile_key: TileKey,
    pub ref_image: Option<RefImage>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderTreeUpdatedOp {
    pub generation: RenderTreeGeneration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileSlotKeyUpdateOp {
    pub updates: Vec<(NodeId, usize, TileKey)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GpuCmdMsg {
    DrawOp(DrawOp),
    CopyOp(CopyOp),
    ClearOp(ClearOp),
    RenderTreeUpdated(RenderTreeUpdatedOp),
    TileSlotKeyUpdate(TileSlotKeyUpdateOp),
}
