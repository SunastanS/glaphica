use serde::{Deserialize, Serialize};

use glaphica_core::{BackendId, NodeId};
use images::layout::ImageLayout;
use images::{Image, ImageCreateError};

use crate::{
    BranchBlendMode, BranchConfig, Document, LeafBlendMode, LeafConfig, Metadata, SolidColorLayer,
    SpecialLayer, UiBranchNode, UiLayerNode, UiLayerTree, UiLeafContent, UiLeafNode, UiNodeMeta,
};

const STORAGE_VERSION: u32 = 1;

fn default_visible() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentStorageError {
    UnsupportedVersion {
        expected: u32,
        actual: u32,
    },
    RasterSizeMismatch {
        node_id: NodeId,
        expected_width: u32,
        expected_height: u32,
        actual_width: u32,
        actual_height: u32,
    },
    ImageCreate(ImageCreateError),
}

impl From<ImageCreateError> for DocumentStorageError {
    fn from(error: ImageCreateError) -> Self {
        Self::ImageCreate(error)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentStorageManifest {
    pub version: u32,
    pub name: String,
    pub canvas_width: u32,
    pub canvas_height: u32,
    pub root: StoredLayerNode,
    pub active_node_id: Option<u64>,
    pub next_node_id: u64,
    pub next_layer_label_index: u64,
    pub next_group_label_index: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RasterLayerAssetMetadata {
    pub node_id: u64,
    pub file_name: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterLayerExportRequest {
    pub node_id: NodeId,
    pub file_name: String,
    pub layout: ImageLayout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredLeafBlendMode {
    Normal,
    Multiply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum StoredBranchBlendMode {
    Base(StoredLeafBlendMode),
    Penetrate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredLayerNode {
    Branch {
        id: u64,
        label: String,
        #[serde(default = "default_visible")]
        visible: bool,
        opacity: f32,
        blend_mode: StoredBranchBlendMode,
        children: Vec<StoredLayerNode>,
    },
    RasterLayer {
        id: u64,
        label: String,
        #[serde(default = "default_visible")]
        visible: bool,
        opacity: f32,
        blend_mode: StoredLeafBlendMode,
        image: RasterLayerAssetMetadata,
    },
    SolidColorLayer {
        id: u64,
        label: String,
        #[serde(default = "default_visible")]
        visible: bool,
        opacity: f32,
        blend_mode: StoredLeafBlendMode,
        color: [f32; 4],
    },
}

impl Document {
    pub fn storage_manifest(&self) -> DocumentStorageManifest {
        DocumentStorageManifest {
            version: STORAGE_VERSION,
            name: self.metadata.name().to_string(),
            canvas_width: self.layout.size_x(),
            canvas_height: self.layout.size_y(),
            root: export_layer_node(&self.layer_tree.root),
            active_node_id: self.active_node.map(|node_id| node_id.0),
            next_node_id: self.next_node_id.0,
            next_layer_label_index: self.next_layer_label_index,
            next_group_label_index: self.next_group_label_index,
        }
    }

    pub fn raster_layer_export_requests(&self) -> Vec<RasterLayerExportRequest> {
        let mut requests = Vec::new();
        collect_raster_layer_export_requests(&self.layer_tree.root, &mut requests);
        requests
    }

    pub fn from_storage_manifest(
        manifest: DocumentStorageManifest,
        leaf_backend: BackendId,
        render_cache_backend: BackendId,
    ) -> Result<Self, DocumentStorageError> {
        if manifest.version != STORAGE_VERSION {
            return Err(DocumentStorageError::UnsupportedVersion {
                expected: STORAGE_VERSION,
                actual: manifest.version,
            });
        }

        let layout = ImageLayout::new(manifest.canvas_width, manifest.canvas_height);
        let root = import_layer_node(&manifest.root, layout, leaf_backend)?;

        Ok(Document {
            layer_tree: UiLayerTree { root },
            layout,
            metadata: Metadata::new(manifest.name),
            leaf_backend,
            render_cache_backend,
            next_node_id: NodeId(manifest.next_node_id),
            next_layer_label_index: manifest.next_layer_label_index,
            next_group_label_index: manifest.next_group_label_index,
            active_node: manifest.active_node_id.map(NodeId),
        })
    }
}

fn export_layer_node(node: &UiLayerNode) -> StoredLayerNode {
    match node {
        UiLayerNode::Branch(branch) => StoredLayerNode::Branch {
            id: branch.meta.id.0,
            label: branch.meta.label.clone(),
            visible: branch.meta.visible,
            opacity: branch.config.opacity,
            blend_mode: branch.config.blend_mode.into(),
            children: branch.children.iter().map(export_layer_node).collect(),
        },
        UiLayerNode::Leaf(leaf) => match &leaf.content {
            UiLeafContent::Raster { image } => {
                let node_id = leaf.meta.id;
                StoredLayerNode::RasterLayer {
                    id: node_id.0,
                    label: leaf.meta.label.clone(),
                    visible: leaf.meta.visible,
                    opacity: leaf.config.opacity,
                    blend_mode: leaf.config.blend_mode.into(),
                    image: RasterLayerAssetMetadata {
                        node_id: node_id.0,
                        file_name: raster_layer_file_name(node_id),
                        width: image.layout().size_x(),
                        height: image.layout().size_y(),
                    },
                }
            }
            UiLeafContent::Special(SpecialLayer::SolidColor(layer)) => {
                StoredLayerNode::SolidColorLayer {
                    id: leaf.meta.id.0,
                    label: leaf.meta.label.clone(),
                    visible: leaf.meta.visible,
                    opacity: leaf.config.opacity,
                    blend_mode: leaf.config.blend_mode.into(),
                    color: layer.color,
                }
            }
        },
    }
}

fn collect_raster_layer_export_requests(
    node: &UiLayerNode,
    output: &mut Vec<RasterLayerExportRequest>,
) {
    match node {
        UiLayerNode::Branch(branch) => {
            for child in &branch.children {
                collect_raster_layer_export_requests(child, output);
            }
        }
        UiLayerNode::Leaf(leaf) => {
            let UiLeafContent::Raster { image } = &leaf.content else {
                return;
            };
            output.push(RasterLayerExportRequest {
                node_id: leaf.meta.id,
                file_name: raster_layer_file_name(leaf.meta.id),
                layout: *image.layout(),
            });
        }
    }
}

fn import_layer_node(
    node: &StoredLayerNode,
    layout: ImageLayout,
    leaf_backend: BackendId,
) -> Result<UiLayerNode, DocumentStorageError> {
    match node {
        StoredLayerNode::Branch {
            id,
            label,
            visible,
            opacity,
            blend_mode,
            children,
        } => Ok(UiLayerNode::Branch(UiBranchNode {
            meta: UiNodeMeta {
                id: NodeId(*id),
                label: label.clone(),
                visible: *visible,
            },
            config: BranchConfig {
                opacity: *opacity,
                blend_mode: (*blend_mode).into(),
            },
            children: children
                .iter()
                .map(|child| import_layer_node(child, layout, leaf_backend))
                .collect::<Result<Vec<_>, _>>()?,
        })),
        StoredLayerNode::RasterLayer {
            id,
            label,
            visible,
            opacity,
            blend_mode,
            image,
        } => {
            if image.width != layout.size_x() || image.height != layout.size_y() {
                return Err(DocumentStorageError::RasterSizeMismatch {
                    node_id: NodeId(*id),
                    expected_width: layout.size_x(),
                    expected_height: layout.size_y(),
                    actual_width: image.width,
                    actual_height: image.height,
                });
            }
            Ok(UiLayerNode::Leaf(UiLeafNode {
                meta: UiNodeMeta {
                    id: NodeId(*id),
                    label: label.clone(),
                    visible: *visible,
                },
                config: LeafConfig {
                    opacity: *opacity,
                    blend_mode: (*blend_mode).into(),
                },
                content: UiLeafContent::Raster {
                    image: Image::new(layout, leaf_backend)?,
                },
            }))
        }
        StoredLayerNode::SolidColorLayer {
            id,
            label,
            visible,
            opacity,
            blend_mode,
            color,
        } => Ok(UiLayerNode::Leaf(UiLeafNode {
            meta: UiNodeMeta {
                id: NodeId(*id),
                label: label.clone(),
                visible: *visible,
            },
            config: LeafConfig {
                opacity: *opacity,
                blend_mode: (*blend_mode).into(),
            },
            content: UiLeafContent::Special(SpecialLayer::SolidColor(SolidColorLayer {
                color: *color,
            })),
        })),
    }
}

fn raster_layer_file_name(node_id: NodeId) -> String {
    format!("layers/{}.png", node_id.0)
}

impl From<LeafBlendMode> for StoredLeafBlendMode {
    fn from(value: LeafBlendMode) -> Self {
        match value {
            LeafBlendMode::Normal => Self::Normal,
            LeafBlendMode::Multiply => Self::Multiply,
        }
    }
}

impl From<StoredLeafBlendMode> for LeafBlendMode {
    fn from(value: StoredLeafBlendMode) -> Self {
        match value {
            StoredLeafBlendMode::Normal => Self::Normal,
            StoredLeafBlendMode::Multiply => Self::Multiply,
        }
    }
}

impl From<BranchBlendMode> for StoredBranchBlendMode {
    fn from(value: BranchBlendMode) -> Self {
        match value {
            BranchBlendMode::Base(mode) => Self::Base(mode.into()),
            BranchBlendMode::Penetrate => Self::Penetrate,
        }
    }
}

impl From<StoredBranchBlendMode> for BranchBlendMode {
    fn from(value: StoredBranchBlendMode) -> Self {
        match value {
            StoredBranchBlendMode::Base(mode) => Self::Base(mode.into()),
            StoredBranchBlendMode::Penetrate => Self::Penetrate,
        }
    }
}

#[cfg(test)]
mod tests {
    use glaphica_core::BackendId;

    use super::StoredLayerNode;
    use crate::{Document, NewLayerKind};
    use images::layout::ImageLayout;

    #[test]
    fn storage_manifest_round_trip_preserves_tree_shape() {
        let mut document = Document::new(
            "storage".to_string(),
            ImageLayout::new(128, 64),
            BackendId::new(1),
            BackendId::new(2),
        )
        .unwrap();
        document.create_group_above_active().unwrap();
        document
            .create_layer_above_active(NewLayerKind::SolidColor {
                color: [0.25, 0.5, 0.75, 1.0],
            })
            .unwrap();

        let manifest = document.storage_manifest();
        let restored = Document::from_storage_manifest(
            manifest.clone(),
            BackendId::new(9),
            BackendId::new(10),
        )
        .unwrap();

        assert_eq!(restored.metadata().name(), "storage");
        assert_eq!(restored.layout().size_x(), 128);
        assert_eq!(restored.layout().size_y(), 64);
        assert_eq!(restored.storage_manifest(), manifest);
    }

    #[test]
    fn raster_layer_export_requests_match_manifest_assets() {
        let document = Document::new(
            "storage".to_string(),
            ImageLayout::new(128, 64),
            BackendId::new(1),
            BackendId::new(2),
        )
        .unwrap();

        let manifest = document.storage_manifest();
        let requests = document.raster_layer_export_requests();

        let StoredLayerNode::Branch { children, .. } = manifest.root else {
            panic!("expected branch root");
        };
        let raster_assets = children
            .iter()
            .filter_map(|child| match child {
                StoredLayerNode::RasterLayer { image, .. } => Some(image.file_name.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(requests.len(), raster_assets.len());
        assert_eq!(
            requests
                .iter()
                .map(|request| request.file_name.clone())
                .collect::<Vec<_>>(),
            raster_assets
        );
    }
}
