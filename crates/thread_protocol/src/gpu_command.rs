use glaphica_core::{BrushId, NodeId, RenderTreeGeneration, TileKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawBlendMode {
    /// Standard alpha compositing for brush dab rendering.
    Alpha,
    /// Replace destination content in draw pipeline.
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteBlendMode {
    /// Normal blend on top of destination.
    Normal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawFrameMergePolicy {
    None,
    KeepLastInFrameByNodeTileBrush,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefImage {
    pub tile_key: TileKey,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DrawOp {
    /// Image node that owns `tile_index`.
    pub node_id: NodeId,
    /// Tile index in image-space tile grid (without gutter).
    pub tile_index: usize,
    /// Destination atlas tile key.
    pub tile_key: TileKey,
    pub blend_mode: DrawBlendMode,
    /// In-frame merge hint used by frame scheduler/runtime.
    pub frame_merge: DrawFrameMergePolicy,
    /// Optional "origin snapshot" tile key used by brush pipelines that need read/restore.
    /// `TileKey::EMPTY` means no origin snapshot.
    pub origin_tile: TileKey,
    /// Optional reference image tile used by some brush pipelines.
    pub ref_image: Option<RefImage>,
    /// Brush-defined draw payload.
    pub input: Vec<f32>,
    pub brush_id: BrushId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyOp {
    /// Source tile in atlas space.
    pub src_tile_key: TileKey,
    /// Destination tile in atlas space.
    ///
    /// Semantics are full-tile replacement (not blending).
    pub dst_tile_key: TileKey,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WriteOp {
    /// Source tile in atlas space.
    pub src_tile_key: TileKey,
    /// Destination tile in atlas space.
    ///
    /// Semantics preserve destination and apply `blend_mode` on top.
    pub dst_tile_key: TileKey,
    pub blend_mode: WriteBlendMode,
    /// Global write opacity multiplier in [0, 1].
    pub opacity: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompositeOp {
    /// Base tile in atlas space.
    ///
    /// The other source tile is composited onto this base.
    pub base_tile_key: TileKey,
    /// Overlay tile in atlas space.
    ///
    /// This tile is drawn onto `base_tile_key` using `blend_mode` and `opacity`.
    pub overlay_tile_key: TileKey,
    /// Destination tile in atlas space.
    pub dst_tile_key: TileKey,
    pub blend_mode: WriteBlendMode,
    /// Global composite opacity multiplier in [0, 1].
    pub opacity: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClearOp {
    /// Tile to clear to transparent.
    pub tile_key: TileKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderTreeUpdatedMsg {
    pub generation: RenderTreeGeneration,
    pub dirty_branch_caches: Vec<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileSlotKeyUpdateMsg {
    pub updates: Vec<(NodeId, usize, TileKey)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GpuCmdMsg {
    /// Brush pipeline draw into one destination tile.
    DrawOp(DrawOp),
    /// Full-tile replacement: `src` overwrites `dst`.
    CopyOp(CopyOp),
    /// Blend `src` onto `dst` with configured write blend mode.
    WriteOp(WriteOp),
    /// Composite overlay onto base and write the result to destination.
    CompositeOp(CompositeOp),
    /// Clear one tile to transparent.
    ClearOp(ClearOp),
    RenderTreeUpdated(RenderTreeUpdatedMsg),
    TileSlotKeyUpdate(TileSlotKeyUpdateMsg),
}
