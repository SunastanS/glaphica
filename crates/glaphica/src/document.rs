use std::collections::{BTreeMap, HashSet};
use std::convert::Infallible;
use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasLayout, AtlasTextureStore, NoAtlasTextures, TilePos};
use gla_color::{ChannelCount, ChannelType, GlaFormat, PremultipliedRgbaF32};
use gla_core::CanvasCoordF;
use gla_draw_on::{DrawOnInput, DrawOnLoweringError, DrawOnPass, DrawOnToolSpec};
use gla_image::IMAGE_TILE_SIZE;
use gla_ir::{
    DocImageUse, DocumentVersionId, DrawOnCommand, DrawOnToolKind, DrawSessionIR, GraphCommand,
    ImageId, ImageLayoutSpec, ImageRole, RegistryPatch, RegistryPatchOp,
};
use gla_renderer::{Pass, PresentTile, PresentTileParams, RenderBackend};
use gla_session::{DrawCommit, DrawHistory, DrawRecordId, DrawSession, FrameBudget, SessionError};
use gla_storage::{GlobalStorage, GlobalStorageError, GlobalTileError};
use tile_key::{NewAtlasError, TileReadRef, Tiles};

use crate::{
    AppView, BrushInput, BrushInputError, DocumentBlendMode, DocumentLayerTree,
    DocumentLayerTreeError, DocumentNodeId, DocumentNodeKind, ScriptDrawCommand, ScriptDrawSession,
};

pub const DEFAULT_CANVAS_WIDTH_PX: u32 = 1024;
pub const DEFAULT_CANVAS_HEIGHT_PX: u32 = 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReplaceCircleStrokeSample {
    pub center: CanvasCoordF,
    pub radius_px: f32,
    pub color: PremultipliedRgbaF32,
}

impl ReplaceCircleStrokeSample {
    pub fn new(center_x: f32, center_y: f32, radius_px: f32, color: PremultipliedRgbaF32) -> Self {
        Self {
            center: CanvasCoordF::new(center_x, center_y),
            radius_px,
            color,
        }
    }
}

#[derive(Debug)]
pub enum DocumentBrushInputError {
    BrushInput(BrushInputError),
    Session(SessionError),
}

pub struct DocumentWorkspace {
    storage: GlobalStorage,
    root: ImageId,
    format: GlaFormat,
    layout: ImageLayoutSpec,
    layers: DocumentLayerTree,
    layer_composite_image: Option<ImageId>,
    layer_composite_valid: bool,
    layer_composite_tile_indices: Vec<u32>,
    stroke_preview_image: Option<ImageId>,
    stroke_preview_composite_image: Option<ImageId>,
    stroke_preview_tile_indices: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentRootTileRead {
    pub tile_index: u32,
    pub src: TilePos,
    pub source_width: u32,
    pub source_height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LayerRenderInput {
    image: ImageId,
    opacity: f32,
    blend_mode: gla_color::BlendMode,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LayerRenderTileInput {
    src: TilePos,
    opacity: f32,
    blend_mode: gla_color::BlendMode,
}

impl DocumentWorkspace {
    pub fn default_blank() -> Result<Self, DocumentWorkspaceBuildError<Infallible>> {
        Self::blank(DEFAULT_CANVAS_WIDTH_PX, DEFAULT_CANVAS_HEIGHT_PX)
    }

    pub fn blank(
        width_px: u32,
        height_px: u32,
    ) -> Result<Self, DocumentWorkspaceBuildError<Infallible>> {
        let mut textures = NoAtlasTextures;
        Self::blank_with_textures(width_px, height_px, &mut textures)
    }

    pub fn blank_with_textures<S>(
        width_px: u32,
        height_px: u32,
        textures: &mut S,
    ) -> Result<Self, DocumentWorkspaceBuildError<S::Error>>
    where
        S: AtlasTextureStore,
    {
        let format = default_canvas_format();
        Self::primitive_root_with_textures(ImageId::new(1), width_px, height_px, format, textures)
    }

    pub(crate) fn primitive_root_with_textures<S>(
        root: ImageId,
        width_px: u32,
        height_px: u32,
        format: GlaFormat,
        textures: &mut S,
    ) -> Result<Self, DocumentWorkspaceBuildError<S::Error>>
    where
        S: AtlasTextureStore,
    {
        Self::primitive_root_with_textures_min_slots(root, width_px, height_px, format, 0, textures)
    }

    pub(crate) fn primitive_root_with_textures_min_slots<S>(
        root: ImageId,
        width_px: u32,
        height_px: u32,
        format: GlaFormat,
        min_slots: u64,
        textures: &mut S,
    ) -> Result<Self, DocumentWorkspaceBuildError<S::Error>>
    where
        S: AtlasTextureStore,
    {
        let layout = ImageLayoutSpec::new(width_px, height_px);
        let mut tiles = Tiles::new();
        tiles
            .new_atlas(initial_atlas_layout(layout, min_slots), format, textures)
            .map_err(DocumentWorkspaceBuildError::Atlas)?;

        let mut storage = GlobalStorage::new(tiles);
        storage
            .apply_registry_patch(RegistryPatch::new(vec![
                RegistryPatchOp::NewImage {
                    id: root,
                    format,
                    layout,
                    role: ImageRole::Primitive,
                },
                RegistryPatchOp::SetRoot(root),
            ]))
            .map_err(DocumentWorkspaceBuildError::Registry)?;

        Ok(Self {
            storage,
            root,
            format,
            layout,
            layers: DocumentLayerTree::new(root),
            layer_composite_image: None,
            layer_composite_valid: false,
            layer_composite_tile_indices: Vec::new(),
            stroke_preview_image: None,
            stroke_preview_composite_image: None,
            stroke_preview_tile_indices: Vec::new(),
        })
    }

    pub fn white_with_textures<B>(
        width_px: u32,
        height_px: u32,
        backend: &mut B,
    ) -> Result<Self, DocumentWorkspaceInitError<<B as AtlasTextureStore>::Error>>
    where
        B: AtlasTextureStore + RenderBackend,
    {
        let mut workspace = Self::blank_with_textures(width_px, height_px, backend)
            .map_err(DocumentWorkspaceInitError::Build)?;
        workspace
            .initialize_root_to_color(backend, PremultipliedRgbaF32::new(1.0, 1.0, 1.0, 1.0))
            .map_err(DocumentWorkspaceInitError::InitialPaint)?;
        Ok(workspace)
    }

    pub fn storage(&self) -> &GlobalStorage {
        &self.storage
    }

    pub fn storage_mut(&mut self) -> &mut GlobalStorage {
        &mut self.storage
    }

    pub fn ensure_draw_on_tool_atlases<S>(
        &mut self,
        tools: impl IntoIterator<Item = DrawOnToolKind>,
        textures: &mut S,
    ) -> Result<(), NewAtlasError<S::Error>>
    where
        S: AtlasTextureStore,
    {
        let mut formats = HashSet::new();
        for format in tools.into_iter().map(DrawOnToolKind::target_format) {
            if formats.insert(format) && self.storage.tiles().atlas_for_format(format).is_none() {
                self.storage.tiles_mut().new_atlas(
                    initial_atlas_layout(self.layout, 0),
                    format,
                    textures,
                )?;
            }
        }
        Ok(())
    }

    pub fn root(&self) -> ImageId {
        self.root
    }

    pub fn version(&self) -> DocumentVersionId {
        self.storage.version()
    }

    pub fn format(&self) -> GlaFormat {
        self.format
    }

    pub fn layout(&self) -> ImageLayoutSpec {
        self.layout
    }

    pub fn canvas_size_px(&self) -> (u32, u32) {
        (self.layout.width_px, self.layout.height_px)
    }

    pub fn layer_tree(&self) -> &DocumentLayerTree {
        &self.layers
    }

    pub fn layer_composite_needs_render(&self) -> bool {
        self.layer_composite_image.is_some()
            && !self.layer_composite_valid
            && self
                .layers
                .child_ids(self.layers.root_id())
                .is_ok_and(|children| !children.is_empty())
    }

    pub fn append_layer(
        &mut self,
        parent_id: DocumentNodeId,
    ) -> Result<DocumentNodeId, DocumentWorkspaceLayerError> {
        self.layers.child_ids(parent_id)?;
        let image = self.register_layer_image(ImageRole::Primitive)?;
        let node = self.layers.append_layer(parent_id, image)?;
        self.layers.set_active_node(node)?;
        self.invalidate_layer_composite();
        Ok(node)
    }

    pub fn append_group(
        &mut self,
        parent_id: DocumentNodeId,
    ) -> Result<DocumentNodeId, DocumentWorkspaceLayerError> {
        self.layers.child_ids(parent_id)?;
        let image = self.register_layer_image(ImageRole::Derived(GraphCommand::new(Vec::new())))?;
        let node = self.layers.append_group(parent_id, image)?;
        self.layers.set_active_node(node)?;
        self.invalidate_layer_composite();
        Ok(node)
    }

    pub fn insert_layer_above_active(
        &mut self,
    ) -> Result<DocumentNodeId, DocumentWorkspaceLayerError> {
        let (parent_id, index) = self.active_insert_position()?;
        let image = self.register_layer_image(ImageRole::Primitive)?;
        let node = self.layers.insert_layer(parent_id, index, image)?;
        self.layers.set_active_node(node)?;
        self.invalidate_layer_composite();
        Ok(node)
    }

    pub fn insert_group_above_active(
        &mut self,
    ) -> Result<DocumentNodeId, DocumentWorkspaceLayerError> {
        let (parent_id, index) = self.active_insert_position()?;
        let image = self.register_layer_image(ImageRole::Derived(GraphCommand::new(Vec::new())))?;
        let node = self.layers.insert_group(parent_id, index, image)?;
        self.layers.set_active_node(node)?;
        self.invalidate_layer_composite();
        Ok(node)
    }

    pub fn set_active_node(
        &mut self,
        node_id: DocumentNodeId,
    ) -> Result<(), DocumentWorkspaceLayerError> {
        self.layers.set_active_node(node_id)?;
        Ok(())
    }

    pub fn set_node_opacity(
        &mut self,
        node_id: DocumentNodeId,
        opacity: f32,
    ) -> Result<(), DocumentWorkspaceLayerError> {
        self.layers.set_opacity(node_id, opacity)?;
        self.invalidate_layer_composite();
        Ok(())
    }

    pub fn set_node_blend_mode(
        &mut self,
        node_id: DocumentNodeId,
        blend_mode: DocumentBlendMode,
    ) -> Result<(), DocumentWorkspaceLayerError> {
        self.layers.set_blend_mode(node_id, blend_mode)?;
        self.invalidate_layer_composite();
        Ok(())
    }

    pub fn move_node(
        &mut self,
        node_id: DocumentNodeId,
        new_parent_id: DocumentNodeId,
        new_index: usize,
    ) -> Result<(), DocumentWorkspaceLayerError> {
        self.layers.move_node(node_id, new_parent_id, new_index)?;
        self.invalidate_layer_composite();
        Ok(())
    }

    pub fn delete_node(
        &mut self,
        node_id: DocumentNodeId,
    ) -> Result<(), DocumentWorkspaceLayerError> {
        if node_id == self.layers.root_id() {
            self.layers.delete_node(node_id)?;
            return Ok(());
        }
        let mut subtree = Vec::new();
        self.layers
            .collect_subtree_preorder(node_id, &mut subtree)?;
        let image_ids = subtree
            .iter()
            .map(|node_id| self.layers.node(*node_id).map(|node| node.image()))
            .collect::<Result<HashSet<_>, _>>()?;
        self.storage.delete_images(&image_ids)?;
        self.layers.delete_node(node_id)?;
        self.invalidate_layer_composite();
        Ok(())
    }

    pub fn delete_active_node(&mut self) -> Result<bool, DocumentWorkspaceLayerError> {
        let active = self.layers.active_node_id();
        if active == self.layers.root_id() {
            return Ok(false);
        }
        self.delete_node(active)?;
        Ok(true)
    }

    pub(crate) fn restore_layer_tree_from_export(
        &mut self,
        layers: DocumentLayerTree,
    ) -> Result<(), DocumentWorkspaceLayerError> {
        let root = layers
            .node(layers.root_id())
            .map_err(DocumentWorkspaceLayerError::Tree)?;
        if root.kind() != DocumentNodeKind::Root || root.image() != self.root {
            return Err(DocumentWorkspaceLayerError::InvalidLayerTreeRootImage {
                expected: self.root,
                actual: root.image(),
            });
        }
        layers.node(layers.active_node_id())?;

        let mut node_ids = Vec::new();
        layers.collect_subtree_preorder(layers.root_id(), &mut node_ids)?;
        let mut images = HashSet::new();
        let mut max_image_id = self.max_image_id();
        let mut ops = Vec::new();
        for node_id in node_ids {
            let node = layers.node(node_id)?;
            if !images.insert(node.image()) {
                return Err(DocumentWorkspaceLayerError::DuplicateLayerImage { id: node.image() });
            }
            max_image_id = max_image_id.max(node.image().value());
            if node_id == layers.root_id() {
                continue;
            }
            let role = match node.kind() {
                DocumentNodeKind::Root => continue,
                DocumentNodeKind::Group => ImageRole::Derived(GraphCommand::new(Vec::new())),
                DocumentNodeKind::Layer => ImageRole::Primitive,
            };
            ops.push(RegistryPatchOp::NewImage {
                id: node.image(),
                format: self.format,
                layout: self.layout,
                role,
            });
        }

        let has_layer_children = layers
            .child_ids(layers.root_id())
            .is_ok_and(|children| !children.is_empty());
        let composite = if has_layer_children {
            let id = max_image_id
                .checked_add(1)
                .ok_or(DocumentWorkspaceLayerError::ImageIdExhausted)?;
            let id = ImageId::new(id);
            ops.push(RegistryPatchOp::NewImage {
                id,
                format: self.format,
                layout: self.layout,
                role: ImageRole::Derived(GraphCommand::new(Vec::new())),
            });
            Some(id)
        } else {
            None
        };

        if !ops.is_empty() {
            self.storage.apply_registry_patch(RegistryPatch::new(ops))?;
        }
        self.layers = layers;
        self.layer_composite_image = composite;
        self.layer_composite_valid = false;
        self.layer_composite_tile_indices.clear();
        self.clear_stroke_preview_state();
        Ok(())
    }

    pub fn render_layer_tree_full<B>(
        &mut self,
        backend: &mut B,
    ) -> Result<Vec<u32>, DocumentLayerRenderError<B::Error>>
    where
        B: RenderBackend,
    {
        if !self.layer_composite_needs_render() {
            return Ok(Vec::new());
        }
        let composite = self
            .layer_composite_image
            .expect("layer composite image exists when render is needed");
        let layout = self
            .root_layout()
            .map_err(DocumentLayerRenderError::Present)?;
        let tile_count = layout.tile_count();
        let inputs = self
            .layer_render_inputs()
            .map_err(DocumentLayerRenderError::Layer)?;
        let mut passes = Vec::new();
        let mut dirty = Vec::with_capacity(tile_count as usize);

        for tile_index in 0..tile_count {
            let mut layer_tile_inputs = Vec::new();
            for input in &inputs {
                let TileReadRef::Physical(src) = self
                    .storage
                    .read_global_ref(input.image, tile_index)
                    .map_err(DocumentLayerRenderError::Tile)?
                else {
                    continue;
                };
                layer_tile_inputs.push(LayerRenderTileInput {
                    src,
                    opacity: input.opacity,
                    blend_mode: input.blend_mode,
                });
            }
            if layer_tile_inputs.is_empty() {
                continue;
            }

            let dst = self
                .storage
                .write_global_cache_pos(composite, tile_index)
                .map_err(DocumentLayerRenderError::Tile)?;
            passes.push(Pass::Clear { dst });
            if let TileReadRef::Physical(src) = self
                .storage
                .read_global_ref(self.root, tile_index)
                .map_err(DocumentLayerRenderError::Tile)?
            {
                passes.push(Pass::Copy { src, dst });
            }

            for input in &layer_tile_inputs {
                passes.push(Pass::RenderTo {
                    src: input.src,
                    dst,
                    blend_mode: input.blend_mode,
                    opacity: input.opacity,
                });
            }
            passes.push(Pass::FixGutter { dst });
            dirty.push(tile_index);
        }

        if !passes.is_empty() {
            backend
                .submit(&passes)
                .map_err(DocumentLayerRenderError::Render)?;
        }
        self.layer_composite_tile_indices = dirty.clone();
        self.layer_composite_valid = true;
        Ok(dirty)
    }

    pub fn render_stroke_preview<B>(
        &mut self,
        backend: &mut B,
        samples: impl IntoIterator<Item = ReplaceCircleStrokeSample>,
    ) -> Result<Vec<u32>, DocumentStrokePreviewError<B::Error>>
    where
        B: RenderBackend,
    {
        let samples = samples.into_iter().collect::<Vec<_>>();
        if samples.is_empty() {
            return self.clear_stroke_preview(backend);
        }

        let preview = self.ensure_stroke_preview_image()?;
        let composite = self.ensure_stroke_preview_composite_image()?;
        let layout = self
            .image_layout(preview)
            .map_err(DocumentStrokePreviewError::Present)?;
        let previous_tiles = self.stroke_preview_tile_indices.clone();
        let mut next_tiles = Vec::new();
        let mut draw_invocations = Vec::new();

        for sample in samples {
            let radius_px = sample.radius_px.max(0.0);
            let input = DrawOnInput::replace_circle_4d(
                sample.center.x,
                sample.center.y,
                radius_px,
                radius_px,
                sample.color,
            );
            let passes = gla_draw_on::lower_input(
                preview,
                DrawOnToolKind::ReplaceCircle4D,
                layout,
                input,
                |_image, tile_index| {
                    let tile_index = tile_index.value();
                    next_tiles.push(tile_index);
                    Ok::<u32, Infallible>(tile_index)
                },
            )
            .map_err(DocumentStrokePreviewError::DrawOnLowering)?;
            draw_invocations.extend(passes.into_iter().map(DrawOnPass::invocation));
        }

        sort_dedup(&mut next_tiles);
        let dirty_tiles = union_tile_indices(&previous_tiles, &next_tiles);
        if dirty_tiles.is_empty() {
            return Ok(Vec::new());
        }

        let mut passes = Vec::new();
        let mut preview_tile_positions = BTreeMap::new();
        for tile_index in &dirty_tiles {
            let dst = self
                .storage
                .write_global_cache_pos(preview, *tile_index)
                .map_err(DocumentStrokePreviewError::Tile)?;
            passes.push(Pass::Clear { dst });
            preview_tile_positions.insert(*tile_index, dst);
        }

        let mut touched_preview_tiles = Vec::new();
        for invocation in draw_invocations {
            let tile_index = invocation.target();
            let Some(&dst) = preview_tile_positions.get(&tile_index) else {
                continue;
            };
            passes.push(Pass::DrawOn(invocation.map_target(|_| dst)));
            touched_preview_tiles.push(dst);
        }
        sort_dedup_tile_pos(&mut touched_preview_tiles);
        for dst in touched_preview_tiles {
            passes.push(Pass::FixGutter { dst });
        }

        let tile_count_x = layout.tile_count_x();
        for tile_index in &dirty_tiles {
            let dst = self
                .storage
                .write_global_cache_pos(composite, *tile_index)
                .map_err(DocumentStrokePreviewError::Tile)?;
            passes.push(Pass::Clear { dst });
            if let Some(base_tile) = self
                .physical_tile_for_index(
                    self.base_display_image_for_tile_index(*tile_index),
                    tile_count_x,
                    *tile_index,
                )
                .map_err(DocumentStrokePreviewError::Present)?
            {
                passes.push(Pass::Copy {
                    src: base_tile.src,
                    dst,
                });
            }
            if let Some(preview_tile) = self
                .physical_tile_for_index(preview, tile_count_x, *tile_index)
                .map_err(DocumentStrokePreviewError::Present)?
            {
                passes.push(Pass::RenderTo {
                    src: preview_tile.src,
                    dst,
                    blend_mode: gla_color::BlendMode::Normal,
                    opacity: 1.0,
                });
            }
            passes.push(Pass::FixGutter { dst });
        }

        backend
            .submit(&passes)
            .map_err(DocumentStrokePreviewError::Render)?;
        self.stroke_preview_tile_indices = next_tiles;
        Ok(dirty_tiles)
    }

    pub fn clear_stroke_preview<B>(
        &mut self,
        backend: &mut B,
    ) -> Result<Vec<u32>, DocumentStrokePreviewError<B::Error>>
    where
        B: RenderBackend,
    {
        let Some(preview) = self.stroke_preview_image else {
            self.stroke_preview_tile_indices.clear();
            return Ok(Vec::new());
        };
        let Some(composite) = self.stroke_preview_composite_image else {
            self.stroke_preview_tile_indices.clear();
            return Ok(Vec::new());
        };
        let dirty_tiles = std::mem::take(&mut self.stroke_preview_tile_indices);
        if dirty_tiles.is_empty() {
            return Ok(Vec::new());
        }

        let layout = self
            .image_layout(preview)
            .map_err(DocumentStrokePreviewError::Present)?;
        let tile_count_x = layout.tile_count_x();
        let mut passes = Vec::new();
        for tile_index in &dirty_tiles {
            let preview_dst = self
                .storage
                .write_global_cache_pos(preview, *tile_index)
                .map_err(DocumentStrokePreviewError::Tile)?;
            passes.push(Pass::Clear { dst: preview_dst });
            passes.push(Pass::FixGutter { dst: preview_dst });

            let composite_dst = self
                .storage
                .write_global_cache_pos(composite, *tile_index)
                .map_err(DocumentStrokePreviewError::Tile)?;
            passes.push(Pass::Clear { dst: composite_dst });
            if let Some(base_tile) = self
                .physical_tile_for_index(
                    self.base_display_image_for_tile_index(*tile_index),
                    tile_count_x,
                    *tile_index,
                )
                .map_err(DocumentStrokePreviewError::Present)?
            {
                passes.push(Pass::Copy {
                    src: base_tile.src,
                    dst: composite_dst,
                });
            }
            passes.push(Pass::FixGutter { dst: composite_dst });
        }

        backend
            .submit(&passes)
            .map_err(DocumentStrokePreviewError::Render)?;
        Ok(dirty_tiles)
    }

    pub fn root_reader_ir(&self) -> DrawSessionIR {
        DrawSessionIR {
            expected_document_version: self.version(),
            doc_images: vec![DocImageUse::read(self.root)],
            session_images: Vec::new(),
            draw_on: Vec::new(),
            derive: Vec::new(),
        }
    }

    pub fn root_replace_circle_ir(&self) -> DrawSessionIR {
        self.replace_circle_ir(self.root)
    }

    pub fn active_paint_image(&self) -> Option<ImageId> {
        let node = self.layers.node(self.layers.active_node_id()).ok()?;
        match node.kind() {
            DocumentNodeKind::Root => Some(self.root),
            DocumentNodeKind::Group => None,
            DocumentNodeKind::Layer => Some(node.image()),
        }
    }

    pub fn active_replace_circle_ir(&self) -> Option<DrawSessionIR> {
        self.active_paint_image()
            .map(|target| self.replace_circle_ir(target))
    }

    fn replace_circle_ir(&self, target: ImageId) -> DrawSessionIR {
        DrawSessionIR {
            expected_document_version: self.version(),
            doc_images: vec![DocImageUse::read_write(target)],
            session_images: Vec::new(),
            draw_on: vec![DrawOnCommand::with_tool(
                target,
                DrawOnToolKind::ReplaceCircle4D,
            )],
            derive: Vec::new(),
        }
    }

    pub fn begin_session(&mut self, ir: &DrawSessionIR) -> Result<DrawSession<'_>, SessionError> {
        DrawSession::begin(ir, &mut self.storage)
    }

    pub fn apply_registry_patch(
        &mut self,
        patch: RegistryPatch,
    ) -> Result<DocumentVersionId, GlobalStorageError> {
        self.storage.apply_registry_patch(patch)?;
        self.sync_root_metadata();
        Ok(self.version())
    }

    fn sync_root_metadata(&mut self) {
        let Some(root) = self.storage.root() else {
            return;
        };
        let image = self
            .storage
            .image(root)
            .expect("storage root should reference an existing image");
        let layout = image.layout();
        self.root = root;
        self.format = image.format();
        self.layout = ImageLayoutSpec::new(layout.width_px(), layout.height_px());
        self.layer_composite_image = None;
        self.layer_composite_valid = false;
        self.layer_composite_tile_indices.clear();
        self.clear_stroke_preview_state();
        if self
            .layers
            .node(self.layers.root_id())
            .map(|node| node.image())
            .ok()
            != Some(root)
        {
            self.layers = DocumentLayerTree::new(root);
        }
    }

    fn clear_stroke_preview_state(&mut self) {
        self.stroke_preview_image = None;
        self.stroke_preview_composite_image = None;
        self.stroke_preview_tile_indices.clear();
    }

    fn register_layer_image(
        &mut self,
        role: ImageRole,
    ) -> Result<ImageId, DocumentWorkspaceLayerError> {
        let max_id = self.max_image_id();
        let mut next = max_id
            .checked_add(1)
            .ok_or(DocumentWorkspaceLayerError::ImageIdExhausted)?;
        let mut ops = Vec::new();
        let new_composite = if self.layer_composite_image.is_none() {
            let id = ImageId::new(next);
            next = next
                .checked_add(1)
                .ok_or(DocumentWorkspaceLayerError::ImageIdExhausted)?;
            ops.push(RegistryPatchOp::NewImage {
                id,
                format: self.format,
                layout: self.layout,
                role: ImageRole::Derived(GraphCommand::new(Vec::new())),
            });
            Some(id)
        } else {
            None
        };
        let id = ImageId::new(next);
        ops.push(RegistryPatchOp::NewImage {
            id,
            format: self.format,
            layout: self.layout,
            role,
        });
        self.storage.apply_registry_patch(RegistryPatch::new(ops))?;
        if let Some(composite) = new_composite {
            self.layer_composite_image = Some(composite);
        }
        Ok(id)
    }

    fn register_hidden_cache_image(&mut self) -> Result<ImageId, DocumentWorkspaceLayerError> {
        let id = self.next_image_id()?;
        self.storage.insert_unversioned_cache_image(
            id,
            self.format,
            self.layout,
            GraphCommand::new(Vec::new()),
        )?;
        Ok(id)
    }

    fn ensure_stroke_preview_image(&mut self) -> Result<ImageId, DocumentWorkspaceLayerError> {
        if let Some(id) = self.stroke_preview_image {
            return Ok(id);
        }
        let id = self.register_hidden_cache_image()?;
        self.stroke_preview_image = Some(id);
        Ok(id)
    }

    fn ensure_stroke_preview_composite_image(
        &mut self,
    ) -> Result<ImageId, DocumentWorkspaceLayerError> {
        if let Some(id) = self.stroke_preview_composite_image {
            return Ok(id);
        }
        let id = self.register_hidden_cache_image()?;
        self.stroke_preview_composite_image = Some(id);
        Ok(id)
    }

    fn next_image_id(&self) -> Result<ImageId, DocumentWorkspaceLayerError> {
        self.max_image_id()
            .checked_add(1)
            .map(ImageId::new)
            .ok_or(DocumentWorkspaceLayerError::ImageIdExhausted)
    }

    fn max_image_id(&self) -> u64 {
        self.storage
            .images()
            .keys()
            .map(|id| id.value())
            .max()
            .unwrap_or(0)
    }

    fn active_insert_position(
        &self,
    ) -> Result<(DocumentNodeId, usize), DocumentWorkspaceLayerError> {
        let active = self.layers.active_node_id();
        let active_node = self.layers.node(active)?;
        match active_node.parent() {
            Some(parent_id) => {
                let index = self.layers.child_index(parent_id, active)?;
                Ok((parent_id, index + 1))
            }
            None => {
                let root = self.layers.root_id();
                Ok((root, self.layers.child_ids(root)?.len()))
            }
        }
    }

    fn invalidate_layer_composite(&mut self) {
        self.layer_composite_valid = false;
    }

    fn base_display_root(&self) -> ImageId {
        self.root
    }

    fn base_display_image_for_tile_index(&self, tile_index: u32) -> ImageId {
        if self.layer_composite_valid
            && self
                .layer_composite_tile_indices
                .binary_search(&tile_index)
                .is_ok()
        {
            if let Some(image) = self.layer_composite_image {
                return image;
            }
        }
        self.base_display_root()
    }

    fn display_image_for_tile_index(&self, tile_index: u32) -> ImageId {
        if self
            .stroke_preview_tile_indices
            .binary_search(&tile_index)
            .is_ok()
        {
            if let Some(image) = self.stroke_preview_composite_image {
                return image;
            }
        }
        self.base_display_image_for_tile_index(tile_index)
    }

    fn layer_render_inputs(&self) -> Result<Vec<LayerRenderInput>, DocumentLayerTreeError> {
        let mut inputs = Vec::new();
        self.collect_layer_render_inputs(self.layers.root_id(), 1.0, &mut inputs)?;
        Ok(inputs)
    }

    fn collect_layer_render_inputs(
        &self,
        node_id: DocumentNodeId,
        parent_opacity: f32,
        inputs: &mut Vec<LayerRenderInput>,
    ) -> Result<(), DocumentLayerTreeError> {
        let node = self.layers.node(node_id)?;
        let opacity = parent_opacity * node.opacity();
        match node.kind() {
            DocumentNodeKind::Root | DocumentNodeKind::Group => {
                for child in node.children().unwrap_or_default() {
                    self.collect_layer_render_inputs(*child, opacity, inputs)?;
                }
            }
            DocumentNodeKind::Layer => inputs.push(LayerRenderInput {
                image: node.image(),
                opacity,
                blend_mode: node
                    .blend_mode()
                    .as_renderer_blend_mode()
                    .expect("all document blend modes must map to renderer blend modes"),
            }),
        }
        Ok(())
    }

    fn initialize_root_to_color<B>(
        &mut self,
        backend: &mut B,
        color: PremultipliedRgbaF32,
    ) -> Result<(), SessionError>
    where
        B: RenderBackend,
    {
        let center_x = self.layout.width_px as f32 * 0.5;
        let center_y = self.layout.height_px as f32 * 0.5;
        let radius_px = (self.layout.width_px as f32).hypot(self.layout.height_px as f32);
        let root = self.root;
        let ir = self.root_replace_circle_ir();
        let mut session = self.begin_session(&ir)?;
        {
            let mut frame = session.begin_frame();
            frame.draw_on(
                root,
                DrawOnInput::replace_circle_4d(
                    center_x,
                    center_y,
                    radius_px.max(1.0),
                    radius_px.max(1.0),
                    color,
                ),
            )?;
            frame.flush(backend)?;
        }
        if session.commit_discarding_undo()?.is_some() {
            self.invalidate_layer_composite();
        }
        Ok(())
    }

    pub fn replace_circle_on_root<B>(
        &mut self,
        history: &mut DrawHistory,
        backend: &mut B,
        center_x: f32,
        center_y: f32,
        radius_px: f32,
        color: PremultipliedRgbaF32,
    ) -> Result<Option<DrawCommit>, SessionError>
    where
        B: RenderBackend,
    {
        self.replace_circle_stroke_on_root(
            history,
            backend,
            [ReplaceCircleStrokeSample::new(
                center_x, center_y, radius_px, color,
            )],
        )
    }

    pub fn replace_circle_brush_input_on_root<B>(
        &mut self,
        history: &mut DrawHistory,
        backend: &mut B,
        brush_input: &BrushInput,
    ) -> Result<Option<DrawCommit>, DocumentBrushInputError>
    where
        B: RenderBackend,
    {
        self.replace_circle_brush_input_on_root_with_frame_budget(
            history,
            backend,
            brush_input,
            u32::MAX,
        )
    }

    pub fn replace_circle_brush_input_on_root_with_frame_budget<B>(
        &mut self,
        history: &mut DrawHistory,
        backend: &mut B,
        brush_input: &BrushInput,
        max_dabs_per_frame: u32,
    ) -> Result<Option<DrawCommit>, DocumentBrushInputError>
    where
        B: RenderBackend,
    {
        let samples = brush_input.replace_circle_samples()?;
        self.replace_circle_stroke_on_root_with_frame_budget(
            history,
            backend,
            samples,
            max_dabs_per_frame,
        )
        .map_err(DocumentBrushInputError::Session)
    }

    pub fn replace_circle_stroke_on_root<B>(
        &mut self,
        history: &mut DrawHistory,
        backend: &mut B,
        samples: impl IntoIterator<Item = ReplaceCircleStrokeSample>,
    ) -> Result<Option<DrawCommit>, SessionError>
    where
        B: RenderBackend,
    {
        self.replace_circle_stroke_on_root_with_frame_budget(history, backend, samples, u32::MAX)
    }

    pub fn replace_circle_stroke_on_root_with_frame_budget<B>(
        &mut self,
        history: &mut DrawHistory,
        backend: &mut B,
        samples: impl IntoIterator<Item = ReplaceCircleStrokeSample>,
        max_dabs_per_frame: u32,
    ) -> Result<Option<DrawCommit>, SessionError>
    where
        B: RenderBackend,
    {
        let root = self.root;
        let ir = self.root_replace_circle_ir();
        let mut session = self.begin_session(&ir)?;
        let mut samples = samples.into_iter();
        let mut next_sample = samples.next();
        while next_sample.is_some() {
            let mut budget = FrameBudget::new(max_dabs_per_frame.max(1));
            let mut frame = session.begin_frame();
            while let Some(sample) = next_sample.take() {
                if !budget.try_accept_dab() {
                    next_sample = Some(sample);
                    break;
                }
                let radius_px = sample.radius_px.max(0.0);
                frame.draw_on(
                    root,
                    DrawOnInput::replace_circle_4d(
                        sample.center.x,
                        sample.center.y,
                        radius_px,
                        radius_px,
                        sample.color,
                    ),
                )?;
                next_sample = samples.next();
            }
            frame.flush(backend)?;
        }
        let commit = session.commit(history)?;
        if commit.is_some() {
            self.invalidate_layer_composite();
        }
        Ok(commit)
    }

    pub fn replace_circle_brush_input_on_active_paint_target<B>(
        &mut self,
        history: &mut DrawHistory,
        backend: &mut B,
        brush_input: &BrushInput,
    ) -> Result<Option<DrawCommit>, DocumentBrushInputError>
    where
        B: RenderBackend,
    {
        self.replace_circle_brush_input_on_active_paint_target_with_frame_budget(
            history,
            backend,
            brush_input,
            u32::MAX,
        )
    }

    pub fn replace_circle_brush_input_on_active_paint_target_with_frame_budget<B>(
        &mut self,
        history: &mut DrawHistory,
        backend: &mut B,
        brush_input: &BrushInput,
        max_dabs_per_frame: u32,
    ) -> Result<Option<DrawCommit>, DocumentBrushInputError>
    where
        B: RenderBackend,
    {
        let samples = brush_input.replace_circle_samples()?;
        self.replace_circle_stroke_on_active_paint_target_with_frame_budget(
            history,
            backend,
            samples,
            max_dabs_per_frame,
        )
        .map_err(DocumentBrushInputError::Session)
    }

    pub fn replace_circle_stroke_on_active_paint_target<B>(
        &mut self,
        history: &mut DrawHistory,
        backend: &mut B,
        samples: impl IntoIterator<Item = ReplaceCircleStrokeSample>,
    ) -> Result<Option<DrawCommit>, SessionError>
    where
        B: RenderBackend,
    {
        self.replace_circle_stroke_on_active_paint_target_with_frame_budget(
            history,
            backend,
            samples,
            u32::MAX,
        )
    }

    pub fn replace_circle_stroke_on_active_paint_target_with_frame_budget<B>(
        &mut self,
        history: &mut DrawHistory,
        backend: &mut B,
        samples: impl IntoIterator<Item = ReplaceCircleStrokeSample>,
        max_dabs_per_frame: u32,
    ) -> Result<Option<DrawCommit>, SessionError>
    where
        B: RenderBackend,
    {
        let Some(target) = self.active_paint_image() else {
            return Ok(None);
        };
        let ir = self.replace_circle_ir(target);
        let mut session = self.begin_session(&ir)?;
        let mut samples = samples.into_iter();
        let mut next_sample = samples.next();
        while next_sample.is_some() {
            let mut budget = FrameBudget::new(max_dabs_per_frame.max(1));
            let mut frame = session.begin_frame();
            while let Some(sample) = next_sample.take() {
                if !budget.try_accept_dab() {
                    next_sample = Some(sample);
                    break;
                }
                let radius_px = sample.radius_px.max(0.0);
                frame.draw_on(
                    target,
                    DrawOnInput::replace_circle_4d(
                        sample.center.x,
                        sample.center.y,
                        radius_px,
                        radius_px,
                        sample.color,
                    ),
                )?;
                next_sample = samples.next();
            }
            frame.flush(backend)?;
        }
        let commit = session.commit(history)?;
        if commit.is_some() {
            self.invalidate_layer_composite();
        }
        Ok(commit)
    }

    pub fn run_script_draw_session<B>(
        &mut self,
        history: &mut DrawHistory,
        backend: &mut B,
        request: &ScriptDrawSession,
    ) -> Result<Option<DrawCommit>, SessionError>
    where
        B: RenderBackend,
    {
        let mut session = self.begin_session(&request.ir)?;
        for frame_request in &request.frames {
            let mut frame = session.begin_frame();
            for command in &frame_request.commands {
                match *command {
                    ScriptDrawCommand::DrawOn { target, input } => {
                        frame.draw_on(target, input)?;
                    }
                    ScriptDrawCommand::DrawDab { shown_image, input } => {
                        frame.draw_dab(shown_image, input)?;
                    }
                }
            }
            frame.flush(backend)?;
        }
        let commit = session.commit(history)?;
        if commit.is_some() {
            self.invalidate_layer_composite();
        }
        Ok(commit)
    }

    pub fn apply_draw_record<B>(
        &mut self,
        history: &mut DrawHistory,
        backend: &mut B,
        record_id: DrawRecordId,
    ) -> Result<DrawCommit, SessionError>
    where
        B: RenderBackend,
    {
        let commit = history.apply_stored_patch(record_id, &mut self.storage, backend)?;
        self.invalidate_layer_composite();
        Ok(commit)
    }

    pub fn dirty_tile_indices(&self, commit: &DrawCommit) -> Vec<u32> {
        let mut full_tile_count = None::<u32>;
        let mut tiles = Vec::new();
        for dirty in commit.dirty.values() {
            if dirty.is_full() {
                let tile_count = dirty.layout().tile_count();
                full_tile_count = Some(
                    full_tile_count
                        .map(|current| current.max(tile_count))
                        .unwrap_or(tile_count),
                );
                continue;
            }
            if let Some(dirty_tiles) = dirty.tile_indices() {
                tiles.extend(dirty_tiles.iter().map(|tile| tile.value()));
            }
        }
        if let Some(tile_count) = full_tile_count {
            return (0..tile_count).collect();
        }
        tiles.sort_unstable();
        tiles.dedup();
        tiles
    }

    pub fn root_dirty_tile_indices(&self, commit: &DrawCommit) -> Vec<u32> {
        let Some(dirty) = commit.dirty.get(&self.root) else {
            return Vec::new();
        };
        if dirty.is_full() {
            return (0..dirty.layout().tile_count()).collect();
        }
        dirty
            .tile_indices()
            .into_iter()
            .flatten()
            .map(|tile| tile.value())
            .collect()
    }

    pub fn root_present_tiles(&self) -> Result<Vec<PresentTile>, DocumentPresentError> {
        self.root_present_tiles_for_view(&AppView::identity())
    }

    pub fn root_physical_tiles(&self) -> Result<Vec<DocumentRootTileRead>, DocumentPresentError> {
        let layout = self.image_layout(self.root)?;
        let tile_count_x = layout.tile_count_x();
        let tile_count = layout.tile_count();
        let mut tiles = Vec::new();

        for tile_index in 0..tile_count {
            if let Some(tile) = self.physical_tile_for_index(
                self.display_image_for_tile_index(tile_index),
                tile_count_x,
                tile_index,
            )? {
                tiles.push(tile);
            }
        }

        Ok(tiles)
    }

    pub(crate) fn image_physical_tiles(
        &self,
        image: ImageId,
    ) -> Result<Vec<DocumentRootTileRead>, DocumentPresentError> {
        let layout = self.image_layout(image)?;
        let tile_count_x = layout.tile_count_x();
        let tile_count = layout.tile_count();
        let mut tiles = Vec::new();

        for tile_index in 0..tile_count {
            if let Some(tile) = self.physical_tile_for_index(image, tile_count_x, tile_index)? {
                tiles.push(tile);
            }
        }

        Ok(tiles)
    }

    pub fn root_present_tiles_for_view(
        &self,
        view: &AppView,
    ) -> Result<Vec<PresentTile>, DocumentPresentError> {
        let layout = self.root_layout()?;
        let tile_count_x = layout.tile_count_x();
        let tile_count = layout.tile_count();
        let mut tiles = Vec::new();

        for tile_index in 0..tile_count {
            if let Some(tile) = self.present_tile_for_index(
                view,
                self.display_image_for_tile_index(tile_index),
                tile_count_x,
                tile_index,
            )? {
                tiles.push(tile);
            }
        }

        Ok(tiles)
    }

    pub fn root_present_tiles_for_view_tile_indices(
        &self,
        view: &AppView,
        tile_indices: &[u32],
    ) -> Result<Vec<PresentTile>, DocumentPresentError> {
        let layout = self.root_layout()?;
        let tile_count_x = layout.tile_count_x();
        let mut tiles = Vec::new();

        for tile_index in tile_indices.iter().copied() {
            if let Some(tile) = self.present_tile_for_index(
                view,
                self.display_image_for_tile_index(tile_index),
                tile_count_x,
                tile_index,
            )? {
                tiles.push(tile);
            }
        }

        Ok(tiles)
    }

    fn root_layout(&self) -> Result<gla_image::GlaImageLayout, DocumentPresentError> {
        self.image_layout(self.root)
    }

    fn image_layout(
        &self,
        image: ImageId,
    ) -> Result<gla_image::GlaImageLayout, DocumentPresentError> {
        let image = self
            .storage
            .image(image)
            .ok_or(DocumentPresentError::MissingRoot { id: image })?;
        Ok(image.layout())
    }

    fn present_tile_for_index(
        &self,
        view: &AppView,
        image: ImageId,
        tile_count_x: u32,
        tile_index: u32,
    ) -> Result<Option<PresentTile>, DocumentPresentError> {
        let Some(tile) = self.physical_tile_for_index(image, tile_count_x, tile_index)? else {
            return Ok(None);
        };
        let tile_x = tile_index % tile_count_x;
        let tile_y = tile_index / tile_count_x;
        let origin_x = tile_x * IMAGE_TILE_SIZE;
        let origin_y = tile_y * IMAGE_TILE_SIZE;
        let target_min =
            view.document_to_screen_point(CanvasCoordF::new(origin_x as f32, origin_y as f32));
        let target_max = view.document_to_screen_point(CanvasCoordF::new(
            (origin_x + tile.source_width) as f32,
            (origin_y + tile.source_height) as f32,
        ));

        Ok(Some(PresentTile {
            src: tile.src,
            params: PresentTileParams {
                target_min_px: [target_min.x, target_min.y],
                target_max_px: [target_max.x, target_max.y],
                source_width: tile.source_width,
                source_height: tile.source_height,
            },
        }))
    }

    fn physical_tile_for_index(
        &self,
        image: ImageId,
        tile_count_x: u32,
        tile_index: u32,
    ) -> Result<Option<DocumentRootTileRead>, DocumentPresentError> {
        let tile_ref = self
            .storage
            .read_global_ref(image, tile_index)
            .map_err(DocumentPresentError::Tile)?;
        let TileReadRef::Physical(src) = tile_ref else {
            return Ok(None);
        };

        let tile_x = tile_index % tile_count_x;
        let tile_y = tile_index / tile_count_x;
        let origin_x = tile_x * IMAGE_TILE_SIZE;
        let origin_y = tile_y * IMAGE_TILE_SIZE;
        let source_width = self
            .layout
            .width_px
            .saturating_sub(origin_x)
            .min(IMAGE_TILE_SIZE);
        let source_height = self
            .layout
            .height_px
            .saturating_sub(origin_y)
            .min(IMAGE_TILE_SIZE);
        if source_width == 0 || source_height == 0 {
            return Ok(None);
        }

        Ok(Some(DocumentRootTileRead {
            tile_index,
            src,
            source_width,
            source_height,
        }))
    }
}

#[derive(Debug)]
pub enum DocumentWorkspaceBuildError<E> {
    Atlas(NewAtlasError<E>),
    Registry(GlobalStorageError),
}

#[derive(Debug)]
pub enum DocumentWorkspaceInitError<E> {
    Build(DocumentWorkspaceBuildError<E>),
    InitialPaint(SessionError),
}

#[derive(Debug)]
pub enum DocumentPresentError {
    MissingRoot { id: ImageId },
    Tile(GlobalTileError),
}

#[derive(Debug)]
pub enum DocumentWorkspaceLayerError {
    Tree(DocumentLayerTreeError),
    Registry(GlobalStorageError),
    InvalidLayerTreeRootImage { expected: ImageId, actual: ImageId },
    DuplicateLayerImage { id: ImageId },
    ImageIdExhausted,
}

#[derive(Debug)]
pub enum DocumentLayerRenderError<E> {
    Present(DocumentPresentError),
    Layer(DocumentLayerTreeError),
    Tile(GlobalTileError),
    Render(E),
}

#[derive(Debug)]
pub enum DocumentStrokePreviewError<E> {
    Layer(DocumentWorkspaceLayerError),
    Present(DocumentPresentError),
    Tile(GlobalTileError),
    DrawOnLowering(DrawOnLoweringError<Infallible>),
    Render(E),
}

impl Display for DocumentPresentError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingRoot { id } => write!(f, "document root image {id:?} is missing"),
            Self::Tile(error) => write!(f, "document root tile access failed: {error}"),
        }
    }
}

impl Error for DocumentPresentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::MissingRoot { .. } => None,
            Self::Tile(error) => Some(error),
        }
    }
}

impl Display for DocumentBrushInputError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BrushInput(error) => Display::fmt(error, f),
            Self::Session(error) => Display::fmt(error, f),
        }
    }
}

impl Error for DocumentBrushInputError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::BrushInput(error) => Some(error),
            Self::Session(error) => Some(error),
        }
    }
}

impl From<BrushInputError> for DocumentBrushInputError {
    fn from(error: BrushInputError) -> Self {
        Self::BrushInput(error)
    }
}

impl From<SessionError> for DocumentBrushInputError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

impl Display for DocumentWorkspaceLayerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tree(error) => Display::fmt(error, f),
            Self::Registry(error) => Display::fmt(error, f),
            Self::InvalidLayerTreeRootImage { expected, actual } => write!(
                f,
                "exported layer tree root image {actual:?} does not match workspace root {expected:?}"
            ),
            Self::DuplicateLayerImage { id } => {
                write!(f, "exported layer tree repeats image id {id:?}")
            }
            Self::ImageIdExhausted => f.write_str("document workspace image ids are exhausted"),
        }
    }
}

impl Error for DocumentWorkspaceLayerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Tree(error) => Some(error),
            Self::Registry(error) => Some(error),
            Self::InvalidLayerTreeRootImage { .. } | Self::DuplicateLayerImage { .. } => None,
            Self::ImageIdExhausted => None,
        }
    }
}

impl<E: Display> Display for DocumentLayerRenderError<E> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Present(error) => Display::fmt(error, f),
            Self::Layer(error) => Display::fmt(error, f),
            Self::Tile(error) => Display::fmt(error, f),
            Self::Render(error) => write!(f, "document layer render failed: {error}"),
        }
    }
}

impl<E> Error for DocumentLayerRenderError<E>
where
    E: Error + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Present(error) => Some(error),
            Self::Layer(error) => Some(error),
            Self::Tile(error) => Some(error),
            Self::Render(error) => Some(error),
        }
    }
}

impl<E: Display> Display for DocumentStrokePreviewError<E> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Layer(error) => Display::fmt(error, f),
            Self::Present(error) => Display::fmt(error, f),
            Self::Tile(error) => Display::fmt(error, f),
            Self::DrawOnLowering(error) => {
                write!(f, "document stroke preview draw lowering failed: {error:?}")
            }
            Self::Render(error) => write!(f, "document stroke preview render failed: {error}"),
        }
    }
}

impl<E> Error for DocumentStrokePreviewError<E>
where
    E: Error + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Layer(error) => Some(error),
            Self::Present(error) => Some(error),
            Self::Tile(error) => Some(error),
            Self::DrawOnLowering(_) => None,
            Self::Render(error) => Some(error),
        }
    }
}

impl<E> From<DocumentWorkspaceLayerError> for DocumentStrokePreviewError<E> {
    fn from(error: DocumentWorkspaceLayerError) -> Self {
        Self::Layer(error)
    }
}

impl From<DocumentLayerTreeError> for DocumentWorkspaceLayerError {
    fn from(error: DocumentLayerTreeError) -> Self {
        Self::Tree(error)
    }
}

impl From<GlobalStorageError> for DocumentWorkspaceLayerError {
    fn from(error: GlobalStorageError) -> Self {
        Self::Registry(error)
    }
}

pub type DocumentWorkspaceError = DocumentWorkspaceBuildError<Infallible>;

impl<E: Display> Display for DocumentWorkspaceInitError<E> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Build(error) => Display::fmt(error, f),
            Self::InitialPaint(error) => write!(f, "failed to initialize document canvas: {error}"),
        }
    }
}

impl<E> Error for DocumentWorkspaceInitError<E>
where
    E: Error + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Build(error) => Some(error),
            Self::InitialPaint(error) => Some(error),
        }
    }
}

impl<E: Display> Display for DocumentWorkspaceBuildError<E> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Atlas(error) => write!(f, "failed to allocate document atlas: {error}"),
            Self::Registry(error) => write!(f, "failed to create document registry: {error}"),
        }
    }
}

impl<E> Error for DocumentWorkspaceBuildError<E>
where
    E: Error + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Atlas(error) => Some(error),
            Self::Registry(error) => Some(error),
        }
    }
}

fn default_canvas_format() -> GlaFormat {
    GlaFormat {
        channel_count: ChannelCount::D4,
        channel_type: ChannelType::F32,
    }
}

fn initial_atlas_layout(layout: ImageLayoutSpec, min_slots: u64) -> AtlasLayout {
    const INITIAL_FULL_IMAGE_CAPACITY: u64 = 4;
    let tile_count_x = layout.width_px.div_ceil(IMAGE_TILE_SIZE);
    let tile_count_y = layout.height_px.div_ceil(IMAGE_TILE_SIZE);
    let canvas_slots = u64::from(tile_count_x)
        .saturating_mul(u64::from(tile_count_y))
        .saturating_mul(INITIAL_FULL_IMAGE_CAPACITY);
    atlas_layout_for_slot_count(canvas_slots.max(min_slots))
}

fn atlas_layout_for_slot_count(slot_count: u64) -> AtlasLayout {
    [
        AtlasLayout::TINY8,
        AtlasLayout::SMALL11,
        AtlasLayout::MEDIUM14,
        AtlasLayout::LARGE17,
        AtlasLayout::HUGE20,
    ]
    .into_iter()
    .find(|layout| layout.total_slots() as u64 >= slot_count)
    .unwrap_or(AtlasLayout::HUGE20)
}

fn union_tile_indices(left: &[u32], right: &[u32]) -> Vec<u32> {
    let mut union = Vec::with_capacity(left.len() + right.len());
    union.extend_from_slice(left);
    union.extend_from_slice(right);
    sort_dedup(&mut union);
    union
}

fn sort_dedup(values: &mut Vec<u32>) {
    values.sort_unstable();
    values.dedup();
}

fn sort_dedup_tile_pos(values: &mut Vec<TilePos>) {
    values.sort_by_key(|pos| (pos.atlas_id, pos.layer, pos.tile_x, pos.tile_y));
    values.dedup();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ScriptDrawCommand, ScriptDrawFrame, ScriptDrawSession};
    use gla_draw_on::DrawOnInput;
    use gla_renderer::{Pass, RenderBackend};
    use std::fmt::{Display, Formatter};

    #[derive(Debug)]
    struct RecordingBackendError;

    impl Display for RecordingBackendError {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            f.write_str("recording backend failed")
        }
    }

    impl Error for RecordingBackendError {}

    #[derive(Default)]
    struct RecordingBackend {
        submitted: Vec<Vec<Pass>>,
    }

    impl RecordingBackend {
        fn submitted_passes(&self) -> impl Iterator<Item = Pass> + '_ {
            self.submitted
                .iter()
                .flat_map(|passes| passes.iter().copied())
        }
    }

    impl RenderBackend for RecordingBackend {
        type Error = RecordingBackendError;

        fn submit(&mut self, passes: &[Pass]) -> Result<(), Self::Error> {
            self.submitted.push(passes.to_vec());
            Ok(())
        }
    }

    impl AtlasTextureStore for RecordingBackend {
        type Error = RecordingBackendError;

        fn create_atlas_texture(
            &mut self,
            _atlas_id: u8,
            _layout: AtlasLayout,
            _format: GlaFormat,
        ) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn default_blank_matches_preview_canvas_size_contract() {
        let workspace = DocumentWorkspace::default_blank().unwrap();

        assert_eq!(workspace.canvas_size_px(), (1024, 1024));
        assert_eq!(workspace.version(), DocumentVersionId::new(1));
    }

    #[test]
    fn blank_workspace_uses_compact_initial_atlas_layout() {
        let default = DocumentWorkspace::default_blank().unwrap();
        let small = DocumentWorkspace::blank(128, 96).unwrap();

        assert_eq!(
            default.storage().tiles().atlases()[0].layout,
            AtlasLayout::SMALL11
        );
        assert_eq!(
            small.storage().tiles().atlases()[0].layout,
            AtlasLayout::TINY8
        );
    }

    #[test]
    fn blank_workspace_creates_root_primitive_document() {
        let workspace = DocumentWorkspace::blank(320, 240).unwrap();
        let root = workspace.root();
        let image = workspace.storage().image(root).unwrap();
        let root_node = workspace
            .layer_tree()
            .node(workspace.layer_tree().root_id())
            .unwrap();

        assert_eq!(workspace.storage().root(), Some(root));
        assert_eq!(workspace.version(), DocumentVersionId::new(1));
        assert_eq!(workspace.canvas_size_px(), (320, 240));
        assert_eq!(workspace.format(), default_canvas_format());
        assert!(image.role().is_primitive());
        assert_eq!(image.layout().width_px(), 320);
        assert_eq!(image.layout().height_px(), 240);
        assert_eq!(root_node.kind(), DocumentNodeKind::Root);
        assert_eq!(root_node.image(), root);
        assert_eq!(
            workspace.layer_tree().active_ancestor_chain(),
            &[workspace.layer_tree().root_id()]
        );
    }

    #[test]
    fn root_reader_ir_begins_session_at_current_version() {
        let mut workspace = DocumentWorkspace::blank(320, 240).unwrap();
        let expected = workspace.version();
        let ir = workspace.root_reader_ir();
        let session = workspace.begin_session(&ir).unwrap();

        assert_eq!(session.expected_document_version(), expected);
        session.discard();
    }

    #[test]
    fn root_replace_circle_ir_reserves_direct_paint_session() {
        let mut workspace = DocumentWorkspace::blank(320, 240).unwrap();
        let ir = workspace.root_replace_circle_ir();

        assert!(
            ir.required_draw_on_tools()
                .contains(&DrawOnToolKind::ReplaceCircle4D)
        );
        let session = workspace.begin_session(&ir).unwrap();
        session.discard();
    }

    #[test]
    fn registry_patch_updates_workspace_root_metadata() {
        let mut workspace = DocumentWorkspace::blank(320, 240).unwrap();
        let version = workspace
            .apply_registry_patch(RegistryPatch::new(vec![
                RegistryPatchOp::NewImage {
                    id: ImageId::new(2),
                    format: default_canvas_format(),
                    layout: ImageLayoutSpec::new(64, 32),
                    role: ImageRole::Primitive,
                },
                RegistryPatchOp::SetRoot(ImageId::new(2)),
            ]))
            .unwrap();

        assert_eq!(version, DocumentVersionId::new(2));
        assert_eq!(workspace.root(), ImageId::new(2));
        assert_eq!(workspace.canvas_size_px(), (64, 32));
        assert_eq!(workspace.storage().root(), Some(ImageId::new(2)));
        assert!(workspace.root_present_tiles().unwrap().is_empty());
        assert_eq!(workspace.layer_tree().len(), 1);
        assert_eq!(
            workspace
                .layer_tree()
                .node(workspace.layer_tree().root_id())
                .unwrap()
                .image(),
            ImageId::new(2)
        );
    }

    #[test]
    fn workspace_layer_tree_registers_storage_images() {
        let mut workspace = DocumentWorkspace::blank(320, 240).unwrap();
        let root_node = workspace.layer_tree().root_id();

        let group = workspace.append_group(root_node).unwrap();
        let layer = workspace.append_layer(group).unwrap();

        let group_image = workspace.layer_tree().node(group).unwrap().image();
        let layer_image = workspace.layer_tree().node(layer).unwrap().image();
        assert_eq!(workspace.version(), DocumentVersionId::new(3));
        assert_eq!(
            workspace.layer_tree().child_ids(root_node).unwrap(),
            &[group]
        );
        assert_eq!(workspace.layer_tree().child_ids(group).unwrap(), &[layer]);
        assert_eq!(
            workspace.layer_tree().node(group).unwrap().kind(),
            DocumentNodeKind::Group
        );
        assert_eq!(
            workspace.layer_tree().node(layer).unwrap().kind(),
            DocumentNodeKind::Layer
        );
        assert_eq!(workspace.layer_tree().active_node_id(), layer);
        assert_eq!(workspace.active_paint_image(), Some(layer_image));
        assert!(
            workspace
                .storage()
                .image(group_image)
                .unwrap()
                .role()
                .is_derived()
        );
        assert!(
            workspace
                .storage()
                .image(layer_image)
                .unwrap()
                .role()
                .is_primitive()
        );
    }

    #[test]
    fn workspace_layer_operations_track_active_node_and_metadata() {
        let mut workspace = DocumentWorkspace::blank(320, 240).unwrap();
        let root_node = workspace.layer_tree().root_id();
        let group = workspace.append_group(root_node).unwrap();
        let layer = workspace.append_layer(group).unwrap();
        let group_image = workspace.layer_tree().node(group).unwrap().image();
        let layer_image = workspace.layer_tree().node(layer).unwrap().image();

        workspace.set_active_node(layer).unwrap();
        workspace.set_node_opacity(layer, 0.5).unwrap();
        workspace
            .set_node_blend_mode(layer, DocumentBlendMode::Multiply)
            .unwrap();

        let node = workspace.layer_tree().node(layer).unwrap();
        assert_eq!(workspace.layer_tree().active_node_id(), layer);
        assert_eq!(
            workspace.layer_tree().active_ancestor_chain(),
            &[layer, group, root_node]
        );
        assert_eq!(node.opacity(), 0.5);
        assert_eq!(node.blend_mode(), DocumentBlendMode::Multiply);

        workspace.delete_node(group).unwrap();
        assert_eq!(workspace.layer_tree().active_node_id(), root_node);
        assert!(!workspace.layer_tree().contains_node(layer));
        assert!(workspace.storage().image(group_image).is_none());
        assert!(workspace.storage().image(layer_image).is_none());
    }

    #[test]
    fn workspace_inserts_new_nodes_above_active_node() {
        let mut workspace = DocumentWorkspace::blank(320, 240).unwrap();
        let root_node = workspace.layer_tree().root_id();

        let first = workspace.insert_layer_above_active().unwrap();
        let second = workspace.insert_layer_above_active().unwrap();
        workspace.set_active_node(first).unwrap();
        let group = workspace.insert_group_above_active().unwrap();

        assert_eq!(
            workspace.layer_tree().child_ids(root_node).unwrap(),
            &[first, group, second]
        );
        assert_eq!(workspace.layer_tree().active_node_id(), group);
        assert_eq!(workspace.active_paint_image(), None);
    }

    #[test]
    fn workspace_deletes_active_node_without_deleting_root() {
        let mut workspace = DocumentWorkspace::blank(320, 240).unwrap();
        let root_node = workspace.layer_tree().root_id();

        assert!(!workspace.delete_active_node().unwrap());
        let layer = workspace.append_layer(root_node).unwrap();
        let layer_image = workspace.layer_tree().node(layer).unwrap().image();

        assert!(workspace.delete_active_node().unwrap());
        assert_eq!(workspace.layer_tree().active_node_id(), root_node);
        assert!(!workspace.layer_tree().contains_node(layer));
        assert!(workspace.storage().image(layer_image).is_none());
    }

    #[test]
    fn active_replace_circle_ir_targets_paintable_active_node() {
        let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
        let root_node = workspace.layer_tree().root_id();
        assert_eq!(workspace.active_paint_image(), Some(workspace.root()));
        assert!(
            workspace
                .active_replace_circle_ir()
                .unwrap()
                .doc_images
                .contains(&DocImageUse::read_write(workspace.root()))
        );

        let group = workspace.append_group(root_node).unwrap();
        assert_eq!(workspace.layer_tree().active_node_id(), group);
        assert_eq!(workspace.active_paint_image(), None);
        assert!(workspace.active_replace_circle_ir().is_none());

        let layer = workspace.append_layer(group).unwrap();
        let layer_image = workspace.layer_tree().node(layer).unwrap().image();
        let ir = workspace.active_replace_circle_ir().unwrap();

        assert_eq!(workspace.layer_tree().active_node_id(), layer);
        assert_eq!(workspace.active_paint_image(), Some(layer_image));
        assert!(
            ir.doc_images
                .contains(&DocImageUse::read_write(layer_image))
        );
        assert!(ir.draw_on.iter().any(|command| command.dst == layer_image));
    }

    #[test]
    fn layer_composite_render_noops_without_layer_children() {
        let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
        let mut backend = RecordingBackend::default();

        let dirty = workspace.render_layer_tree_full(&mut backend).unwrap();

        assert!(dirty.is_empty());
        assert!(!workspace.layer_composite_needs_render());
        assert!(backend.submitted.is_empty());
    }

    #[test]
    fn layer_composite_render_skips_empty_layer_children() {
        let mut backend = RecordingBackend::default();
        let mut workspace = DocumentWorkspace::white_with_textures(128, 96, &mut backend).unwrap();
        backend.submitted.clear();
        let root_node = workspace.layer_tree().root_id();

        workspace.append_layer(root_node).unwrap();
        let dirty = workspace.render_layer_tree_full(&mut backend).unwrap();

        assert!(dirty.is_empty());
        assert!(!workspace.layer_composite_needs_render());
        assert!(backend.submitted.is_empty());
        assert_eq!(
            workspace.root_present_tiles().unwrap().len() as u32,
            workspace
                .storage()
                .image(workspace.root())
                .unwrap()
                .layout()
                .tile_count()
        );
    }

    #[test]
    fn layer_composite_render_blends_layer_over_root() {
        let mut backend = RecordingBackend::default();
        let mut workspace = DocumentWorkspace::white_with_textures(128, 96, &mut backend).unwrap();
        backend.submitted.clear();
        let root_node = workspace.layer_tree().root_id();
        let layer = workspace.append_layer(root_node).unwrap();
        let layer_image = workspace.layer_tree().node(layer).unwrap().image();
        let request = ScriptDrawSession::with_frames(
            DrawSessionIR {
                expected_document_version: workspace.version(),
                doc_images: vec![DocImageUse::read_write(layer_image)],
                session_images: Vec::new(),
                draw_on: vec![DrawOnCommand::with_tool(
                    layer_image,
                    DrawOnToolKind::ReplaceCircle4D,
                )],
                derive: Vec::new(),
            },
            vec![ScriptDrawFrame::new(vec![ScriptDrawCommand::DrawOn {
                target: layer_image,
                input: DrawOnInput::replace_circle_4d(
                    24.0,
                    32.0,
                    8.0,
                    8.0,
                    PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0),
                ),
            }])],
        );
        let mut history = DrawHistory::new();
        workspace
            .run_script_draw_session(&mut history, &mut backend, &request)
            .unwrap()
            .unwrap();
        backend.submitted.clear();

        let dirty = workspace.render_layer_tree_full(&mut backend).unwrap();

        assert_eq!(dirty, vec![0]);
        assert!(!workspace.layer_composite_needs_render());
        assert!(backend.submitted_passes().any(|pass| matches!(
            pass,
            Pass::RenderTo {
                blend_mode: gla_color::BlendMode::Normal,
                opacity,
                ..
            } if opacity == 1.0
        )));
        assert_eq!(
            workspace.root_present_tiles().unwrap().len() as u32,
            workspace
                .storage()
                .image(workspace.root())
                .unwrap()
                .layout()
                .tile_count()
        );
        assert_eq!(
            workspace.root_physical_tiles().unwrap().len() as u32,
            workspace
                .storage()
                .image(workspace.root())
                .unwrap()
                .layout()
                .tile_count()
        );
    }

    #[test]
    fn blank_workspace_can_use_injected_texture_store() {
        let mut textures = NoAtlasTextures;
        let workspace = DocumentWorkspace::blank_with_textures(128, 96, &mut textures).unwrap();

        assert_eq!(workspace.canvas_size_px(), (128, 96));
        assert_eq!(workspace.storage().root(), Some(workspace.root()));
    }

    #[test]
    fn white_workspace_initializes_root_tiles_without_external_history() {
        let mut backend = RecordingBackend::default();
        let workspace = DocumentWorkspace::white_with_textures(128, 96, &mut backend).unwrap();

        assert_eq!(workspace.canvas_size_px(), (128, 96));
        assert_eq!(workspace.version(), DocumentVersionId::new(2));
        assert_eq!(
            workspace.root_present_tiles().unwrap().len() as u32,
            workspace
                .storage()
                .image(workspace.root())
                .unwrap()
                .layout()
                .tile_count()
        );
        assert!(backend.submitted_passes().any(|pass| matches!(
            pass,
            Pass::DrawOn(gla_draw_on::DrawOnInvocation::ReplaceCircle4D { .. })
        )));
    }

    #[test]
    fn stroke_preview_renders_hidden_composite_for_present_tiles() {
        let mut backend = RecordingBackend::default();
        let mut workspace = DocumentWorkspace::white_with_textures(128, 96, &mut backend).unwrap();
        backend.submitted.clear();
        let version = workspace.version();

        let dirty = workspace
            .render_stroke_preview(
                &mut backend,
                [ReplaceCircleStrokeSample::new(
                    32.0,
                    32.0,
                    8.0,
                    PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0),
                )],
            )
            .unwrap();

        assert_eq!(dirty, vec![0]);
        assert_eq!(workspace.version(), version);
        assert_eq!(workspace.layer_tree().len(), 1);
        assert_eq!(workspace.stroke_preview_tile_indices, vec![0]);
        let preview_composite = workspace.stroke_preview_composite_image.unwrap();
        let TileReadRef::Physical(composite_tile) = workspace
            .storage
            .read_global_ref(preview_composite, 0)
            .unwrap()
        else {
            panic!("stroke preview composite tile should be materialized");
        };
        let present = workspace
            .root_present_tiles_for_view_tile_indices(&AppView::identity(), &[0])
            .unwrap();
        assert_eq!(present.len(), 1);
        assert_eq!(present[0].src, composite_tile);
        assert!(backend.submitted_passes().any(|pass| matches!(
            pass,
            Pass::DrawOn(gla_draw_on::DrawOnInvocation::ReplaceCircle4D { .. })
        )));
        assert!(backend.submitted_passes().any(|pass| matches!(
            pass,
            Pass::RenderTo {
                blend_mode: gla_color::BlendMode::Normal,
                opacity: 1.0,
                ..
            }
        )));
    }

    #[test]
    fn stroke_preview_move_marks_old_and_new_tiles_dirty() {
        let mut backend = RecordingBackend::default();
        let mut workspace = DocumentWorkspace::white_with_textures(300, 128, &mut backend).unwrap();

        let first_dirty = workspace
            .render_stroke_preview(
                &mut backend,
                [ReplaceCircleStrokeSample::new(
                    32.0,
                    32.0,
                    8.0,
                    PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0),
                )],
            )
            .unwrap();
        let moved_dirty = workspace
            .render_stroke_preview(
                &mut backend,
                [ReplaceCircleStrokeSample::new(
                    260.0,
                    32.0,
                    8.0,
                    PremultipliedRgbaF32::new(0.0, 1.0, 0.0, 1.0),
                )],
            )
            .unwrap();

        assert_eq!(first_dirty, vec![0]);
        assert_eq!(moved_dirty, vec![0, 4]);
        assert_eq!(workspace.stroke_preview_tile_indices, vec![4]);
    }

    #[test]
    fn clear_stroke_preview_restores_base_presentation() {
        let mut backend = RecordingBackend::default();
        let mut workspace = DocumentWorkspace::white_with_textures(128, 96, &mut backend).unwrap();

        workspace
            .render_stroke_preview(
                &mut backend,
                [ReplaceCircleStrokeSample::new(
                    32.0,
                    32.0,
                    8.0,
                    PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0),
                )],
            )
            .unwrap();
        let root_tile = workspace.image_physical_tiles(workspace.root()).unwrap()[0].src;

        let dirty = workspace.clear_stroke_preview(&mut backend).unwrap();
        let present = workspace
            .root_present_tiles_for_view_tile_indices(&AppView::identity(), &[0])
            .unwrap();

        assert_eq!(dirty, vec![0]);
        assert!(workspace.stroke_preview_tile_indices.is_empty());
        assert_eq!(present.len(), 1);
        assert_eq!(present[0].src, root_tile);
    }

    #[test]
    fn replace_circle_on_root_flushes_and_commits_document_edit() {
        let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
        let mut history = DrawHistory::new();
        let mut backend = RecordingBackend::default();

        let commit = workspace
            .replace_circle_on_root(
                &mut history,
                &mut backend,
                24.0,
                32.0,
                8.0,
                PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0),
            )
            .unwrap()
            .unwrap();

        assert_eq!(commit.version, DocumentVersionId::new(2));
        assert_eq!(workspace.version(), DocumentVersionId::new(2));
        assert_eq!(workspace.root_dirty_tile_indices(&commit), vec![0]);
        assert!(backend.submitted_passes().any(|pass| matches!(
            pass,
            Pass::DrawOn(gla_draw_on::DrawOnInvocation::ReplaceCircle4D { .. })
        )));
    }

    #[test]
    fn replace_circle_stroke_on_root_batches_samples_into_one_commit() {
        let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
        let mut history = DrawHistory::new();
        let mut backend = RecordingBackend::default();
        let color = PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0);

        let commit = workspace
            .replace_circle_stroke_on_root(
                &mut history,
                &mut backend,
                [
                    ReplaceCircleStrokeSample::new(24.0, 32.0, 8.0, color),
                    ReplaceCircleStrokeSample::new(34.0, 42.0, 8.0, color),
                ],
            )
            .unwrap()
            .unwrap();

        assert_eq!(commit.version, DocumentVersionId::new(2));
        assert_eq!(workspace.version(), DocumentVersionId::new(2));
        assert_eq!(backend.submitted.len(), 1);
        assert_eq!(
            backend
                .submitted_passes()
                .filter(|pass| matches!(
                    pass,
                    Pass::DrawOn(gla_draw_on::DrawOnInvocation::ReplaceCircle4D { .. })
                ))
                .count(),
            2
        );

        let redo_commit = workspace
            .apply_draw_record(&mut history, &mut backend, commit.record_id)
            .unwrap();
        assert!(workspace.root_present_tiles().unwrap().is_empty());
        assert_eq!(workspace.root_dirty_tile_indices(&redo_commit), vec![0]);
        assert_ne!(redo_commit.record_id, commit.record_id);
    }

    #[test]
    fn replace_circle_stroke_on_root_splits_flushes_by_frame_budget() {
        let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
        let mut history = DrawHistory::new();
        let mut backend = RecordingBackend::default();
        let color = PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0);

        let commit = workspace
            .replace_circle_stroke_on_root_with_frame_budget(
                &mut history,
                &mut backend,
                [
                    ReplaceCircleStrokeSample::new(24.0, 32.0, 8.0, color),
                    ReplaceCircleStrokeSample::new(34.0, 42.0, 8.0, color),
                    ReplaceCircleStrokeSample::new(44.0, 52.0, 8.0, color),
                ],
                2,
            )
            .unwrap()
            .unwrap();
        let draw_counts: Vec<usize> = backend
            .submitted
            .iter()
            .map(|passes| {
                passes
                    .iter()
                    .filter(|pass| {
                        matches!(
                            pass,
                            Pass::DrawOn(gla_draw_on::DrawOnInvocation::ReplaceCircle4D { .. })
                        )
                    })
                    .count()
            })
            .collect();

        assert_eq!(commit.version, DocumentVersionId::new(2));
        assert_eq!(workspace.version(), DocumentVersionId::new(2));
        assert_eq!(draw_counts, vec![2, 1]);
        assert_eq!(workspace.root_dirty_tile_indices(&commit), vec![0]);
    }

    #[test]
    fn replace_circle_brush_input_on_root_commits_with_frame_budget() {
        let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
        let mut history = DrawHistory::new();
        let mut backend = RecordingBackend::default();
        let color = PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0);
        let brush_input = BrushInput::from_replace_circle_samples(
            crate::BrushId::DEFAULT,
            [
                ReplaceCircleStrokeSample::new(24.0, 32.0, 8.0, color),
                ReplaceCircleStrokeSample::new(34.0, 42.0, 8.0, color),
                ReplaceCircleStrokeSample::new(44.0, 52.0, 8.0, color),
            ],
        );

        let commit = workspace
            .replace_circle_brush_input_on_root_with_frame_budget(
                &mut history,
                &mut backend,
                &brush_input,
                2,
            )
            .unwrap()
            .unwrap();
        let draw_counts: Vec<usize> = backend
            .submitted
            .iter()
            .map(|passes| {
                passes
                    .iter()
                    .filter(|pass| {
                        matches!(
                            pass,
                            Pass::DrawOn(gla_draw_on::DrawOnInvocation::ReplaceCircle4D { .. })
                        )
                    })
                    .count()
            })
            .collect();

        assert_eq!(commit.version, DocumentVersionId::new(2));
        assert_eq!(workspace.version(), DocumentVersionId::new(2));
        assert_eq!(draw_counts, vec![2, 1]);
        assert_eq!(workspace.root_dirty_tile_indices(&commit), vec![0]);
    }

    #[test]
    fn replace_circle_stroke_on_active_layer_writes_layer_image() {
        let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
        let root_node = workspace.layer_tree().root_id();
        let layer = workspace.append_layer(root_node).unwrap();
        let layer_image = workspace.layer_tree().node(layer).unwrap().image();
        let mut history = DrawHistory::new();
        let mut backend = RecordingBackend::default();
        let color = PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0);

        let commit = workspace
            .replace_circle_stroke_on_active_paint_target(
                &mut history,
                &mut backend,
                [ReplaceCircleStrokeSample::new(24.0, 32.0, 8.0, color)],
            )
            .unwrap()
            .unwrap();

        assert!(commit.dirty.contains_key(&layer_image));
        assert!(!commit.dirty.contains_key(&workspace.root()));
        assert_eq!(workspace.dirty_tile_indices(&commit), vec![0]);
        assert_eq!(
            workspace.root_dirty_tile_indices(&commit),
            Vec::<u32>::new()
        );
        assert!(workspace.layer_composite_needs_render());
    }

    #[test]
    fn replace_circle_stroke_on_active_layer_splits_flushes_by_frame_budget() {
        let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
        let root_node = workspace.layer_tree().root_id();
        let layer = workspace.append_layer(root_node).unwrap();
        let layer_image = workspace.layer_tree().node(layer).unwrap().image();
        let mut history = DrawHistory::new();
        let mut backend = RecordingBackend::default();
        let color = PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0);

        let commit = workspace
            .replace_circle_stroke_on_active_paint_target_with_frame_budget(
                &mut history,
                &mut backend,
                [
                    ReplaceCircleStrokeSample::new(24.0, 32.0, 8.0, color),
                    ReplaceCircleStrokeSample::new(34.0, 42.0, 8.0, color),
                ],
                1,
            )
            .unwrap()
            .unwrap();
        let draw_counts: Vec<usize> = backend
            .submitted
            .iter()
            .map(|passes| {
                passes
                    .iter()
                    .filter(|pass| {
                        matches!(
                            pass,
                            Pass::DrawOn(gla_draw_on::DrawOnInvocation::ReplaceCircle4D { .. })
                        )
                    })
                    .count()
            })
            .collect();

        assert!(commit.dirty.contains_key(&layer_image));
        assert_eq!(draw_counts, vec![1, 1]);
        assert!(workspace.layer_composite_needs_render());
    }

    #[test]
    fn active_layer_stroke_undo_redo_reports_layer_dirty_tiles() {
        let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
        let root_node = workspace.layer_tree().root_id();
        let layer = workspace.append_layer(root_node).unwrap();
        let layer_image = workspace.layer_tree().node(layer).unwrap().image();
        let mut history = DrawHistory::new();
        let mut backend = RecordingBackend::default();
        let color = PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0);

        let paint_commit = workspace
            .replace_circle_stroke_on_active_paint_target(
                &mut history,
                &mut backend,
                [ReplaceCircleStrokeSample::new(24.0, 32.0, 8.0, color)],
            )
            .unwrap()
            .unwrap();
        let undo_commit = workspace
            .apply_draw_record(&mut history, &mut backend, paint_commit.record_id)
            .unwrap();
        let redo_commit = workspace
            .apply_draw_record(&mut history, &mut backend, undo_commit.record_id)
            .unwrap();

        assert!(undo_commit.dirty.contains_key(&layer_image));
        assert!(redo_commit.dirty.contains_key(&layer_image));
        assert!(!undo_commit.dirty.contains_key(&workspace.root()));
        assert!(!redo_commit.dirty.contains_key(&workspace.root()));
        assert_eq!(workspace.dirty_tile_indices(&undo_commit), vec![0]);
        assert_eq!(workspace.dirty_tile_indices(&redo_commit), vec![0]);
    }

    #[test]
    fn root_present_tiles_skip_zero_tiles_and_include_committed_physical_tiles() {
        let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
        assert!(workspace.root_present_tiles().unwrap().is_empty());
        assert!(workspace.root_physical_tiles().unwrap().is_empty());

        let mut history = DrawHistory::new();
        let mut backend = RecordingBackend::default();
        workspace
            .replace_circle_on_root(
                &mut history,
                &mut backend,
                24.0,
                32.0,
                8.0,
                PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0),
            )
            .unwrap()
            .unwrap();

        let tiles = workspace.root_present_tiles().unwrap();
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].params.target_min_px, [0.0, 0.0]);
        assert_eq!(tiles[0].params.target_max_px, [62.0, 62.0]);
        assert_eq!(tiles[0].params.source_width, IMAGE_TILE_SIZE);
        assert_eq!(tiles[0].params.source_height, IMAGE_TILE_SIZE);
    }

    #[test]
    fn root_physical_tiles_expose_export_readback_metadata() {
        let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
        let mut history = DrawHistory::new();
        let mut backend = RecordingBackend::default();
        workspace
            .replace_circle_on_root(
                &mut history,
                &mut backend,
                24.0,
                32.0,
                8.0,
                PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0),
            )
            .unwrap()
            .unwrap();

        let physical = workspace.root_physical_tiles().unwrap();
        let present = workspace.root_present_tiles().unwrap();

        assert_eq!(physical.len(), 1);
        assert_eq!(physical[0].tile_index, 0);
        assert_eq!(physical[0].source_width, IMAGE_TILE_SIZE);
        assert_eq!(physical[0].source_height, IMAGE_TILE_SIZE);
        assert_eq!(physical[0].src, present[0].src);
    }

    #[test]
    fn root_present_tiles_apply_view_transform() {
        let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
        let mut history = DrawHistory::new();
        let mut backend = RecordingBackend::default();
        workspace
            .replace_circle_on_root(
                &mut history,
                &mut backend,
                24.0,
                32.0,
                8.0,
                PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0),
            )
            .unwrap()
            .unwrap();
        let view = AppView::new([2.0, 0.0, 0.0, 2.0, 10.0, 20.0]).unwrap();

        let tiles = workspace.root_present_tiles_for_view(&view).unwrap();

        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].params.target_min_px, [10.0, 20.0]);
        assert_eq!(tiles[0].params.target_max_px, [134.0, 144.0]);
    }

    #[test]
    fn root_present_tiles_can_filter_by_tile_indices() {
        let mut backend = RecordingBackend::default();
        let workspace = DocumentWorkspace::white_with_textures(128, 96, &mut backend).unwrap();
        let view = AppView::new([1.5, 0.0, 0.0, 1.5, 4.0, 8.0]).unwrap();

        let all_tiles = workspace.root_present_tiles_for_view(&view).unwrap();
        assert!(all_tiles.len() > 2);
        let filtered = workspace
            .root_present_tiles_for_view_tile_indices(&view, &[0, 2])
            .unwrap();

        assert_eq!(filtered, vec![all_tiles[0], all_tiles[2]]);
    }

    #[test]
    fn draw_record_can_be_applied_for_undo_and_redo() {
        let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
        let mut history = DrawHistory::new();
        let mut backend = RecordingBackend::default();
        let commit = workspace
            .replace_circle_on_root(
                &mut history,
                &mut backend,
                24.0,
                32.0,
                8.0,
                PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0),
            )
            .unwrap()
            .unwrap();

        assert!(!workspace.root_present_tiles().unwrap().is_empty());
        let redo_commit = workspace
            .apply_draw_record(&mut history, &mut backend, commit.record_id)
            .unwrap();
        assert!(workspace.root_present_tiles().unwrap().is_empty());
        assert_eq!(workspace.root_dirty_tile_indices(&redo_commit), vec![0]);
        let undo_commit = workspace
            .apply_draw_record(&mut history, &mut backend, redo_commit.record_id)
            .unwrap();

        assert_ne!(undo_commit.record_id, redo_commit.record_id);
        assert_eq!(workspace.root_dirty_tile_indices(&undo_commit), vec![0]);
        assert!(!workspace.root_present_tiles().unwrap().is_empty());
    }

    #[test]
    fn script_draw_session_runs_frames_and_commits_document_edit() {
        let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
        let mut history = DrawHistory::new();
        let mut backend = RecordingBackend::default();
        let root = workspace.root();
        let request = ScriptDrawSession::with_frames(
            workspace.root_replace_circle_ir(),
            vec![ScriptDrawFrame::new(vec![ScriptDrawCommand::DrawOn {
                target: root,
                input: DrawOnInput::replace_circle_4d(
                    24.0,
                    32.0,
                    8.0,
                    8.0,
                    PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0),
                ),
            }])],
        );

        let commit = workspace
            .run_script_draw_session(&mut history, &mut backend, &request)
            .unwrap()
            .unwrap();

        assert_eq!(commit.version, DocumentVersionId::new(2));
        assert_eq!(workspace.root_dirty_tile_indices(&commit), vec![0]);
        assert!(backend.submitted_passes().any(|pass| matches!(
            pass,
            Pass::DrawOn(gla_draw_on::DrawOnInvocation::ReplaceCircle4D { .. })
        )));
    }

    #[test]
    fn blank_workspace_rejects_empty_layout() {
        let error = match DocumentWorkspace::blank(0, 240) {
            Ok(_) => panic!("empty layout should be rejected"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            DocumentWorkspaceError::Registry(GlobalStorageError::ImageCreate { .. })
        ));
    }
}
