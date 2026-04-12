use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use atlas::BackendId;
use gla_image::{GlaImage, GlaImageLayout, GlaStoredImage, GlaStoredImageError};
use slotmap::SecondaryMap;

use crate::document::{GlaDoc, GlaDocError};
use crate::node::{GlaBlendMode, GlaNodeId, GlaNodeKind};

const DOCUMENT_FILE_NAME: &str = "document.bin";
const THUMBNAIL_FILE_NAME: &str = "thumbnail.bin";
const TILE_DIRECTORY_NAME: &str = "tiles";
const DOCUMENT_MAGIC: [u8; 8] = *b"GLADOC01";
const STORED_IMAGE_MAGIC: [u8; 8] = *b"GLAIMG01";
const DOCUMENT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlaDocImageAssetKind {
    LeafSource { node_id: GlaNodeId },
    Thumbnail,
}

pub struct GlaDocImageExportRequest {
    asset_kind: GlaDocImageAssetKind,
    source_node_id: GlaNodeId,
    relative_path: PathBuf,
}

impl GlaDocImageExportRequest {
    pub fn asset_kind(&self) -> GlaDocImageAssetKind {
        self.asset_kind
    }

    pub fn source_node_id(&self) -> GlaNodeId {
        self.source_node_id
    }

    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }
}

pub struct GlaDocDirectorySavePlan {
    root_path: PathBuf,
    document_bytes: Vec<u8>,
    export_requests: Vec<GlaDocImageExportRequest>,
}

impl GlaDocDirectorySavePlan {
    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn export_requests(&self) -> &[GlaDocImageExportRequest] {
        &self.export_requests
    }

    pub fn source_image<'a>(
        &self,
        doc: &'a GlaDoc,
        request: &GlaDocImageExportRequest,
    ) -> Result<&'a GlaImage, GlaDocStorageError> {
        Ok(doc.node_image(request.source_node_id())?)
    }

    pub fn write_exported_images(
        self,
        exported_images: &[GlaStoredImage],
    ) -> Result<(), GlaDocStorageError> {
        if exported_images.len() != self.export_requests.len() {
            return Err(GlaDocStorageError::ExportedImageCountMismatch {
                expected: self.export_requests.len(),
                actual: exported_images.len(),
            });
        }

        fs::create_dir_all(&self.root_path)?;
        let tile_directory = self.root_path.join(TILE_DIRECTORY_NAME);
        if tile_directory.exists() {
            fs::remove_dir_all(&tile_directory)?;
        }
        fs::create_dir_all(&tile_directory)?;

        fs::write(self.root_path.join(DOCUMENT_FILE_NAME), self.document_bytes)?;

        for (request, image) in self.export_requests.iter().zip(exported_images) {
            let absolute_path = self.root_path.join(request.relative_path());
            write_stored_image_file(&absolute_path, image)?;
        }

        Ok(())
    }
}

pub struct GlaDocLeafSource {
    pub node_id: GlaNodeId,
    pub image: GlaStoredImage,
}

pub struct GlaDocLoadResult {
    pub doc: GlaDoc,
    pub leaf_sources: Vec<GlaDocLeafSource>,
    pub thumbnail: GlaStoredImage,
}

#[derive(Debug, PartialEq)]
pub enum GlaDocStorageError {
    Io(std::io::ErrorKind),
    Document(GlaDocError),
    StoredImage(GlaStoredImageError),
    InvalidDocumentMagic,
    InvalidStoredImageMagic,
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
    ExportedImageCountMismatch { expected: usize, actual: usize },
}

impl std::fmt::Display for GlaDocStorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(kind) => write!(f, "io error: {kind:?}"),
            Self::Document(err) => std::fmt::Display::fmt(err, f),
            Self::StoredImage(err) => std::fmt::Display::fmt(err, f),
            Self::InvalidDocumentMagic => write!(f, "invalid gla document magic"),
            Self::InvalidStoredImageMagic => write!(f, "invalid stored image magic"),
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
            Self::ExportedImageCountMismatch { expected, actual } => write!(
                f,
                "exported image count mismatch: expected {expected}, got {actual}"
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

impl From<GlaStoredImageError> for GlaDocStorageError {
    fn from(error: GlaStoredImageError) -> Self {
        Self::StoredImage(error)
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
    pub fn plan_directory_save(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<GlaDocDirectorySavePlan, GlaDocStorageError> {
        let root_path = path.as_ref().to_path_buf();
        let mut serialized_node_ids = Vec::new();
        self.collect_subtree_preorder(self.root_id(), &mut serialized_node_ids)?;

        let mut node_indices = SecondaryMap::new();
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

        let mut export_requests = Vec::new();
        for (serialized_index, &node_id) in serialized_node_ids.iter().enumerate() {
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

            match node.kind() {
                GlaNodeKind::Leaf => export_requests.push(GlaDocImageExportRequest {
                    asset_kind: GlaDocImageAssetKind::LeafSource { node_id },
                    source_node_id: node_id,
                    relative_path: tile_relative_path(serialized_index),
                }),
                GlaNodeKind::Root => export_requests.push(GlaDocImageExportRequest {
                    asset_kind: GlaDocImageAssetKind::Thumbnail,
                    source_node_id: node_id,
                    relative_path: PathBuf::from(THUMBNAIL_FILE_NAME),
                }),
                GlaNodeKind::Branch => {}
            }
        }

        Ok(GlaDocDirectorySavePlan {
            root_path,
            document_bytes,
            export_requests,
        })
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

            let image_path = root_path.join(tile_relative_path(serialized_index));
            if !image_path.exists() {
                return Err(GlaDocStorageError::MissingLeafSourceFile(as_u32(
                    serialized_index,
                )?));
            }

            leaf_sources.push(GlaDocLeafSource {
                node_id: live_node_ids[serialized_index],
                image: read_stored_image_file(&image_path)?,
            });
        }

        let thumbnail_path = root_path.join(THUMBNAIL_FILE_NAME);
        if !thumbnail_path.exists() {
            return Err(GlaDocStorageError::MissingThumbnailFile);
        }
        let thumbnail = read_stored_image_file(&thumbnail_path)?;

        Ok(GlaDocLoadResult {
            doc,
            leaf_sources,
            thumbnail,
        })
    }
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

fn tile_relative_path(serialized_index: usize) -> PathBuf {
    PathBuf::from(TILE_DIRECTORY_NAME).join(format!("{serialized_index}.bin"))
}

fn write_stored_image_file(path: &Path, image: &GlaStoredImage) -> Result<(), GlaDocStorageError> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&STORED_IMAGE_MAGIC);
    write_u32(&mut bytes, image.width());
    write_u32(&mut bytes, image.height());
    write_u32(&mut bytes, as_u32(image.pixels_rgba8().len())?);
    bytes.extend_from_slice(image.pixels_rgba8());
    fs::write(path, bytes)?;
    Ok(())
}

fn read_stored_image_file(path: &Path) -> Result<GlaStoredImage, GlaDocStorageError> {
    let bytes = fs::read(path)?;
    let mut cursor = std::io::Cursor::new(bytes);
    let mut magic = [0u8; 8];
    cursor.read_exact(&mut magic)?;
    if magic != STORED_IMAGE_MAGIC {
        return Err(GlaDocStorageError::InvalidStoredImageMagic);
    }

    let width = read_u32(&mut cursor)?;
    let height = read_u32(&mut cursor)?;
    let pixel_len = usize_from_u32(read_u32(&mut cursor)?)?;
    let mut pixels_rgba8 = vec![0; pixel_len];
    cursor.read_exact(&mut pixels_rgba8)?;
    GlaStoredImage::new_rgba8(width, height, pixels_rgba8).map_err(Into::into)
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

fn as_u32(value: usize) -> Result<u32, GlaDocStorageError> {
    u32::try_from(value).map_err(|_| GlaDocStorageError::InvalidNodeIndex(u32::MAX))
}

fn usize_from_u32(value: u32) -> Result<usize, GlaDocStorageError> {
    usize::try_from(value).map_err(|_| GlaDocStorageError::InvalidNodeIndex(value))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use atlas::BackendId;
    use glaphica_core::IMAGE_TILE_SIZE;

    use crate::{GlaDoc, GlaImageLayout, GlaNodeKind};

    use super::{GlaDocImageAssetKind, GlaDocStorageError};

    #[test]
    fn plan_directory_save_emits_leaf_sources_and_thumbnail_requests() {
        let mut doc = new_doc(BackendId::new(3), BackendId::new(7));
        let group_id = doc
            .append_group(doc.root_id())
            .expect("group should append");
        let root_layer_id = doc
            .append_layer(doc.root_id())
            .expect("root layer should append");
        let nested_layer_id = doc
            .append_layer(group_id)
            .expect("nested layer should append");

        let plan = doc
            .plan_directory_save(temp_directory("gla-doc-save-plan"))
            .expect("save plan should build");

        assert_eq!(plan.export_requests().len(), 3);
        assert_eq!(
            plan.export_requests()[0].asset_kind(),
            GlaDocImageAssetKind::Thumbnail
        );
        assert_eq!(plan.export_requests()[1].source_node_id(), nested_layer_id);
        assert_eq!(
            plan.export_requests()[1].asset_kind(),
            GlaDocImageAssetKind::LeafSource {
                node_id: nested_layer_id
            }
        );
        assert_eq!(plan.export_requests()[2].source_node_id(), root_layer_id);
        assert_eq!(
            plan.export_requests()[2].asset_kind(),
            GlaDocImageAssetKind::LeafSource {
                node_id: root_layer_id
            }
        );
    }

    #[test]
    fn plan_write_and_load_round_trip_preserves_tree_and_assets() {
        let mut doc = new_doc(BackendId::new(3), BackendId::new(7));
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

        let temp_dir = temp_directory("gla-doc-round-trip");
        let plan = doc
            .plan_directory_save(&temp_dir)
            .expect("save plan should build");
        let exported_images = vec![
            test_image(8, 6, 91),
            test_image(IMAGE_TILE_SIZE + 3, IMAGE_TILE_SIZE + 1, 29),
            test_image(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE, 11),
        ];
        plan.write_exported_images(&exported_images)
            .expect("exported images should write");

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
        assert_eq!(loaded.leaf_sources[0].image, exported_images[1]);
        assert_eq!(loaded.leaf_sources[1].node_id, preorder[3]);
        assert_eq!(loaded.leaf_sources[1].image, exported_images[2]);
        assert_eq!(loaded.thumbnail, exported_images[0]);

        std::fs::remove_dir_all(temp_dir).expect("temp directory should remove");
        assert_eq!(root_layer_id, root_layer_id);
    }

    #[test]
    fn write_exported_images_requires_exact_request_count() {
        let doc = new_doc(BackendId::new(3), BackendId::new(7));
        let temp_dir = temp_directory("gla-doc-export-count");
        let plan = doc
            .plan_directory_save(&temp_dir)
            .expect("save plan should build");

        let result = plan.write_exported_images(&[]);

        assert_eq!(
            result.err(),
            Some(GlaDocStorageError::ExportedImageCountMismatch {
                expected: 1,
                actual: 0,
            })
        );
    }

    fn new_doc(image_backend: BackendId, render_backend: BackendId) -> GlaDoc {
        GlaDoc::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE),
            image_backend,
            render_backend,
        )
        .expect("document should build")
    }

    fn test_image(width: u32, height: u32, seed: u8) -> gla_image::GlaStoredImage {
        let mut pixels = vec![0; (width * height * 4) as usize];
        for (index, value) in pixels.iter_mut().enumerate() {
            *value = seed.wrapping_add(index as u8);
        }
        gla_image::GlaStoredImage::new_rgba8(width, height, pixels)
            .expect("stored image should build")
    }

    fn temp_directory(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{unique}"))
    }
}
