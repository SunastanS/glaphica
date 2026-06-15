use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Component, Path, PathBuf};

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceExportSnapshot {
    pub manifest: WorkspaceExportManifest,
    pub tiles: Vec<WorkspaceExportTileAsset>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceExportTileAsset {
    pub tile_index: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub enum WorkspaceExportError {
    Io(std::io::Error),
    Document(DocumentPresentError),
    Renderer(GpuRendererError),
    UnsupportedVersion(u32),
    InvalidTilePath {
        path: PathBuf,
    },
    InvalidTileLength {
        path: PathBuf,
        expected: usize,
        actual: usize,
    },
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

pub fn read_workspace_directory(
    root_path: impl AsRef<Path>,
) -> Result<WorkspaceExportSnapshot, WorkspaceExportError> {
    let root_path = root_path.as_ref();
    let manifest = read_workspace_manifest(root_path)?;
    validate_manifest_version(&manifest)?;
    let expected_tile_len = expected_tile_byte_len(&manifest);
    let mut assets = Vec::with_capacity(manifest.tiles.len());

    for tile in &manifest.tiles {
        validate_relative_asset_path(&tile.path)?;
        let bytes = fs::read(root_path.join(&tile.path))?;
        if bytes.len() != expected_tile_len {
            return Err(WorkspaceExportError::InvalidTileLength {
                path: tile.path.clone(),
                expected: expected_tile_len,
                actual: bytes.len(),
            });
        }
        assets.push(WorkspaceExportTileAsset {
            tile_index: tile.tile_index,
            source_width: tile.source_width,
            source_height: tile.source_height,
            path: tile.path.clone(),
            bytes,
        });
    }

    Ok(WorkspaceExportSnapshot {
        manifest,
        tiles: assets,
    })
}

pub fn root_tile_asset_relative_path(tile_index: u32) -> PathBuf {
    PathBuf::from(TILE_DIRECTORY_NAME).join(format!("root_{tile_index:06}.bin"))
}

fn validate_manifest_version(
    manifest: &WorkspaceExportManifest,
) -> Result<(), WorkspaceExportError> {
    if manifest.version != EXPORT_VERSION {
        return Err(WorkspaceExportError::UnsupportedVersion(manifest.version));
    }
    Ok(())
}

fn validate_relative_asset_path(path: &Path) -> Result<(), WorkspaceExportError> {
    let mut has_component = false;
    for component in path.components() {
        match component {
            Component::Normal(_) => has_component = true,
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                return Err(WorkspaceExportError::InvalidTilePath {
                    path: path.to_path_buf(),
                });
            }
        }
    }
    if !has_component {
        return Err(WorkspaceExportError::InvalidTilePath {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn expected_tile_byte_len(manifest: &WorkspaceExportManifest) -> usize {
    manifest.padded_bytes_per_row as usize * manifest.atlas_tile_size as usize
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
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported workspace export version {version}")
            }
            Self::InvalidTilePath { path } => {
                write!(f, "workspace export tile path is not relative: {path:?}")
            }
            Self::InvalidTileLength {
                path,
                expected,
                actual,
            } => write!(
                f,
                "workspace export tile {path:?} has {actual} bytes, expected {expected}"
            ),
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
            Self::UnsupportedVersion(_)
            | Self::InvalidTilePath { .. }
            | Self::InvalidTileLength { .. }
            | Self::UnsupportedFormat(_) => None,
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
        read_workspace_directory, read_workspace_manifest, root_tile_asset_relative_path,
        write_workspace_manifest,
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
    fn workspace_directory_reads_manifest_and_tile_assets() {
        let path = export_path("directory-read");
        let tile_path = root_tile_asset_relative_path(0);
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
                path: tile_path.clone(),
            }],
        };
        let expected_len =
            manifest.padded_bytes_per_row as usize * manifest.atlas_tile_size as usize;
        write_workspace_manifest(&path, &manifest).unwrap();
        std::fs::create_dir_all(path.join("tiles")).unwrap();
        std::fs::write(path.join(&tile_path), vec![7_u8; expected_len]).unwrap();

        let snapshot = read_workspace_directory(&path).unwrap();

        assert_eq!(snapshot.manifest, manifest);
        assert_eq!(snapshot.tiles.len(), 1);
        assert_eq!(snapshot.tiles[0].tile_index, 0);
        assert_eq!(snapshot.tiles[0].path, tile_path);
        assert_eq!(snapshot.tiles[0].bytes, vec![7_u8; expected_len]);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn workspace_directory_rejects_escaping_tile_paths() {
        let path = export_path("escaping-path");
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
                path: std::path::PathBuf::from("../outside.bin"),
            }],
        };
        write_workspace_manifest(&path, &manifest).unwrap();

        let error = read_workspace_directory(&path).unwrap_err();

        assert!(matches!(
            error,
            super::WorkspaceExportError::InvalidTilePath { .. }
        ));
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
