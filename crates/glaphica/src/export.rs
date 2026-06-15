use std::collections::HashMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Component, Path, PathBuf};

use gla_color::{ChannelCount, ChannelType, GlaFormat};
use gla_core::ATLAS_TILE_SIZE;
use gla_ir::ImageId;
use gla_renderer::{GpuRenderer, GpuRendererError};
use gla_storage::{GlobalEditError, GlobalStorage, GlobalStorageError, ImageEdit};
use serde::{Deserialize, Serialize};
use tile_key::{Tile, TilesError};

use crate::{DocumentPresentError, DocumentWorkspace, DocumentWorkspaceBuildError};

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
    DocumentBuild(DocumentWorkspaceBuildError<GpuRendererError>),
    Storage(GlobalStorageError),
    StorageEdit(GlobalEditError),
    Tile(TilesError),
    UnsupportedVersion(u32),
    UnsupportedAtlasTileSize(u32),
    InvalidBytesPerPixel {
        expected: u32,
        actual: u32,
    },
    InvalidPaddedBytesPerRow {
        expected: u32,
        actual: u32,
    },
    InvalidTileIndex {
        tile_index: u32,
        tile_count: u32,
    },
    DuplicateTile {
        tile_index: u32,
    },
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
    validate_manifest_layout(&manifest)?;
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

pub fn import_workspace_directory(
    renderer: &mut GpuRenderer,
    root_path: impl AsRef<Path>,
) -> Result<DocumentWorkspace, WorkspaceExportError> {
    let snapshot = read_workspace_directory(root_path)?;
    workspace_from_export_snapshot(renderer, snapshot)
}

pub fn workspace_from_export_snapshot(
    renderer: &mut GpuRenderer,
    snapshot: WorkspaceExportSnapshot,
) -> Result<DocumentWorkspace, WorkspaceExportError> {
    let WorkspaceExportSnapshot {
        manifest,
        mut tiles,
    } = snapshot;
    validate_manifest_layout(&manifest)?;
    validate_snapshot_tile_indices(&manifest, &tiles)?;

    let mut workspace = DocumentWorkspace::primitive_root_with_textures(
        ImageId::new(manifest.root_image_id),
        manifest.canvas_width_px,
        manifest.canvas_height_px,
        manifest.format,
        renderer,
    )
    .map_err(WorkspaceExportError::DocumentBuild)?;
    apply_snapshot_root_tiles(&mut workspace, renderer, &manifest, &mut tiles)?;
    Ok(workspace)
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

fn validate_manifest_layout(
    manifest: &WorkspaceExportManifest,
) -> Result<(), WorkspaceExportError> {
    validate_manifest_version(manifest)?;
    if manifest.atlas_tile_size != ATLAS_TILE_SIZE {
        return Err(WorkspaceExportError::UnsupportedAtlasTileSize(
            manifest.atlas_tile_size,
        ));
    }
    let expected_bytes_per_pixel = bytes_per_pixel(manifest.format)
        .ok_or(WorkspaceExportError::UnsupportedFormat(manifest.format))?;
    if manifest.bytes_per_pixel != expected_bytes_per_pixel {
        return Err(WorkspaceExportError::InvalidBytesPerPixel {
            expected: expected_bytes_per_pixel,
            actual: manifest.bytes_per_pixel,
        });
    }
    let expected_padded_bytes_per_row = padded_bytes_per_row(manifest.bytes_per_pixel);
    if manifest.padded_bytes_per_row != expected_padded_bytes_per_row {
        return Err(WorkspaceExportError::InvalidPaddedBytesPerRow {
            expected: expected_padded_bytes_per_row,
            actual: manifest.padded_bytes_per_row,
        });
    }
    Ok(())
}

fn validate_snapshot_tile_indices(
    manifest: &WorkspaceExportManifest,
    tiles: &[WorkspaceExportTileAsset],
) -> Result<(), WorkspaceExportError> {
    let tile_count = manifest
        .canvas_width_px
        .div_ceil(gla_image::IMAGE_TILE_SIZE)
        .checked_mul(
            manifest
                .canvas_height_px
                .div_ceil(gla_image::IMAGE_TILE_SIZE),
        )
        .unwrap_or(u32::MAX);
    let mut indices = tiles.iter().map(|tile| tile.tile_index).collect::<Vec<_>>();
    indices.sort_unstable();
    for tile_index in indices.iter().copied() {
        if tile_index >= tile_count {
            return Err(WorkspaceExportError::InvalidTileIndex {
                tile_index,
                tile_count,
            });
        }
    }
    for pair in indices.windows(2) {
        if pair[0] == pair[1] {
            return Err(WorkspaceExportError::DuplicateTile {
                tile_index: pair[0],
            });
        }
    }
    Ok(())
}

fn apply_snapshot_root_tiles(
    workspace: &mut DocumentWorkspace,
    renderer: &mut GpuRenderer,
    manifest: &WorkspaceExportManifest,
    tiles: &mut Vec<WorkspaceExportTileAsset>,
) -> Result<(), WorkspaceExportError> {
    if tiles.is_empty() {
        return Ok(());
    }

    tiles.sort_by_key(|tile| tile.tile_index);
    let mut edits = Vec::with_capacity(tiles.len());
    for tile_asset in tiles.drain(..) {
        let mut tile = match workspace
            .storage_mut()
            .reserve_tile_for_format(manifest.format)
        {
            Ok(tile) => tile,
            Err(error) => {
                release_pending_tile_edits(workspace.storage_mut(), edits);
                return Err(error.into());
            }
        };
        let position = match workspace.storage_mut().write_tile_pos(&mut tile) {
            Ok(position) => position,
            Err(error) => {
                workspace.storage_mut().tiles_mut().release(tile);
                release_pending_tile_edits(workspace.storage_mut(), edits);
                return Err(error.into());
            }
        };
        if let Err(error) =
            renderer.write_tile_bytes(position, manifest.bytes_per_pixel, &tile_asset.bytes)
        {
            workspace.storage_mut().tiles_mut().release(tile);
            release_pending_tile_edits(workspace.storage_mut(), edits);
            return Err(error.into());
        }
        edits.push((tile_asset.tile_index, tile));
    }

    let edit = ImageEdit::from_sorted_unique(edits)
        .expect("snapshot tile edits were sorted and deduplicated before apply");
    let mut root_edits = HashMap::new();
    root_edits.insert(workspace.root(), edit);
    match workspace.storage_mut().apply_session_edits(root_edits) {
        Ok(inverse) => {
            release_image_edit_map(workspace.storage_mut(), inverse);
            workspace.storage_mut().bump_version();
            Ok(())
        }
        Err(error) => {
            let (error, edits) = error.into_parts();
            release_image_edit_map(workspace.storage_mut(), edits);
            Err(error.into())
        }
    }
}

fn release_pending_tile_edits(storage: &mut GlobalStorage, edits: Vec<(u32, Tile)>) {
    for (_, tile) in edits {
        storage.tiles_mut().release(tile);
    }
}

fn release_image_edit_map(storage: &mut GlobalStorage, edits: HashMap<ImageId, ImageEdit>) {
    for (_, edit) in edits {
        edit.release_tiles(storage.tiles_mut());
    }
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
            Self::DocumentBuild(error) => Display::fmt(error, f),
            Self::Storage(error) => Display::fmt(error, f),
            Self::StorageEdit(error) => Display::fmt(error, f),
            Self::Tile(error) => Display::fmt(error, f),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported workspace export version {version}")
            }
            Self::UnsupportedAtlasTileSize(size) => {
                write!(f, "unsupported workspace export atlas tile size {size}")
            }
            Self::InvalidBytesPerPixel { expected, actual } => write!(
                f,
                "workspace export has {actual} bytes per pixel, expected {expected}"
            ),
            Self::InvalidPaddedBytesPerRow { expected, actual } => write!(
                f,
                "workspace export has padded row length {actual}, expected {expected}"
            ),
            Self::InvalidTileIndex {
                tile_index,
                tile_count,
            } => write!(
                f,
                "workspace export tile index {tile_index} is out of bounds for {tile_count} tiles"
            ),
            Self::DuplicateTile { tile_index } => {
                write!(f, "workspace export repeats tile index {tile_index}")
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
            Self::DocumentBuild(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::StorageEdit(error) => Some(error),
            Self::Tile(error) => Some(error),
            Self::UnsupportedVersion(_)
            | Self::UnsupportedAtlasTileSize(_)
            | Self::InvalidBytesPerPixel { .. }
            | Self::InvalidPaddedBytesPerRow { .. }
            | Self::InvalidTileIndex { .. }
            | Self::DuplicateTile { .. }
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

impl From<GlobalStorageError> for WorkspaceExportError {
    fn from(error: GlobalStorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<GlobalEditError> for WorkspaceExportError {
    fn from(error: GlobalEditError) -> Self {
        Self::StorageEdit(error)
    }
}

impl From<TilesError> for WorkspaceExportError {
    fn from(error: TilesError) -> Self {
        Self::Tile(error)
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
        WorkspaceExportManifest, WorkspaceExportSnapshot, WorkspaceExportTile,
        WorkspaceExportTileAsset, bytes_per_pixel, padded_bytes_per_row, read_workspace_directory,
        read_workspace_manifest, root_tile_asset_relative_path, workspace_from_export_snapshot,
        write_workspace_manifest,
    };
    use gla_color::{ChannelCount, ChannelType, GlaFormat};
    use gla_ir::{DocumentVersionId, ImageId};
    use gla_renderer::GpuRenderer;

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
    fn workspace_snapshot_import_restores_root_tiles_when_adapter_is_available() {
        let (device, queue) = match pollster::block_on(test_device()) {
            Some(device) => device,
            None => {
                eprintln!("skipping workspace import GPU test: no adapter available");
                return;
            }
        };
        let format = GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::F32,
        };
        let bytes_per_pixel = 16;
        let padded_bytes_per_row = padded_bytes_per_row(bytes_per_pixel);
        let expected_len = padded_bytes_per_row as usize * gla_core::ATLAS_TILE_SIZE as usize;
        let tile_path = root_tile_asset_relative_path(0);
        let mut bytes = vec![0_u8; expected_len];
        let offset = ((4 + gla_core::GUTTER_SIZE) * padded_bytes_per_row
            + (5 + gla_core::GUTTER_SIZE) * bytes_per_pixel) as usize;
        for (channel, value) in [0.125_f32, 0.25, 0.5, 1.0].into_iter().enumerate() {
            let start = offset + channel * 4;
            bytes[start..start + 4].copy_from_slice(&value.to_ne_bytes());
        }
        let manifest = WorkspaceExportManifest {
            version: 1,
            root_image_id: 42,
            canvas_width_px: gla_core::IMAGE_TILE_SIZE,
            canvas_height_px: gla_core::IMAGE_TILE_SIZE,
            format,
            bytes_per_pixel,
            padded_bytes_per_row,
            atlas_tile_size: gla_core::ATLAS_TILE_SIZE,
            tiles: vec![WorkspaceExportTile {
                tile_index: 0,
                source_width: gla_core::IMAGE_TILE_SIZE,
                source_height: gla_core::IMAGE_TILE_SIZE,
                path: tile_path.clone(),
            }],
        };
        let snapshot = WorkspaceExportSnapshot {
            manifest,
            tiles: vec![WorkspaceExportTileAsset {
                tile_index: 0,
                source_width: gla_core::IMAGE_TILE_SIZE,
                source_height: gla_core::IMAGE_TILE_SIZE,
                path: tile_path,
                bytes: bytes.clone(),
            }],
        };
        let mut gpu = GpuRenderer::new(device, queue).unwrap();

        let workspace = workspace_from_export_snapshot(&mut gpu, snapshot).unwrap();

        assert_eq!(workspace.root(), ImageId::new(42));
        assert_eq!(workspace.version(), DocumentVersionId::new(2));
        let physical = workspace.root_physical_tiles().unwrap();
        assert_eq!(physical.len(), 1);
        assert_eq!(
            gpu.read_tile_bytes(physical[0].src, bytes_per_pixel)
                .unwrap(),
            bytes
        );
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

    async fn test_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .ok()?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("glaphica-export-test-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::default(),
            })
            .await
            .ok()?;
        Some((device, queue))
    }
}
