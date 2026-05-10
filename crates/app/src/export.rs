use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::Path;

use atlas::AtlasLayout;
use gla_document::{
    GlaDoc, GlaDocStorageError, GlaDocTileAsset, GlaImage, GlaNodeKind, tile_asset_relative_path,
    write_tile_asset_file,
};
use gla_image::GlaImageTileAccessError;
use glaphica_core::{AlphaMode, ColorProfile};
use renderer::{
    GpuContext, RendererTexture, TextureColorRuntime, TextureIoError,
    tile_image_export::{TileImageExportRequest, readback_image_tiles_rgba8},
};

const DOCUMENT_FILE_NAME: &str = "document.bin";
const TILE_DIRECTORY_NAME: &str = "tiles";

#[derive(Debug)]
pub enum AppExportError {
    Renderer(TextureIoError),
    Document(GlaDocStorageError),
    Image(GlaImageTileAccessError),
}

impl Display for AppExportError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Renderer(error) => Display::fmt(error, f),
            Self::Document(error) => Display::fmt(error, f),
            Self::Image(error) => Display::fmt(error, f),
        }
    }
}

impl Error for AppExportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Renderer(error) => Some(error),
            Self::Document(error) => Some(error),
            Self::Image(error) => Some(error),
        }
    }
}

impl From<TextureIoError> for AppExportError {
    fn from(error: TextureIoError) -> Self {
        Self::Renderer(error)
    }
}

impl From<GlaDocStorageError> for AppExportError {
    fn from(error: GlaDocStorageError) -> Self {
        Self::Document(error)
    }
}

impl From<GlaImageTileAccessError> for AppExportError {
    fn from(error: GlaImageTileAccessError) -> Self {
        Self::Image(error)
    }
}

impl From<std::io::Error> for AppExportError {
    fn from(error: std::io::Error) -> Self {
        Self::Document(GlaDocStorageError::Io(error.kind()))
    }
}

impl From<gla_document::GlaDocError> for AppExportError {
    fn from(error: gla_document::GlaDocError) -> Self {
        Self::Document(GlaDocStorageError::Document(error))
    }
}

pub fn export_document_directory(
    doc: &GlaDoc,
    path: impl AsRef<Path>,
    runtime: &TextureColorRuntime,
    gpu_context: &GpuContext,
    atlas_layout: AtlasLayout,
    atlas_texture: &RendererTexture,
    destination_profile: ColorProfile,
    alpha_mode: AlphaMode,
) -> Result<(), AppExportError> {
    let root_path = path.as_ref().to_path_buf();
    let mut serialized_node_ids = Vec::new();
    doc.collect_subtree_preorder(doc.root_id(), &mut serialized_node_ids)?;

    fs::create_dir_all(&root_path)?;
    let tile_directory = root_path.join(TILE_DIRECTORY_NAME);
    if tile_directory.exists() {
        fs::remove_dir_all(&tile_directory)?;
    }
    fs::create_dir_all(&tile_directory)?;
    let document_bytes = doc.encode_binary()?;

    for (serialized_index, &node_id) in serialized_node_ids.iter().enumerate() {
        let node = doc.node(node_id)?;
        if matches!(node.kind(), GlaNodeKind::Leaf | GlaNodeKind::Root) {
            export_node_tiles(
                &root_path,
                serialized_index,
                node.image(),
                runtime,
                gpu_context,
                atlas_layout,
                atlas_texture,
                destination_profile.clone(),
                alpha_mode,
            )?;
        }
    }

    fs::write(root_path.join(DOCUMENT_FILE_NAME), document_bytes)?;
    Ok(())
}

fn export_node_tiles(
    root_path: &Path,
    serialized_index: usize,
    image: &GlaImage,
    runtime: &TextureColorRuntime,
    gpu_context: &GpuContext,
    atlas_layout: AtlasLayout,
    atlas_texture: &RendererTexture,
    destination_profile: ColorProfile,
    alpha_mode: AlphaMode,
) -> Result<(), AppExportError> {
    let mut tile_keys = Vec::new();
    let mut image_tiles = Vec::new();

    for tile_index in 0..image.slot_count() {
        let Some(tile_key) = image.physical_tile_key(tile_index)? else {
            continue;
        };

        tile_keys.push(tile_key);
        image_tiles.push((tile_index, tile_key));
    }

    if image_tiles.is_empty() {
        return Ok(());
    }

    let request = TileImageExportRequest::from_image_tiles(
        atlas_layout,
        image.layout().size_x(),
        image.layout().size_y(),
        &image_tiles,
    )?;

    let readbacks = readback_image_tiles_rgba8(
        runtime,
        &gpu_context.device,
        &gpu_context.queue,
        atlas_texture,
        &request,
        destination_profile,
        alpha_mode,
    )?;

    for (tile_key, readback) in tile_keys.into_iter().zip(readbacks) {
        write_tile_asset_file(
            root_path.join(tile_asset_relative_path(serialized_index, tile_key)),
            &GlaDocTileAsset {
                image_tile_index: readback.image_tile_index,
                tile_key,
                pixels_rgba8: readback.pixels_rgba8,
            },
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use atlas::{AtlasLayout, Backend, BackendId};

    use gla_document::tile_asset_relative_path;

    #[test]
    fn tile_asset_relative_path_matches_tile_identity() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(3));
        let owner = backend.alloc_active().expect("tile should allocate");
        let relative_path = tile_asset_relative_path(9, owner.tile_key());

        assert!(relative_path.starts_with("tiles/9"));
        assert!(relative_path.extension().is_some_and(|ext| ext == "bin"));
    }
}
