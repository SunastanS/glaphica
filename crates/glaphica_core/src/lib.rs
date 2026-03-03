pub const ATLAS_TILE_SIZE: u32 = 64;
pub const GUTTER_SIZE: u32 = 1;
pub const IMAGE_TILE_SIZE: u32 = ATLAS_TILE_SIZE - 2 * GUTTER_SIZE;

mod tiles;

pub use tiles::{
    BackendId, BackendKind, BackendTag, GenerationId, GenerationTag, Id, SlotId, SlotTag, TileKey,
};

mod id_allocator;

pub use id_allocator::{EpochIdAllocator, PresentFrameIdAllocator, StrokeIdAllocator};

mod vec2;

pub use vec2::{CanvasVec2, RadianVec2, ScreenVec2};

mod texture_format;

pub use texture_format::TextureFormat;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BrushId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PresentFrameId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StrokeId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EpochId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InputDeviceKind {
    Pen,
    Cursor,
    Finger(u32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawCursor {
    pub cursor: ScreenVec2,
    pub tilt: RadianVec2,
    pub pressure: f32,
    pub twist: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MappedCursor {
    pub cursor: CanvasVec2,
    pub tilt: RadianVec2, // [tile_x, tile_y]
    pub pressure: f32,    // [0, 1]
    pub twist: f32,       // [-Pi, Pi]
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct BrushInputFlags: u32 {
        const PATH_S = 1 << 0;
        const DELTA_S = 1 << 1;
        const DT_S = 1 << 2;
        const VEL = 1 << 3;
        const SPEED = 1 << 4;
        const TANGENT = 1 << 5;
        const ACC = 1 << 6;
        const ACCEL = 1 << 7;
        const CURVATURE = 1 << 8;
        const CONFIDENCE = 1 << 9;
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrushInput {
    pub stroke: StrokeId,
    pub cursor: MappedCursor,
    pub flags: BrushInputFlags,
    pub path_s: f32,
    pub delta_s: f32,
    pub dt_s: f32,
    pub vel: CanvasVec2,     // canvas per second
    pub speed: f32,          // cached |vel|
    pub tangent: CanvasVec2, // unit-ish, stable near zero speed
    pub acc: CanvasVec2,     // canvas per second ^ 2
    pub accel: f32,          // cached |acc|
    pub curvature: f32,      // 1/canvas_unit
    pub confidence: f32,     // [0, 1]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AtlasLayout {
    Tiny8,
    Small11,
    Medium14,
    Large17,
    Huge20,
}

impl AtlasLayout {
    pub const fn total_slots(self) -> u32 {
        match self {
            AtlasLayout::Tiny8 => 1 << 8,
            AtlasLayout::Small11 => 1 << 11,
            AtlasLayout::Medium14 => 1 << 14,
            AtlasLayout::Large17 => 1 << 17,
            AtlasLayout::Huge20 => 1 << 20,
        }
    }

    pub const fn layers(self) -> u32 {
        match self {
            AtlasLayout::Tiny8 => 1,
            AtlasLayout::Small11 => 2,
            AtlasLayout::Medium14 => 4,
            AtlasLayout::Large17 => 8,
            AtlasLayout::Huge20 => 16,
        }
    }

    pub const fn tiles_per_edge(self) -> u32 {
        match self {
            AtlasLayout::Tiny8 => 16,
            AtlasLayout::Small11 => 32,
            AtlasLayout::Medium14 => 64,
            AtlasLayout::Large17 => 128,
            AtlasLayout::Huge20 => 256,
        }
    }

    pub const fn tiles_per_edge_bits(self) -> u32 {
        match self {
            AtlasLayout::Tiny8 => 4,
            AtlasLayout::Small11 => 5,
            AtlasLayout::Medium14 => 6,
            AtlasLayout::Large17 => 7,
            AtlasLayout::Huge20 => 8,
        }
    }
}
