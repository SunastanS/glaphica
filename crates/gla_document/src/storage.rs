use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::document::{GlaDoc, GlaDocError};
use crate::node::{GlaBlendMode, GlaNodeId, GlaNodeKind};
use atlas::{BackendId, TileKey};
use gla_image::GlaImageLayout;
use glaphica_core::IMAGE_TILE_SIZE;

const DOCUMENT_FILE_NAME: &str = "document.bin";
const TILE_DIRECTORY_NAME: &str = "tiles";
const DOCUMENT_MAGIC: [u8; 8] = *b"GLADOC01";
const STORED_TILE_MAGIC: [u8; 8] = *b"GLATILE1";
const DOCUMENT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlaDocTileAsset {
    pub image_tile_index: usize,
    pub tile_key: TileKey,
    pub pixels_rgba8: Vec<u8>,
}

pub struct GlaDocLeafSource {
    pub node_id: GlaNodeId,
    pub tiles: Vec<GlaDocTileAsset>,
}

pub struct GlaDocLoadResult {
    pub doc: GlaDoc,
    pub leaf_sources: Vec<GlaDocLeafSource>,
    pub thumbnail_tiles: Vec<GlaDocTileAsset>,
}

#[derive(Debug, PartialEq)]
pub enum GlaDocStorageError {
    Io(std::io::ErrorKind),
    Document(GlaDocError),
    InvalidDocumentMagic,
    InvalidStoredTileMagic,
    UnsupportedDocumentVersion(u32),
    InvalidNodeKind(u8),
    InvalidBlendMode(u8),
    InvalidNodeIndex(u32),
    EmptyDocument,
    NonRootNodeAtRoot,
    RootNodeOutsideRootSlot,
    LeafNodeHasChildren(u32),
    MissingDocumentFile,
    MissingLeafSourceFile(u32),
    MissingThumbnailFile,
    NodeHasMultipleParents(u32),
    NodeIsUnreachable(u32),
    InvalidTilePixelCount { expected: usize, actual: usize },
}

impl std::fmt::Display for GlaDocStorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(kind) => write!(f, "io error: {kind:?}"),
            Self::Document(err) => std::fmt::Display::fmt(err, f),
            Self::InvalidDocumentMagic => write!(f, "invalid gla document magic"),
            Self::InvalidStoredTileMagic => write!(f, "invalid stored tile magic"),
            Self::UnsupportedDocumentVersion(version) => {
                write!(f, "unsupported gla document version {version}")
            }
            Self::InvalidNodeKind(value) => write!(f, "invalid serialized node kind {value}"),
            Self::InvalidBlendMode(value) => write!(f, "invalid serialized blend mode {value}"),
            Self::InvalidNodeIndex(index) => write!(f, "invalid serialized node index {index}"),
            Self::EmptyDocument => write!(f, "serialized gla document is empty"),
            Self::NonRootNodeAtRoot => write!(f, "serialized root slot must contain the root node"),
            Self::RootNodeOutsideRootSlot => {
                write!(f, "serialized root node must only appear at index 0")
            }
            Self::LeafNodeHasChildren(index) => {
                write!(f, "serialized leaf node {index} cannot have children")
            }
            Self::MissingDocumentFile => write!(f, "missing gla document file"),
            Self::MissingLeafSourceFile(index) => {
                write!(f, "missing serialized leaf source file for node {index}")
            }
            Self::MissingThumbnailFile => write!(f, "missing serialized thumbnail file"),
            Self::NodeHasMultipleParents(index) => {
                write!(f, "serialized node {index} has multiple parents")
            }
            Self::NodeIsUnreachable(index) => {
                write!(f, "serialized node {index} is unreachable from the root")
            }
            Self::InvalidTilePixelCount { expected, actual } => write!(
                f,
                "stored tile pixel count mismatch: expected {expected} bytes, got {actual}"
            ),
        }
    }
}

impl std::error::Error for GlaDocStorageError {}

impl From<std::io::Error> for GlaDocStorageError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.kind())
    }
}

impl From<GlaDocError> for GlaDocStorageError {
    fn from(error: GlaDocError) -> Self {
        Self::Document(error)
    }
}

#[derive(Clone, Copy)]
struct SerializedNode {
    kind: GlaNodeKind,
    opacity: f32,
    blend_mode: GlaBlendMode,
    child_range_start: usize,
    child_range_len: usize,
}

impl GlaDoc {
    pub fn encode_binary(&self) -> Result<Vec<u8>, GlaDocStorageError> {
        let mut serialized_node_ids = Vec::new();
        self.collect_subtree_preorder(self.root_id(), &mut serialized_node_ids)?;

        let mut node_indices = slotmap::SecondaryMap::new();
        for (serialized_index, &node_id) in serialized_node_ids.iter().enumerate() {
            node_indices.insert(node_id, serialized_index);
        }

        let mut document_bytes = Vec::new();
        document_bytes.extend_from_slice(&DOCUMENT_MAGIC);
        write_u32(&mut document_bytes, DOCUMENT_VERSION);
        write_u32(&mut document_bytes, self.layout().size_x());
        write_u32(&mut document_bytes, self.layout().size_y());

        let active_layer_index = node_indices.get(self.active_layer_id()).copied().ok_or(
            GlaDocStorageError::Document(GlaDocError::InvalidNodeId(self.active_layer_id())),
        )?;
        write_u32(&mut document_bytes, as_u32(active_layer_index)?);
        write_u32(&mut document_bytes, as_u32(serialized_node_ids.len())?);

        for &node_id in &serialized_node_ids {
            let node = self.node(node_id)?;
            write_u8(&mut document_bytes, encode_node_kind(node.kind()));
            write_f32(&mut document_bytes, node.opacity());
            write_u8(&mut document_bytes, encode_blend_mode(node.blend_mode()));

            let children = node.children().unwrap_or(&[]);
            write_u32(&mut document_bytes, as_u32(children.len())?);
            for &child_id in children {
                let child_index =
                    node_indices
                        .get(child_id)
                        .copied()
                        .ok_or(GlaDocStorageError::Document(GlaDocError::InvalidNodeId(
                            child_id,
                        )))?;
                write_u32(&mut document_bytes, as_u32(child_index)?);
            }
        }

        Ok(document_bytes)
    }

    pub fn load_directory(
        path: impl AsRef<Path>,
        image_backend: BackendId,
        render_backend: BackendId,
    ) -> Result<GlaDocLoadResult, GlaDocStorageError> {
        let root_path = path.as_ref();
        let document_path = root_path.join(DOCUMENT_FILE_NAME);
        if !document_path.exists() {
            return Err(GlaDocStorageError::MissingDocumentFile);
        }

        let bytes = fs::read(document_path)?;
        let (layout, active_layer_index, nodes, child_indices) = parse_document_bytes(&bytes)?;
        validate_serialized_nodes(&nodes, &child_indices)?;

        let mut doc = GlaDoc::new(layout, image_backend, render_backend)?;
        doc.set_opacity(doc.root_id(), nodes[0].opacity)?;
        doc.set_blend_mode(doc.root_id(), nodes[0].blend_mode)?;

        let mut live_node_ids = vec![doc.root_id(); nodes.len()];
        let mut stack = vec![(0usize, doc.root_id(), 0usize)];
        while let Some((serialized_parent_index, live_parent_id, next_child_index)) =
            stack.last_mut()
        {
            let parent = nodes[*serialized_parent_index];
            if *next_child_index >= parent.child_range_len {
                stack.pop();
                continue;
            }

            let child_offset = parent.child_range_start + *next_child_index;
            let serialized_child_index = child_indices[child_offset];
            *next_child_index += 1;

            let child = nodes[serialized_child_index];
            let insert_index = doc.child_ids(*live_parent_id)?.len();
            let live_child_id = match child.kind {
                GlaNodeKind::Root => return Err(GlaDocStorageError::RootNodeOutsideRootSlot),
                GlaNodeKind::Branch => doc.insert_group(*live_parent_id, insert_index)?,
                GlaNodeKind::Leaf => doc.insert_layer(*live_parent_id, insert_index)?,
            };
            doc.set_opacity(live_child_id, child.opacity)?;
            doc.set_blend_mode(live_child_id, child.blend_mode)?;
            live_node_ids[serialized_child_index] = live_child_id;

            if child.kind != GlaNodeKind::Leaf {
                stack.push((serialized_child_index, live_child_id, 0));
            }
        }

        doc.set_active_layer(live_node_ids[active_layer_index])?;

        let mut leaf_sources = Vec::new();
        for (serialized_index, node) in nodes.iter().copied().enumerate() {
            if node.kind != GlaNodeKind::Leaf {
                continue;
            }

            leaf_sources.push(GlaDocLeafSource {
                node_id: live_node_ids[serialized_index],
                tiles: read_tile_assets_in_directory(
                    &root_path.join(node_tile_directory(serialized_index)),
                )?,
            });
        }

        let thumbnail_tiles =
            read_tile_assets_in_directory(&root_path.join(node_tile_directory(0)))?;

        Ok(GlaDocLoadResult {
            doc,
            leaf_sources,
            thumbnail_tiles,
        })
    }
}

pub fn tile_asset_relative_path(serialized_index: usize, tile_key: TileKey) -> PathBuf {
    node_tile_directory(serialized_index).join(format!("{}.bin", tile_key_file_stem(tile_key)))
}

pub fn write_tile_asset_file(
    path: impl AsRef<Path>,
    tile: &GlaDocTileAsset,
) -> Result<(), GlaDocStorageError> {
    write_stored_tile_file(path.as_ref(), tile)
}

fn validate_serialized_nodes(
    nodes: &[SerializedNode],
    child_indices: &[usize],
) -> Result<(), GlaDocStorageError> {
    if nodes.is_empty() {
        return Err(GlaDocStorageError::EmptyDocument);
    }
    if nodes[0].kind != GlaNodeKind::Root {
        return Err(GlaDocStorageError::NonRootNodeAtRoot);
    }

    let mut parent_counts = vec![0u32; nodes.len()];
    for (index, node) in nodes.iter().copied().enumerate() {
        if index != 0 && node.kind == GlaNodeKind::Root {
            return Err(GlaDocStorageError::RootNodeOutsideRootSlot);
        }
        if node.kind == GlaNodeKind::Leaf && node.child_range_len != 0 {
            return Err(GlaDocStorageError::LeafNodeHasChildren(as_u32(index)?));
        }

        let child_range_end = node.child_range_start + node.child_range_len;
        for &child_index in &child_indices[node.child_range_start..child_range_end] {
            if child_index >= nodes.len() {
                return Err(GlaDocStorageError::InvalidNodeIndex(as_u32(child_index)?));
            }
            if child_index == 0 {
                return Err(GlaDocStorageError::RootNodeOutsideRootSlot);
            }
            let parent_count = &mut parent_counts[child_index];
            *parent_count = parent_count.saturating_add(1);
            if *parent_count > 1 {
                return Err(GlaDocStorageError::NodeHasMultipleParents(as_u32(
                    child_index,
                )?));
            }
        }
    }

    for (index, &parent_count) in parent_counts.iter().enumerate().skip(1) {
        if parent_count == 0 {
            return Err(GlaDocStorageError::NodeIsUnreachable(as_u32(index)?));
        }
    }

    Ok(())
}

fn parse_document_bytes(
    bytes: &[u8],
) -> Result<(GlaImageLayout, usize, Vec<SerializedNode>, Vec<usize>), GlaDocStorageError> {
    let mut cursor = std::io::Cursor::new(bytes);
    let mut magic = [0u8; 8];
    cursor.read_exact(&mut magic)?;
    if magic != DOCUMENT_MAGIC {
        return Err(GlaDocStorageError::InvalidDocumentMagic);
    }

    let version = read_u32(&mut cursor)?;
    if version != DOCUMENT_VERSION {
        return Err(GlaDocStorageError::UnsupportedDocumentVersion(version));
    }

    let layout = GlaImageLayout::new(read_u32(&mut cursor)?, read_u32(&mut cursor)?);
    let active_layer_index = usize_from_u32(read_u32(&mut cursor)?)?;
    let node_count = usize_from_u32(read_u32(&mut cursor)?)?;

    let mut nodes = Vec::with_capacity(node_count);
    let mut child_indices = Vec::new();
    for _ in 0..node_count {
        let kind = decode_node_kind(read_u8(&mut cursor)?)?;
        let opacity = read_f32(&mut cursor)?;
        let blend_mode = decode_blend_mode(read_u8(&mut cursor)?)?;
        let child_count = usize_from_u32(read_u32(&mut cursor)?)?;
        let child_range_start = child_indices.len();
        for _ in 0..child_count {
            child_indices.push(usize_from_u32(read_u32(&mut cursor)?)?);
        }
        nodes.push(SerializedNode {
            kind,
            opacity,
            blend_mode,
            child_range_start,
            child_range_len: child_count,
        });
    }

    if active_layer_index >= nodes.len() {
        return Err(GlaDocStorageError::InvalidNodeIndex(as_u32(
            active_layer_index,
        )?));
    }

    Ok((layout, active_layer_index, nodes, child_indices))
}

pub(crate) fn node_tile_directory(serialized_index: usize) -> PathBuf {
    PathBuf::from(TILE_DIRECTORY_NAME).join(serialized_index.to_string())
}

fn write_stored_tile_file(path: &Path, tile: &GlaDocTileAsset) -> Result<(), GlaDocStorageError> {
    validate_tile_pixels_rgba8_len(tile.pixels_rgba8.len())?;
    let parent = path
        .parent()
        .ok_or(GlaDocStorageError::Io(std::io::ErrorKind::InvalidInput))?;
    fs::create_dir_all(parent)?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&STORED_TILE_MAGIC);
    write_u32(&mut bytes, as_u32(tile.image_tile_index)?);
    write_u64(&mut bytes, tile_key_to_u64(tile.tile_key));
    write_u32(&mut bytes, as_u32(tile.pixels_rgba8.len())?);
    bytes.extend_from_slice(&tile.pixels_rgba8);
    fs::write(path, bytes)?;
    Ok(())
}

fn read_stored_tile_file(path: &Path) -> Result<GlaDocTileAsset, GlaDocStorageError> {
    let bytes = fs::read(path)?;
    let mut cursor = std::io::Cursor::new(bytes);
    let mut magic = [0u8; 8];
    cursor.read_exact(&mut magic)?;
    if magic != STORED_TILE_MAGIC {
        return Err(GlaDocStorageError::InvalidStoredTileMagic);
    }

    let image_tile_index = usize_from_u32(read_u32(&mut cursor)?)?;
    let tile_key = tile_key_from_u64(read_u64(&mut cursor)?);
    let pixel_len = usize_from_u32(read_u32(&mut cursor)?)?;
    validate_tile_pixels_rgba8_len(pixel_len)?;
    let mut pixels_rgba8 = vec![0; pixel_len];
    cursor.read_exact(&mut pixels_rgba8)?;
    Ok(GlaDocTileAsset {
        image_tile_index,
        tile_key,
        pixels_rgba8,
    })
}

fn read_tile_assets_in_directory(path: &Path) -> Result<Vec<GlaDocTileAsset>, GlaDocStorageError> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let mut file_paths = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        file_paths.push(entry.path());
    }
    file_paths.sort();

    let mut tiles = Vec::with_capacity(file_paths.len());
    for file_path in file_paths {
        tiles.push(read_stored_tile_file(&file_path)?);
    }
    Ok(tiles)
}

fn validate_tile_pixels_rgba8_len(len: usize) -> Result<(), GlaDocStorageError> {
    let expected = (IMAGE_TILE_SIZE as usize)
        .checked_mul(IMAGE_TILE_SIZE as usize)
        .and_then(|pixels: usize| pixels.checked_mul(4))
        .ok_or(GlaDocStorageError::InvalidTilePixelCount {
            expected: usize::MAX,
            actual: len,
        })?;
    if len == expected {
        Ok(())
    } else {
        Err(GlaDocStorageError::InvalidTilePixelCount {
            expected,
            actual: len,
        })
    }
}

fn encode_node_kind(kind: GlaNodeKind) -> u8 {
    match kind {
        GlaNodeKind::Root => 0,
        GlaNodeKind::Branch => 1,
        GlaNodeKind::Leaf => 2,
    }
}

fn decode_node_kind(value: u8) -> Result<GlaNodeKind, GlaDocStorageError> {
    match value {
        0 => Ok(GlaNodeKind::Root),
        1 => Ok(GlaNodeKind::Branch),
        2 => Ok(GlaNodeKind::Leaf),
        _ => Err(GlaDocStorageError::InvalidNodeKind(value)),
    }
}

fn encode_blend_mode(blend_mode: GlaBlendMode) -> u8 {
    match blend_mode {
        GlaBlendMode::Normal => 0,
        GlaBlendMode::Multiply => 1,
    }
}

fn decode_blend_mode(value: u8) -> Result<GlaBlendMode, GlaDocStorageError> {
    match value {
        0 => Ok(GlaBlendMode::Normal),
        1 => Ok(GlaBlendMode::Multiply),
        _ => Err(GlaDocStorageError::InvalidBlendMode(value)),
    }
}

fn write_u8(output: &mut Vec<u8>, value: u8) {
    output.push(value);
}

fn write_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn write_f32(output: &mut Vec<u8>, value: f32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn read_u8(reader: &mut impl Read) -> Result<u8, GlaDocStorageError> {
    let mut bytes = [0u8; 1];
    reader.read_exact(&mut bytes)?;
    Ok(bytes[0])
}

fn read_u32(reader: &mut impl Read) -> Result<u32, GlaDocStorageError> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_f32(reader: &mut impl Read) -> Result<f32, GlaDocStorageError> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(f32::from_le_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> Result<u64, GlaDocStorageError> {
    let mut bytes = [0u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

pub(crate) fn as_u32(value: usize) -> Result<u32, GlaDocStorageError> {
    u32::try_from(value).map_err(|_| GlaDocStorageError::InvalidNodeIndex(u32::MAX))
}

fn usize_from_u32(value: u32) -> Result<usize, GlaDocStorageError> {
    usize::try_from(value).map_err(|_| GlaDocStorageError::InvalidNodeIndex(value))
}

fn tile_key_file_stem(tile_key: TileKey) -> String {
    let parts = tile_key.parts();
    format!(
        "{:02x}-{:06x}-{:08x}",
        parts.backend_id.raw(),
        parts.generation,
        parts.slot_index
    )
}

fn tile_key_to_u64(tile_key: TileKey) -> u64 {
    unsafe { std::mem::transmute::<TileKey, u64>(tile_key) }
}

fn tile_key_from_u64(value: u64) -> TileKey {
    unsafe { std::mem::transmute::<u64, TileKey>(value) }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use atlas::{AtlasLayout, Backend, BackendId};
    use glaphica_core::IMAGE_TILE_SIZE;

    use crate::{GlaDoc, GlaImageLayout, GlaNodeKind};

    use super::GlaDocTileAsset;

    #[test]
    fn plan_write_and_load_round_trip_preserves_tree_and_assets() {
        let mut doc = new_doc(BackendId::new(3), BackendId::new(7));
        let image_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(3));
        let render_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(7));
        doc.set_opacity(doc.root_id(), 0.75)
            .expect("root opacity should update");
        doc.set_blend_mode(doc.root_id(), crate::GlaBlendMode::Multiply)
            .expect("root blend should update");

        let group_id = doc
            .append_group(doc.root_id())
            .expect("group should append");
        let root_layer_id = doc
            .append_layer(doc.root_id())
            .expect("root layer should append");
        let nested_layer_id = doc
            .append_layer(group_id)
            .expect("nested layer should append");

        doc.set_opacity(group_id, 0.5)
            .expect("group opacity should update");
        doc.set_blend_mode(nested_layer_id, crate::GlaBlendMode::Multiply)
            .expect("nested layer blend should update");
        doc.set_active_layer(nested_layer_id)
            .expect("active layer should update");
        let root_id = doc.root_id();
        assign_tile(&mut doc, root_id, 0, &render_backend);
        assign_tile(&mut doc, nested_layer_id, 0, &image_backend);
        assign_tile(&mut doc, root_layer_id, 0, &image_backend);

        let temp_dir = temp_directory("gla-doc-round-trip");
        let expected_root_tile_key = tile_key_for_node(&doc, doc.root_id(), 0);
        let expected_nested_tile_key = tile_key_for_node(&doc, nested_layer_id, 0);
        let expected_root_layer_tile_key = tile_key_for_node(&doc, root_layer_id, 0);
        write_document_fixture(&doc, &temp_dir, &[91, 29, 11], &[(0, 0), (2, 0), (3, 0)])
            .expect("fixture should write");

        let loaded = GlaDoc::load_directory(&temp_dir, BackendId::new(13), BackendId::new(17))
            .expect("document should load");
        let loaded_doc = loaded.doc;

        let mut preorder = Vec::new();
        loaded_doc
            .collect_subtree_preorder(loaded_doc.root_id(), &mut preorder)
            .expect("preorder should collect");

        assert_eq!(
            loaded_doc.layout(),
            GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE)
        );
        assert_eq!(loaded_doc.image_backend(), BackendId::new(13));
        assert_eq!(loaded_doc.render_backend(), BackendId::new(17));
        assert_eq!(preorder.len(), 4);

        let loaded_root = loaded_doc.node(preorder[0]).expect("root should exist");
        let loaded_group = loaded_doc.node(preorder[1]).expect("group should exist");
        let loaded_nested_layer = loaded_doc
            .node(preorder[2])
            .expect("nested layer should exist");
        let loaded_root_layer = loaded_doc
            .node(preorder[3])
            .expect("root layer should exist");

        assert_eq!(loaded_root.kind(), GlaNodeKind::Root);
        assert_eq!(loaded_root.opacity(), 0.75);
        assert_eq!(loaded_root.blend_mode(), crate::GlaBlendMode::Multiply);
        assert_eq!(loaded_group.kind(), GlaNodeKind::Branch);
        assert_eq!(loaded_group.opacity(), 0.5);
        assert_eq!(loaded_nested_layer.kind(), GlaNodeKind::Leaf);
        assert_eq!(
            loaded_nested_layer.blend_mode(),
            crate::GlaBlendMode::Multiply
        );
        assert_eq!(loaded_root_layer.kind(), GlaNodeKind::Leaf);

        assert_eq!(loaded_doc.active_layer_id(), preorder[2]);
        assert_eq!(
            loaded_doc.active_layer_ancestor_chain(),
            &[preorder[2], preorder[1], preorder[0]]
        );

        assert_eq!(loaded.leaf_sources.len(), 2);
        assert_eq!(loaded.leaf_sources[0].node_id, preorder[2]);
        assert_eq!(
            loaded.leaf_sources[0].tiles,
            vec![test_tile(expected_nested_tile_key, 0, 29)]
        );
        assert_eq!(loaded.leaf_sources[1].node_id, preorder[3]);
        assert_eq!(
            loaded.leaf_sources[1].tiles,
            vec![test_tile(expected_root_layer_tile_key, 0, 11)]
        );
        assert_eq!(
            loaded.thumbnail_tiles,
            vec![test_tile(expected_root_tile_key, 0, 91)]
        );

        std::fs::remove_dir_all(temp_dir).expect("temp directory should remove");
        assert_eq!(root_layer_id, root_layer_id);
    }

    fn new_doc(image_backend: BackendId, render_backend: BackendId) -> GlaDoc {
        GlaDoc::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE),
            image_backend,
            render_backend,
        )
        .expect("document should build")
    }

    fn test_tile(tile_key: atlas::TileKey, image_tile_index: usize, seed: u8) -> GlaDocTileAsset {
        let mut pixels = vec![0; (IMAGE_TILE_SIZE * IMAGE_TILE_SIZE * 4) as usize];
        for (index, value) in pixels.iter_mut().enumerate() {
            *value = seed.wrapping_add(index as u8);
        }
        GlaDocTileAsset {
            image_tile_index,
            tile_key,
            pixels_rgba8: pixels,
        }
    }

    fn assign_tile(
        doc: &mut GlaDoc,
        node_id: crate::GlaNodeId,
        tile_index: usize,
        backend: &Backend,
    ) {
        let owner = backend.alloc_active().expect("tile should allocate");
        doc.node_image_mut(node_id)
            .expect("node image should exist")
            .replace_tile_owner(tile_index, owner)
            .expect("tile owner should replace");
    }

    fn tile_key_for_node(
        doc: &GlaDoc,
        node_id: crate::GlaNodeId,
        tile_index: usize,
    ) -> atlas::TileKey {
        doc.node_image(node_id)
            .expect("node image should exist")
            .tile_key(tile_index)
            .expect("tile key should exist")
    }

    fn write_document_fixture(
        doc: &GlaDoc,
        root_path: &std::path::Path,
        seeds: &[u8],
        node_tiles: &[(usize, usize)],
    ) -> Result<(), super::GlaDocStorageError> {
        let mut serialized_node_ids = Vec::new();
        doc.collect_subtree_preorder(doc.root_id(), &mut serialized_node_ids)?;

        let mut node_indices = slotmap::SecondaryMap::new();
        for (serialized_index, &node_id) in serialized_node_ids.iter().enumerate() {
            node_indices.insert(node_id, serialized_index);
        }

        let mut document_bytes = Vec::new();
        document_bytes.extend_from_slice(super::DOCUMENT_MAGIC.as_slice());
        super::write_u32(&mut document_bytes, super::DOCUMENT_VERSION);
        super::write_u32(&mut document_bytes, doc.layout().size_x());
        super::write_u32(&mut document_bytes, doc.layout().size_y());
        let active_layer_index = node_indices.get(doc.active_layer_id()).copied().ok_or(
            super::GlaDocStorageError::Document(crate::GlaDocError::InvalidNodeId(
                doc.active_layer_id(),
            )),
        )?;
        super::write_u32(&mut document_bytes, super::as_u32(active_layer_index)?);
        super::write_u32(
            &mut document_bytes,
            super::as_u32(serialized_node_ids.len())?,
        );

        for &node_id in &serialized_node_ids {
            let node = doc.node(node_id)?;
            super::write_u8(&mut document_bytes, super::encode_node_kind(node.kind()));
            super::write_f32(&mut document_bytes, node.opacity());
            super::write_u8(
                &mut document_bytes,
                super::encode_blend_mode(node.blend_mode()),
            );
            let children = node.children().unwrap_or(&[]);
            super::write_u32(&mut document_bytes, super::as_u32(children.len())?);
            for &child_id in children {
                super::write_u32(
                    &mut document_bytes,
                    super::as_u32(node_indices.get(child_id).copied().expect("child index"))?,
                );
            }
        }

        std::fs::create_dir_all(root_path)?;
        let tile_directory = root_path.join(super::TILE_DIRECTORY_NAME);
        if tile_directory.exists() {
            std::fs::remove_dir_all(&tile_directory)?;
        }
        std::fs::create_dir_all(&tile_directory)?;
        std::fs::write(root_path.join(super::DOCUMENT_FILE_NAME), document_bytes)?;

        for ((serialized_index, image_tile_index), seed) in
            node_tiles.iter().copied().zip(seeds.iter().copied())
        {
            let node_id = serialized_node_ids[serialized_index];
            let tile_key = tile_key_for_node(doc, node_id, image_tile_index);
            let tile = test_tile(tile_key, image_tile_index, seed);
            super::write_stored_tile_file(
                &root_path.join(super::tile_asset_relative_path(serialized_index, tile_key)),
                &tile,
            )?;
        }

        Ok(())
    }

    fn temp_directory(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{unique}"))
    }
}
