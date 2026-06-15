use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};

use gla_color::{ChannelCount, ChannelType, GlaFormat};
use gla_core::ATLAS_TILE_SIZE;
use gla_renderer::{GpuRenderer, GpuRendererError};
use serde::{Deserialize, Serialize};

use crate::{DocumentPresentError, DocumentWorkspace};

const EXPORT_VERSION: u32 = 1;
const MANIFEST_FILE_NAME: &str = "workspace.json";
const TILE_DIRECTORY_NAME: &str = "tiles";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceExportManifest {
    pub version: u32,
    pub root_image_id: u64,
    pub canvas_width_px: u32,
    pub canvas_height_px: u32,
    pub format: GlaFormat,
    pub bytes_per_pixel: u32,
    pub padded_bytes_per_row: u32,
    pub atlas_tile_size: u32,
    pub tiles: Vec<WorkspaceExportTile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceExportTile {
    pub tile_index: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub path: PathBuf,
}

#[derive(Debug)]
pub enum WorkspaceExportError {
    Io(std::io::Error),
    Document(DocumentPresentError),
    Renderer(GpuRendererError),
    UnsupportedFormat(GlaFormat),
}

pub fn export_workspace_directory(
    workspace: &DocumentWorkspace,
    renderer: &GpuRenderer,
    path: impl AsRef<Path>,
) -> Result<WorkspaceExportManifest, WorkspaceExportError> {
    let root_path = path.as_ref();
    let tile_dir = root_path.join(TILE_DIRECTORY_NAME);
    if tile_dir.exists() {
        fs::remove_dir_all(&tile_dir)?;
    }
    fs::create_dir_all(&tile_dir)?;

    let format = workspace.format();
    let bytes_per_pixel =
        bytes_per_pixel(format).ok_or(WorkspaceExportError::UnsupportedFormat(format))?;
    let padded_bytes_per_row = padded_bytes_per_row(bytes_per_pixel);
    let mut tiles = Vec::new();

    for tile in workspace.root_physical_tiles()? {
        let relative_path = root_tile_asset_relative_path(tile.tile_index);
        let bytes = renderer.read_tile_bytes(tile.src, bytes_per_pixel)?;
        fs::write(root_path.join(&relative_path), bytes)?;
        tiles.push(WorkspaceExportTile {
            tile_index: tile.tile_index,
            source_width: tile.source_width,
            source_height: tile.source_height,
            path: relative_path,
        });
    }

    let (canvas_width_px, canvas_height_px) = workspace.canvas_size_px();
    let manifest = WorkspaceExportManifest {
        version: EXPORT_VERSION,
        root_image_id: workspace.root().value(),
        canvas_width_px,
        canvas_height_px,
        format,
        bytes_per_pixel,
        padded_bytes_per_row,
        atlas_tile_size: ATLAS_TILE_SIZE,
        tiles,
    };
    write_workspace_manifest(root_path, &manifest)?;
    Ok(manifest)
}

pub fn write_workspace_manifest(
    root_path: impl AsRef<Path>,
    manifest: &WorkspaceExportManifest,
) -> Result<(), WorkspaceExportError> {
    fs::create_dir_all(root_path.as_ref())?;
    let bytes = serde_json::to_vec_pretty(manifest)?;
    fs::write(root_path.as_ref().join(MANIFEST_FILE_NAME), bytes)?;
    Ok(())
}

pub fn read_workspace_manifest(
    root_path: impl AsRef<Path>,
) -> Result<WorkspaceExportManifest, WorkspaceExportError> {
    let bytes = fs::read(root_path.as_ref().join(MANIFEST_FILE_NAME))?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn root_tile_asset_relative_path(tile_index: u32) -> PathBuf {
    PathBuf::from(TILE_DIRECTORY_NAME).join(format!("root_{tile_index:06}.bin"))
}

fn bytes_per_pixel(format: GlaFormat) -> Option<u32> {
    let channels: u32 = match format.channel_count {
        ChannelCount::D1 => 1,
        ChannelCount::D2 => 2,
        ChannelCount::D4 => 4,
    };
    let channel_bytes: u32 = match format.channel_type {
        ChannelType::U8 => 1,
        ChannelType::U32 | ChannelType::F32 => 4,
        ChannelType::F64 => 8,
    };
    channels.checked_mul(channel_bytes)
}

fn padded_bytes_per_row(bytes_per_pixel: u32) -> u32 {
    (ATLAS_TILE_SIZE * bytes_per_pixel).div_ceil(256) * 256
}

impl Display for WorkspaceExportError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "workspace export io failed: {error}"),
            Self::Document(error) => Display::fmt(error, f),
            Self::Renderer(error) => Display::fmt(error, f),
            Self::UnsupportedFormat(format) => {
                write!(f, "workspace export does not support format {format:?}")
            }
        }
    }
}

impl Error for WorkspaceExportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Document(error) => Some(error),
            Self::Renderer(error) => Some(error),
            Self::UnsupportedFormat(_) => None,
        }
    }
}

impl From<std::io::Error> for WorkspaceExportError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<DocumentPresentError> for WorkspaceExportError {
    fn from(error: DocumentPresentError) -> Self {
        Self::Document(error)
    }
}

impl From<GpuRendererError> for WorkspaceExportError {
    fn from(error: GpuRendererError) -> Self {
        Self::Renderer(error)
    }
}

impl From<serde_json::Error> for WorkspaceExportError {
    fn from(error: serde_json::Error) -> Self {
        Self::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WorkspaceExportManifest, WorkspaceExportTile, bytes_per_pixel, padded_bytes_per_row,
        read_workspace_manifest, root_tile_asset_relative_path, write_workspace_manifest,
    };
    use gla_color::{ChannelCount, ChannelType, GlaFormat};

    fn export_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "glaphica-workspace-export-{name}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn tile_asset_relative_path_is_stable_and_editable() {
        assert_eq!(
            root_tile_asset_relative_path(7),
            std::path::PathBuf::from("tiles/root_000007.bin")
        );
    }

    #[test]
    fn manifest_round_trips_as_readable_json() {
        let path = export_path("manifest");
        let manifest = WorkspaceExportManifest {
            version: 1,
            root_image_id: 1,
            canvas_width_px: 320,
            canvas_height_px: 240,
            format: GlaFormat {
                channel_count: ChannelCount::D4,
                channel_type: ChannelType::F32,
            },
            bytes_per_pixel: 16,
            padded_bytes_per_row: padded_bytes_per_row(16),
            atlas_tile_size: gla_core::ATLAS_TILE_SIZE,
            tiles: vec![WorkspaceExportTile {
                tile_index: 0,
                source_width: 62,
                source_height: 62,
                path: root_tile_asset_relative_path(0),
            }],
        };

        write_workspace_manifest(&path, &manifest).unwrap();
        let loaded = read_workspace_manifest(&path).unwrap();
        let json = std::fs::read_to_string(path.join("workspace.json")).unwrap();

        assert_eq!(loaded, manifest);
        assert!(json.contains("\"canvas_width_px\": 320"));
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn bytes_per_pixel_matches_canvas_format() {
        assert_eq!(
            bytes_per_pixel(GlaFormat {
                channel_count: ChannelCount::D4,
                channel_type: ChannelType::F32,
            }),
            Some(16)
        );
        assert_eq!(
            bytes_per_pixel(GlaFormat {
                channel_count: ChannelCount::D1,
                channel_type: ChannelType::U8,
            }),
            Some(1)
        );
    }
}
