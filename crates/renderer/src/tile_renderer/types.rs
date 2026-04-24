use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasError, BackendId, TileKey};
use glaphica_core::{BlendMode, BrushId};

use super::atlas_texture_set::AtlasTextureStage;
use super::brush_encode::BrushEncodeStage;
use crate::TextureIoError;

#[derive(Debug)]
pub enum TileRendererError {
    Atlas(AtlasError),
    TextureIo(TextureIoError),
    MissingBackendTexture(BackendId),
    BackendTextureFormatMismatch {
        backend_id: BackendId,
        expected: BrushIntermediateFormat,
        actual: BrushIntermediateFormat,
    },
    InvalidTileKey,
    MissingPresentTarget,
    UnsupportedCommand(&'static str),
    MissingBrushShader {
        brush_id: BrushId,
        stage: BrushShaderStage,
    },
}

impl Display for TileRendererError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Atlas(error) => Display::fmt(error, f),
            Self::TextureIo(error) => Display::fmt(error, f),
            Self::MissingBackendTexture(backend_id) => {
                write!(
                    f,
                    "missing renderer texture for atlas backend {}",
                    backend_id.raw()
                )
            }
            Self::BackendTextureFormatMismatch {
                backend_id,
                expected,
                actual,
            } => write!(
                f,
                "atlas backend {} was requested as {expected:?}, but already exists as {actual:?}",
                backend_id.raw()
            ),
            Self::InvalidTileKey => f.write_str("invalid tile key"),
            Self::MissingPresentTarget => f.write_str("present command requires a render target"),
            Self::UnsupportedCommand(name) => {
                write!(f, "renderer command {name} is not implemented")
            }
            Self::MissingBrushShader { brush_id, stage } => {
                write!(
                    f,
                    "missing brush shader for brush {} stage {stage:?}",
                    brush_id.raw()
                )
            }
        }
    }
}

impl Error for TileRendererError {}

impl From<AtlasError> for TileRendererError {
    fn from(error: AtlasError) -> Self {
        Self::Atlas(error)
    }
}

impl From<TextureIoError> for TileRendererError {
    fn from(error: TextureIoError) -> Self {
        Self::TextureIo(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushIntermediateFormat {
    Rgba8Unorm,
    R16Float,
}

#[derive(Debug, Clone, Copy)]
pub struct RenderTarget2d<'a> {
    pub view: &'a wgpu::TextureView,
    pub format: wgpu::TextureFormat,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TileCompositeSource {
    pub tile_key: TileKey,
    pub opacity: f32,
    pub blend_mode: BlendMode,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresentTileParams {
    pub target_min_px: [f32; 2],
    pub target_max_px: [f32; 2],
    pub source_width: u32,
    pub source_height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyTileCommand {
    pub source_tile_key: TileKey,
    pub destination_tile_key: TileKey,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApplyDabCommand {
    pub brush_id: BrushId,
    pub destination_tile_key: TileKey,
    pub source_tile_key: Option<TileKey>,
    pub brush_payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MergeTileCommand {
    pub brush_id: BrushId,
    pub origin_tile_key: TileKey,
    pub intermediate_tile_key: TileKey,
    pub destination_tile_key: TileKey,
    pub brush_payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompositeTileCommand {
    pub target_tile_key: TileKey,
    pub inputs: Vec<TileCompositeSource>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresentTileCommand {
    pub source_tile_key: TileKey,
    pub params: PresentTileParams,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushShaderStage {
    ApplyDab,
    MergeTile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrushShaderSource {
    pub wgsl: &'static str,
    pub entry_point: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrushShaderSpec {
    pub intermediate_format: BrushIntermediateFormat,
    pub apply_dab: BrushShaderSource,
    pub merge_tile: BrushShaderSource,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RenderCommand {
    CopyTile(CopyTileCommand),
    ApplyDab(ApplyDabCommand),
    MergeTile(MergeTileCommand),
    CompositeTile(CompositeTileCommand),
    PresentTile(PresentTileCommand),
}

pub trait BrushShaderProvider {
    fn shader_spec(&self, brush_id: BrushId) -> Option<BrushShaderSpec>;
}

pub trait BrushCommandExecutor {
    fn apply_dab(
        &mut self,
        atlas_texture_set: &AtlasTextureStage,
        brush_encode: &mut BrushEncodeStage,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        command: &ApplyDabCommand,
    ) -> Result<(), TileRendererError>;

    fn merge_tile(
        &mut self,
        atlas_texture_set: &AtlasTextureStage,
        brush_encode: &mut BrushEncodeStage,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        command: &MergeTileCommand,
    ) -> Result<(), TileRendererError>;
}
