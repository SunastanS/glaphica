use glaphica_core::{
    AtlasLayout, BlendMode, BrushId, ImageTileBinding, ImageTileKey, NodeId,
    RenderTreeGeneration, StrokeId, TileKey,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawFrameMergePolicy {
    None,
    KeepLastInFrameByNodeTileBrush,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuCmdFrameMergeTag {
    None,
    KeepFirstInFrameByDstTile,
    KeepLastInFrameByDstTile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefImage {
    pub tile_key: TileKey,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrawStrokeCtx {
    pub blend_mode: BlendMode,
    /// In-frame merge hint used by frame scheduler/runtime.
    pub frame_merge: DrawFrameMergePolicy,
    /// App-owned brush RGB tint in [0, 1].
    pub rgb: [f32; 3],
    pub brush_id: BrushId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DrawOp {
    /// Optional stroke-level context. `None` means receiver should resolve from cached stroke ctx.
    pub stroke_ctx: Option<DrawStrokeCtx>,
    /// Logical tile position in image-space tile grid (without gutter).
    pub image_tile: ImageTileKey,
    /// Destination atlas tile key.
    pub tile_key: TileKey,
    /// Optional "origin snapshot" tile key used by brush pipelines that need read/restore.
    /// `TileKey::EMPTY` means no origin snapshot.
    pub origin_tile: TileKey,
    /// Optional reference image tile used by some brush pipelines.
    pub ref_image: Option<RefImage>,
    /// Brush-defined draw payload.
    pub input: Vec<f32>,
    /// Stroke identity from input pipeline.
    pub stroke_id: StrokeId,
}

impl DrawOp {
    pub const BLEND_MODE_SUBSET: [BlendMode; 3] =
        [BlendMode::Alpha, BlendMode::Additive, BlendMode::Replace];

    pub const fn supports_blend_mode(blend_mode: BlendMode) -> bool {
        matches!(
            blend_mode,
            BlendMode::Alpha | BlendMode::Additive | BlendMode::Replace
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyOp {
    /// Source tile in atlas space.
    pub src_tile_key: TileKey,
    /// Destination tile in atlas space.
    ///
    /// Semantics are full-tile replacement (not blending).
    pub dst_tile_key: TileKey,
    pub frame_merge: GpuCmdFrameMergeTag,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WriteOp {
    /// Source tile in atlas space.
    pub src_tile_key: TileKey,
    /// Logical tile position in image-space tile grid (without gutter).
    pub image_tile: ImageTileKey,
    /// Destination tile in atlas space.
    ///
    /// Semantics preserve destination and apply `blend_mode` on top.
    pub dst_tile_key: TileKey,
    pub blend_mode: BlendMode,
    pub kind: WriteKind,
    /// Global write opacity multiplier in [0, 1].
    pub opacity: f32,
    /// Optional app-owned RGB tint in [0, 1]. When absent, source rgb is preserved.
    pub rgb: Option<[f32; 3]>,
    pub frame_merge: GpuCmdFrameMergeTag,
}

impl WriteOp {
    pub const BLEND_MODE_SUBSET: [BlendMode; 1] = [BlendMode::Normal];

    pub const fn supports_blend_mode(blend_mode: BlendMode) -> bool {
        matches!(blend_mode, BlendMode::Normal)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteKind {
    Paint,
    /// Erase from the origin snapshot using source alpha as the erase mask.
    Erase {
        origin_tile_key: TileKey,
    },
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
    pub blend_mode: BlendMode,
    /// Global composite opacity multiplier in [0, 1].
    pub opacity: f32,
}

impl CompositeOp {
    pub const BLEND_MODE_SUBSET: [BlendMode; 2] = [BlendMode::Normal, BlendMode::Multiply];

    pub const fn supports_blend_mode(blend_mode: BlendMode) -> bool {
        matches!(blend_mode, BlendMode::Normal | BlendMode::Multiply)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClearOp {
    /// Tile to clear to transparent.
    pub tile_key: TileKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderTreeUpdatedMsg {
    pub generation: RenderTreeGeneration,
    pub dirty_render_caches: Vec<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileSlotKeyUpdateMsg {
    pub updates: Vec<ImageTileBinding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpandAtlasBackendMsg {
    pub src_backend_id: u8,
    pub dst_backend_id: u8,
    pub src_layout: AtlasLayout,
    pub dst_layout: AtlasLayout,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GpuCmdMsg {
    /// Create a larger atlas backend, migrate the old backend content into it, then alias
    /// the source backend id to the destination backend id for future tile-key resolution.
    ExpandAtlasBackend(ExpandAtlasBackendMsg),
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
