use crate::{GlobalImage, GlobalStorage};
use atlas::TilePos;
use gla_color::GlaFormat;
use gla_core::CanvasInput;
use gla_image::{
    CacheImage, DenseImage, GlaImageLayout, IMAGE_TILE_SIZE, ImageError, TileReplaceError, TileSet,
};
use gla_image_command::RenderCtx;
use gla_image_command::{Copy, Derive, DeriveCommand as ImageDeriveCommand, ImageRef};
use gla_ir::{
    DocumentImageAccess, DocumentVersionId, DrawOnCommand, DrawSessionIR, FootprintModifier,
    GraphCommand, ImageId, Mapping, MetadataRef, SessionCommand, SessionImageDecl,
    SessionReadImage, Tool,
};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use tile_key::{Tile, TileReadRef, Tiles, TilesError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionImageId {
    Current(ImageId),
    Global(ImageId),
}

#[derive(Debug)]
pub struct ImageEdit {
    edits: Vec<(u32, Tile)>,
}

impl ImageEdit {
    pub fn new() -> Self {
        Self { edits: Vec::new() }
    }

    pub fn from_sorted_unique(edits: Vec<(u32, Tile)>) -> Result<Self, ImageEditCreateError> {
        for pair in edits.windows(2) {
            if pair[0].0 >= pair[1].0 {
                return Err(ImageEditCreateError { edits });
            }
        }
        Ok(Self { edits })
    }

    pub fn edits(&self) -> &[(u32, Tile)] {
        &self.edits
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    pub fn into_edits(self) -> Vec<(u32, Tile)> {
        self.edits
    }

    fn take(&mut self) -> Self {
        Self {
            edits: std::mem::take(&mut self.edits),
        }
    }

    fn tile(&self, tile_index: u32) -> Option<&Tile> {
        self.edits
            .binary_search_by_key(&tile_index, |(index, _)| *index)
            .ok()
            .map(|index| &self.edits[index].1)
    }

    fn tile_mut(&mut self, tile_index: u32) -> Option<&mut Tile> {
        self.edits
            .binary_search_by_key(&tile_index, |(index, _)| *index)
            .ok()
            .map(|index| &mut self.edits[index].1)
    }

    fn insert_tile(&mut self, tile_index: u32, tile: Tile) -> &mut Tile {
        let index = self
            .edits
            .binary_search_by_key(&tile_index, |(index, _)| *index)
            .expect_err("image edit tile must not already exist");
        self.edits.insert(index, (tile_index, tile));
        &mut self.edits[index].1
    }

    fn release_tiles(self, tiles: &mut Tiles) {
        for (_, tile) in self.edits {
            tiles.release(tile);
        }
    }
}

impl Default for ImageEdit {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct ImageEditCreateError {
    edits: Vec<(u32, Tile)>,
}

impl ImageEditCreateError {
    pub fn into_edits(self) -> Vec<(u32, Tile)> {
        self.edits
    }
}

impl Display for ImageEditCreateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("image edit entries must have strictly increasing unique tile indices")
    }
}

impl Error for ImageEditCreateError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrawCommit {
    pub record_id: DrawRecordId,
    pub version: DocumentVersionId,
}

pub type DrawRecordId = u64;

#[derive(Debug)]
struct StoredImageEditPatch {
    version: DocumentVersionId,
    edits: HashMap<ImageId, ImageEdit>,
}

#[derive(Default, Debug)]
pub struct DrawHistory {
    patches: HashMap<DrawRecordId, StoredImageEditPatch>,
    next_id: DrawRecordId,
}

impl DrawHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_stored_patch(
        &mut self,
        id: DrawRecordId,
        global: &mut GlobalStorage,
    ) -> Result<DrawRecordId, LocalCommitError> {
        let stored = self
            .patches
            .get(&id)
            .ok_or(LocalCommitError::InvalidDrawRecord { id })?;
        if stored.version != global.version() {
            return Err(LocalCommitError::VersionMismatch {
                expected: stored.version,
                actual: global.version(),
            });
        }
        validate_primitive_edits(global, &stored.edits)?;

        let stored = self
            .patches
            .remove(&id)
            .expect("validated history patch must still exist");
        let inverse = apply_primitive_edits(global, stored.edits);
        let version = global.bump_version();
        Ok(self.store_inverse(version, inverse))
    }

    fn store_inverse(
        &mut self,
        version: DocumentVersionId,
        edits: HashMap<ImageId, ImageEdit>,
    ) -> DrawRecordId {
        let id = self.next_id;
        self.next_id += 1;
        self.patches
            .insert(id, StoredImageEditPatch { version, edits });
        id
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawOnWriter {
    pub input_mapping: gla_ir::Mapping,
    pub tool: gla_ir::Tool,
    pub tool_params: gla_ir::ToolParams,
}

impl DrawOnWriter {
    fn from_command(command: &DrawOnCommand) -> Self {
        Self {
            input_mapping: command.input_mapping,
            tool: command.tool,
            tool_params: command.tool_params,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DrawOnInput {
    RadialKernel1D {
        center_x: f32,
        center_y: f32,
        /// Engine-space radius. Primitive execution does not read tool config.
        radius: f32,
        /// Engine-space flow after app input, pressure, preset, and curve mapping.
        flow: f32,
    },
}

#[derive(Debug)]
pub enum SessionImageContent {
    Raw(DenseImage),
    Edit(ImageEdit),
}

impl SessionImageContent {
    pub fn is_raw(&self) -> bool {
        matches!(self, Self::Raw(_))
    }

    pub fn is_edit(&self) -> bool {
        matches!(self, Self::Edit(_))
    }

    fn release_tiles(self, tiles: &mut Tiles) {
        match self {
            Self::Raw(image) => image.release_tiles(tiles),
            Self::Edit(edit) => edit.release_tiles(tiles),
        }
    }
}

#[derive(Clone, Debug)]
pub enum SessionImageWriter {
    DrawOn(DrawOnWriter),
    Derive(ImageDeriveCommand<SessionImageId>),
}

#[derive(Debug)]
pub struct SessionImage {
    format: GlaFormat,
    layout: GlaImageLayout,
    content: SessionImageContent,
    writer: SessionImageWriter,
}

impl SessionImage {
    pub fn format(&self) -> GlaFormat {
        self.format
    }

    pub fn layout(&self) -> GlaImageLayout {
        self.layout
    }

    pub fn content(&self) -> &SessionImageContent {
        &self.content
    }

    pub fn writer(&self) -> &SessionImageWriter {
        &self.writer
    }

    fn release_tiles(self, tiles: &mut Tiles) {
        self.content.release_tiles(tiles);
    }
}

#[derive(Debug)]
pub enum LocalStorageError {
    ExpectedDocumentVersion {
        expected: DocumentVersionId,
        actual: DocumentVersionId,
    },
    DuplicateDocImage {
        id: ImageId,
    },
    MissingGlobalImage {
        id: ImageId,
    },
    ReadWriteRequiresPrimitive {
        id: ImageId,
    },
    DuplicateSessionImage {
        id: ImageId,
    },
    SessionImageConflictsWithReadWriteDoc {
        id: ImageId,
    },
    MissingMetadataRef {
        id: ImageId,
    },
    DuplicateWriter {
        id: ImageId,
    },
    MissingWriter {
        id: ImageId,
    },
    DestinationNotWritable {
        id: ImageId,
    },
    BackupReadRequiresDocImage {
        id: ImageId,
    },
    CurrentReadRequiresDeclaredImage {
        id: ImageId,
    },
    WriterCycle {
        id: ImageId,
    },
    ImageCreate {
        id: ImageId,
        source: ImageError,
    },
}

#[derive(Debug)]
pub enum LocalRenderError {
    MissingLocalImage { id: ImageId },
    MissingGlobalImage { id: ImageId },
    MissingMaterializedTile { id: ImageId },
    DestinationNotWritable { id: ImageId },
    GlobalPrimitiveWrite { id: ImageId },
    Image { id: ImageId, source: ImageError },
    Tile { id: ImageId, source: TilesError },
}

impl Display for LocalRenderError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingLocalImage { id } => write!(f, "local image {id:?} is not declared"),
            Self::MissingGlobalImage { id } => write!(f, "global image {id:?} is not declared"),
            Self::MissingMaterializedTile { id } => {
                write!(f, "image {id:?} did not materialize a tile")
            }
            Self::DestinationNotWritable { id } => {
                write!(
                    f,
                    "image {id:?} is not writable in the current render context"
                )
            }
            Self::GlobalPrimitiveWrite { id } => {
                write!(
                    f,
                    "global primitive image {id:?} cannot be written by render"
                )
            }
            Self::Image { id, source } => write!(f, "image {id:?} access failed: {source}"),
            Self::Tile { id, source } => write!(f, "tile access for image {id:?} failed: {source}"),
        }
    }
}

impl Error for LocalRenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Image { source, .. } => Some(source),
            Self::Tile { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum LocalCommitError {
    ExpectedDocumentVersion {
        expected: DocumentVersionId,
        actual: DocumentVersionId,
    },
    VersionMismatch {
        expected: DocumentVersionId,
        actual: DocumentVersionId,
    },
    InvalidDrawRecord {
        id: DrawRecordId,
    },
    MissingGlobalImage {
        id: ImageId,
    },
    DestinationNotWritable {
        id: ImageId,
    },
    InvalidEditTile {
        id: ImageId,
        tile_index: u32,
    },
}

impl Display for LocalCommitError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExpectedDocumentVersion { expected, actual } => write!(
                f,
                "session expected document version {expected:?}, but storage is at {actual:?}"
            ),
            Self::VersionMismatch { expected, actual } => {
                write!(
                    f,
                    "patch expected version {expected:?}, but storage is at {actual:?}"
                )
            }
            Self::InvalidDrawRecord { id } => write!(f, "draw record {id} does not exist"),
            Self::MissingGlobalImage { id } => write!(f, "global image {id:?} is not declared"),
            Self::DestinationNotWritable { id } => {
                write!(f, "image {id:?} is not a writable commit target")
            }
            Self::InvalidEditTile { id, tile_index } => {
                write!(f, "edit tile {tile_index} is invalid for image {id:?}")
            }
        }
    }
}

impl Error for LocalCommitError {}

impl Display for LocalStorageError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExpectedDocumentVersion { expected, actual } => write!(
                f,
                "session expected document version {expected:?}, but storage is at {actual:?}"
            ),
            Self::DuplicateDocImage { id } => write!(f, "doc image {id:?} is declared twice"),
            Self::MissingGlobalImage { id } => write!(f, "global image {id:?} is not declared"),
            Self::ReadWriteRequiresPrimitive { id } => {
                write!(f, "ReadWrite doc image {id:?} must be primitive")
            }
            Self::DuplicateSessionImage { id } => {
                write!(f, "session image {id:?} is declared twice")
            }
            Self::SessionImageConflictsWithReadWriteDoc { id } => {
                write!(
                    f,
                    "session image {id:?} conflicts with a ReadWrite doc image"
                )
            }
            Self::MissingMetadataRef { id } => {
                write!(f, "metadata reference {id:?} does not resolve")
            }
            Self::DuplicateWriter { id } => write!(f, "image {id:?} has multiple writers"),
            Self::MissingWriter { id } => write!(f, "session image {id:?} has no writer"),
            Self::DestinationNotWritable { id } => {
                write!(f, "image {id:?} is not a writable session destination")
            }
            Self::BackupReadRequiresDocImage { id } => {
                write!(f, "backup read {id:?} must reference a declared doc image")
            }
            Self::CurrentReadRequiresDeclaredImage { id } => {
                write!(
                    f,
                    "current read {id:?} must reference a declared doc or session image"
                )
            }
            Self::WriterCycle { id } => write!(f, "session writer graph has a cycle at {id:?}"),
            Self::ImageCreate { id, source } => {
                write!(f, "failed to create local image {id:?}: {source}")
            }
        }
    }
}

impl Error for LocalStorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ImageCreate { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct LocalStorage {
    expected_document_version: DocumentVersionId,
    doc_write_ids: HashSet<ImageId>,
    draw_on_order: Vec<ImageId>,
    frame_dirty: HashMap<ImageId, TileSet>,
    doc_dirty: HashMap<ImageId, TileSet>,
    images: HashMap<ImageId, SessionImage>,
}

#[derive(Clone, Copy, Debug)]
struct DirtyEdge {
    src: ImageId,
    dst: ImageId,
    mapping: Mapping,
    modifier: FootprintModifier,
}

impl LocalStorage {
    pub fn build(
        ir: &DrawSessionIR,
        global: &mut GlobalStorage,
    ) -> Result<Self, LocalStorageError> {
        if ir.expected_document_version != global.version() {
            return Err(LocalStorageError::ExpectedDocumentVersion {
                expected: ir.expected_document_version,
                actual: global.version(),
            });
        }
        let doc_access = collect_doc_access(ir, global)?;
        let doc_write_ids = doc_access
            .iter()
            .filter_map(|(id, access)| (*access == DocumentImageAccess::ReadWrite).then_some(*id))
            .collect();
        let draw_on_order = ir.draw_on.iter().map(|command| command.dst).collect();
        let session_specs = resolve_session_specs(ir, global, &doc_access)?;
        let writers = collect_writers(ir)?;
        let mut plans = build_plans(&doc_access, &session_specs, writers, global)?;
        activate_global_derived_chain(&mut plans, &session_specs, global)?;
        validate_writer_cycles(&plans)?;
        let images = allocate_plans(plans, global)?;
        Ok(Self {
            expected_document_version: ir.expected_document_version,
            doc_write_ids,
            draw_on_order,
            frame_dirty: HashMap::new(),
            doc_dirty: HashMap::new(),
            images,
        })
    }

    pub fn expected_document_version(&self) -> DocumentVersionId {
        self.expected_document_version
    }

    pub fn image(&self, id: ImageId) -> Option<&SessionImage> {
        self.images.get(&id)
    }

    pub fn images(&self) -> &HashMap<ImageId, SessionImage> {
        &self.images
    }

    pub fn doc_dirty(&self) -> &HashMap<ImageId, TileSet> {
        &self.doc_dirty
    }

    pub fn into_images(self) -> HashMap<ImageId, SessionImage> {
        self.images
    }

    pub fn render_ctx<'a>(&'a mut self, global: &'a mut GlobalStorage) -> LocalRenderCtx<'a> {
        LocalRenderCtx {
            local: self,
            global,
        }
    }

    pub fn draw_dab(
        &mut self,
        global: &mut GlobalStorage,
        input: CanvasInput,
    ) -> Result<(), LocalRenderError> {
        self.draw_input(global, input)
    }

    fn draw_input(
        &mut self,
        global: &mut GlobalStorage,
        input: CanvasInput,
    ) -> Result<(), LocalRenderError> {
        let draws = self
            .draw_on_order
            .iter()
            .copied()
            .map(|id| {
                let image = self
                    .images
                    .get(&id)
                    .ok_or(LocalRenderError::MissingLocalImage { id })?;
                let SessionImageWriter::DrawOn(writer) = image.writer() else {
                    return Err(LocalRenderError::DestinationNotWritable { id });
                };
                Ok((
                    id,
                    *writer,
                    image.layout(),
                    draw_on_input_from_canvas(*writer, input),
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut ctx = self.render_ctx(global);
        for (id, writer, layout, input) in draws {
            draw_on(&mut ctx, id, writer, layout, input)?;
        }
        Ok(())
    }

    pub fn flush_frame(&mut self, global: &mut GlobalStorage) -> Result<(), LocalRenderError> {
        if self.frame_dirty.values().all(TileSet::is_empty) {
            return Ok(());
        }

        let frame_dirty = std::mem::take(&mut self.frame_dirty);
        let mut render_demand = HashMap::new();
        for (id, dirty) in frame_dirty {
            if !dirty.is_empty() {
                self.upload_dirty_from(id, &dirty, global, &mut render_demand)?;
            }
        }

        self.render_terminal_demand(global, render_demand)
    }

    pub fn commit(
        mut self,
        global: &mut GlobalStorage,
        history: &mut DrawHistory,
    ) -> Result<DrawCommit, LocalCommitError> {
        if self.expected_document_version != global.version() {
            let expected = self.expected_document_version;
            let actual = global.version();
            self.release_tiles(global.tiles_mut());
            return Err(LocalCommitError::ExpectedDocumentVersion { expected, actual });
        }

        if let Err(error) = self.validate_commit_edits(global) {
            self.release_tiles(global.tiles_mut());
            return Err(error);
        }

        let inverse = self.apply_commit_edits(global);
        let version = global.bump_version();
        let record_id = history.store_inverse(version, inverse);
        self.release_tiles(global.tiles_mut());
        Ok(DrawCommit { record_id, version })
    }

    pub fn discard(self, global: &mut GlobalStorage) {
        self.release_tiles(global.tiles_mut());
    }

    fn release_tiles(self, tiles: &mut Tiles) {
        for (_, image) in self.images {
            image.release_tiles(tiles);
        }
    }

    fn record_frame_dirty(&mut self, id: ImageId, tile_index: u32) {
        self.frame_dirty.entry(id).or_default().insert(tile_index);
    }

    fn record_doc_dirty(&mut self, id: ImageId, dirty: &TileSet) {
        if self.doc_write_ids.contains(&id) {
            self.doc_dirty.entry(id).or_default().union_assign(dirty);
        }
    }

    fn upload_dirty_from(
        &mut self,
        id: ImageId,
        dirty: &TileSet,
        global: &GlobalStorage,
        render_demand: &mut HashMap<ImageId, TileSet>,
    ) -> Result<(), LocalRenderError> {
        self.record_doc_dirty(id, dirty);
        if self.is_local_derive(id) {
            render_demand.entry(id).or_default().union_assign(dirty);
        }

        for edge in self.dirty_edges_from(id) {
            let projected = self.project_dirty_edge(dirty, edge, global)?;
            if !projected.is_empty() {
                self.upload_dirty_from(edge.dst, &projected, global, render_demand)?;
            }
        }

        Ok(())
    }

    fn is_local_derive(&self, id: ImageId) -> bool {
        matches!(
            self.images.get(&id).map(SessionImage::writer),
            Some(SessionImageWriter::Derive(_))
        )
    }

    fn render_terminal_demand(
        &mut self,
        global: &mut GlobalStorage,
        demand: HashMap<ImageId, TileSet>,
    ) -> Result<(), LocalRenderError> {
        let terminals = demand
            .iter()
            .filter_map(|(id, dirty)| {
                (!dirty.is_empty() && !self.has_demand_successor(*id, &demand))
                    .then(|| (*id, dirty.clone()))
            })
            .collect::<Vec<_>>();

        let mut ctx = self.render_ctx(global);
        for (id, dirty) in terminals {
            let layout = ctx
                .local
                .images
                .get(&id)
                .ok_or(LocalRenderError::MissingLocalImage { id })?
                .layout();
            match dirty {
                TileSet::Full => {
                    for tile_index in 0..layout.tile_count() {
                        ctx.render(SessionImageId::Current(id), tile_index)?;
                    }
                }
                TileSet::Tiles(tiles) => {
                    for tile_index in tiles {
                        if tile_index < layout.tile_count() {
                            ctx.render(SessionImageId::Current(id), tile_index)?;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn has_demand_successor(&self, id: ImageId, demand: &HashMap<ImageId, TileSet>) -> bool {
        self.dirty_edges_from(id)
            .into_iter()
            .any(|edge| demand.contains_key(&edge.dst))
    }

    fn dirty_edges_from(&self, src: ImageId) -> Vec<DirtyEdge> {
        let mut edges = Vec::new();
        for image in self.images.values() {
            let SessionImageWriter::Derive(command) = image.writer() else {
                continue;
            };
            let SessionImageId::Current(dst) = command.dst else {
                continue;
            };
            for op in command.ops.iter().copied() {
                let Some(read) = derive_image_ref(op) else {
                    continue;
                };
                if read.key == SessionImageId::Current(src) {
                    edges.push(DirtyEdge {
                        src,
                        dst,
                        mapping: read.mapping,
                        modifier: read.modifier,
                    });
                }
            }
        }
        edges
    }

    fn project_dirty_edge(
        &self,
        src_dirty: &TileSet,
        edge: DirtyEdge,
        global: &GlobalStorage,
    ) -> Result<TileSet, LocalRenderError> {
        if matches!(
            (edge.mapping, edge.modifier),
            (Mapping::Identity, FootprintModifier::None)
        ) && self.layout_of_id(edge.src, global)? == self.layout_of_id(edge.dst, global)?
        {
            return Ok(src_dirty.clone());
        }

        match (edge.mapping, edge.modifier) {
            (Mapping::Identity, FootprintModifier::None) => {
                let dst_tile_count = self.layout_of_id(edge.dst, global)?.tile_count();
                let mut projected = TileSet::default();
                match src_dirty {
                    TileSet::Full => Ok(TileSet::Full),
                    TileSet::Tiles(tiles) => {
                        for tile_index in tiles.iter().copied() {
                            if tile_index < dst_tile_count {
                                projected.insert(tile_index);
                            }
                        }
                        Ok(projected)
                    }
                }
            }
            (Mapping::Identity, FootprintModifier::Expand(_)) | (Mapping::Matrix(_), _) => {
                Ok(TileSet::Full)
            }
        }
    }

    fn layout_of_id(
        &self,
        id: ImageId,
        global: &GlobalStorage,
    ) -> Result<GlaImageLayout, LocalRenderError> {
        self.images
            .get(&id)
            .map(SessionImage::layout)
            .or_else(|| global.image(id).map(GlobalImage::layout))
            .ok_or(LocalRenderError::MissingGlobalImage { id })
    }

    fn validate_commit_edits(&self, global: &GlobalStorage) -> Result<(), LocalCommitError> {
        for (id, session_image) in &self.images {
            let SessionImageContent::Edit(edit) = session_image.content() else {
                continue;
            };
            if edit.is_empty() {
                continue;
            }
            match global
                .image(*id)
                .ok_or(LocalCommitError::MissingGlobalImage { id: *id })?
            {
                GlobalImage::Primitive(image) => validate_dense_edit(*id, image, edit)?,
                GlobalImage::Derived { image, .. } => validate_cache_edit(*id, image, edit)?,
            }
        }
        Ok(())
    }

    fn apply_commit_edits(&mut self, global: &mut GlobalStorage) -> HashMap<ImageId, ImageEdit> {
        let mut inverse = HashMap::new();
        let (images, tiles, _) = global.resources_mut();
        for (id, session_image) in &mut self.images {
            let SessionImageContent::Edit(edit) = &mut session_image.content else {
                continue;
            };
            if edit.is_empty() {
                continue;
            }
            let edit = edit.take();
            match images
                .get_mut(id)
                .expect("commit edits were validated against global storage")
            {
                GlobalImage::Primitive(image) => {
                    let old = apply_dense_edit(image, edit);
                    if !old.is_empty() {
                        inverse.insert(*id, old);
                    }
                }
                GlobalImage::Derived { image, .. } => {
                    let old = apply_cache_edit(image, edit);
                    old.release_tiles(tiles);
                }
            }
        }
        inverse
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DabTile {
    index: u32,
    origin_x: u32,
    origin_y: u32,
}

fn draw_on(
    ctx: &mut LocalRenderCtx<'_>,
    id: ImageId,
    writer: DrawOnWriter,
    layout: GlaImageLayout,
    input: DrawOnInput,
) -> Result<(), LocalRenderError> {
    match (writer.tool, input) {
        (
            Tool::RadialKernel1D,
            DrawOnInput::RadialKernel1D {
                center_x,
                center_y,
                radius,
                flow,
            },
        ) => draw_radial_kernel_1d(ctx, id, layout, center_x, center_y, radius, flow),
    }
}

fn draw_on_input_from_canvas(writer: DrawOnWriter, input: CanvasInput) -> DrawOnInput {
    match writer.tool {
        Tool::RadialKernel1D => {
            let (center_x, center_y) =
                map_input_to_dst(writer.input_mapping, input.position.x, input.position.y);
            // Temporary fallback mapper until the app/tool layer owns custom curves.
            DrawOnInput::RadialKernel1D {
                center_x,
                center_y,
                radius: non_negative_finite(writer.tool_params.radius).max(1.0),
                flow: input.pressure,
            }
        }
    }
}

fn draw_radial_kernel_1d(
    ctx: &mut LocalRenderCtx<'_>,
    id: ImageId,
    layout: GlaImageLayout,
    center_x: f32,
    center_y: f32,
    radius: f32,
    flow: f32,
) -> Result<(), LocalRenderError> {
    let radius = non_negative_finite(radius);
    let flow = finite_or_zero(flow);

    for tile in radial_footprint_tiles(layout, center_x, center_y, radius) {
        let dst = ctx.draw_on_write_pos(id, tile.index)?;
        let center_in_tile_x = center_x - tile.origin_x as f32;
        let center_in_tile_y = center_y - tile.origin_y as f32;
        ctx.renderer()
            .draw_radial_kernel_1d(dst, center_in_tile_x, center_in_tile_y, radius, flow);
        ctx.renderer().fix_gutter(dst);
    }

    Ok(())
}

fn map_input_to_dst(mapping: Mapping, x: f32, y: f32) -> (f32, f32) {
    let x = finite_or_zero(x);
    let y = finite_or_zero(y);
    match mapping {
        Mapping::Identity => (x, y),
        Mapping::Matrix(m) => (m.m11 * x + m.m12 * y + m.tx, m.m21 * x + m.m22 * y + m.ty),
    }
}

fn radial_footprint_tiles(
    layout: GlaImageLayout,
    center_x: f32,
    center_y: f32,
    radius: f32,
) -> Vec<DabTile> {
    if layout.tile_count() == 0 || !footprint_intersects_layout(layout, center_x, center_y, radius)
    {
        return Vec::new();
    }

    let min_tx = tile_coord_for_px(center_x - radius, layout.width_px, layout.tile_count_x());
    let max_tx = tile_coord_for_px(center_x + radius, layout.width_px, layout.tile_count_x());
    let min_ty = tile_coord_for_px(center_y - radius, layout.height_px, layout.tile_count_y());
    let max_ty = tile_coord_for_px(center_y + radius, layout.height_px, layout.tile_count_y());
    let tile_count_x = layout.tile_count_x();
    let mut tiles = Vec::new();

    for ty in min_ty..=max_ty {
        for tx in min_tx..=max_tx {
            tiles.push(DabTile {
                index: ty * tile_count_x + tx,
                origin_x: tx * IMAGE_TILE_SIZE,
                origin_y: ty * IMAGE_TILE_SIZE,
            });
        }
    }

    tiles
}

fn footprint_intersects_layout(
    layout: GlaImageLayout,
    center_x: f32,
    center_y: f32,
    radius: f32,
) -> bool {
    let max_x = layout.width_px as f32;
    let max_y = layout.height_px as f32;
    center_x + radius >= 0.0
        && center_y + radius >= 0.0
        && center_x - radius < max_x
        && center_y - radius < max_y
}

fn tile_coord_for_px(px: f32, extent_px: u32, tile_count: u32) -> u32 {
    debug_assert!(tile_count > 0);
    let max_px = extent_px.saturating_sub(1) as f32;
    let clamped = finite_or_zero(px).max(0.0).min(max_px);
    ((clamped / IMAGE_TILE_SIZE as f32).floor() as u32).min(tile_count - 1)
}

fn non_negative_finite(value: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        0.0
    }
}

fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

pub struct LocalRenderCtx<'a> {
    local: &'a mut LocalStorage,
    global: &'a mut GlobalStorage,
}

impl LocalRenderCtx<'_> {
    pub fn draw_on_write_pos(
        &mut self,
        id: ImageId,
        tile_index: u32,
    ) -> Result<TilePos, LocalRenderError> {
        let first_edit_write = {
            let image = self
                .local
                .images
                .get(&id)
                .ok_or(LocalRenderError::MissingLocalImage { id })?;
            if !matches!(image.writer(), SessionImageWriter::DrawOn(_)) {
                return Err(LocalRenderError::DestinationNotWritable { id });
            }
            match image.content() {
                SessionImageContent::Raw(_) => false,
                SessionImageContent::Edit(edit) => edit.tile(tile_index).is_none(),
            }
        };

        let dst = self.write_current(id, tile_index)?;

        if first_edit_write {
            match self.read_global(id, tile_index)? {
                TileReadRef::Zero => self.global.renderer_mut().clear(dst),
                TileReadRef::Physical(src) => self.global.renderer_mut().copy(src, dst),
            }
        }

        self.local.record_frame_dirty(id, tile_index);
        Ok(dst)
    }

    fn render_image(
        &mut self,
        image: SessionImageId,
        tile_index: u32,
    ) -> Result<TileReadRef, LocalRenderError> {
        match image {
            SessionImageId::Current(id) if self.local.images.contains_key(&id) => {
                self.render_local(id, tile_index)
            }
            SessionImageId::Current(id) | SessionImageId::Global(id) => {
                self.render_global(id, tile_index)
            }
        }
    }

    fn render_local(
        &mut self,
        id: ImageId,
        tile_index: u32,
    ) -> Result<TileReadRef, LocalRenderError> {
        let command = match self
            .local
            .images
            .get(&id)
            .ok_or(LocalRenderError::MissingLocalImage { id })?
            .writer()
        {
            SessionImageWriter::DrawOn(_) => None,
            SessionImageWriter::Derive(command) => Some(command.clone()),
        };

        if let Some(command) = command {
            command.exec_tile(self, tile_index)?;
        }

        self.read_local(id, tile_index)
    }

    fn read_local(
        &mut self,
        id: ImageId,
        tile_index: u32,
    ) -> Result<TileReadRef, LocalRenderError> {
        let image = self
            .local
            .images
            .get(&id)
            .ok_or(LocalRenderError::MissingLocalImage { id })?;
        match image.content() {
            SessionImageContent::Raw(raw) => {
                let tile = raw
                    .tile(tile_index)
                    .map_err(|source| LocalRenderError::Image { id, source })?;
                self.global
                    .tiles()
                    .read_ref(tile)
                    .map_err(|source| LocalRenderError::Tile { id, source })
            }
            SessionImageContent::Edit(edit) => {
                if let Some(tile) = edit.tile(tile_index) {
                    self.global
                        .tiles()
                        .read_ref(tile)
                        .map_err(|source| LocalRenderError::Tile { id, source })
                } else {
                    self.render_global(id, tile_index)
                }
            }
        }
    }

    fn render_global(
        &mut self,
        id: ImageId,
        tile_index: u32,
    ) -> Result<TileReadRef, LocalRenderError> {
        let command = {
            let image = self
                .global
                .image(id)
                .ok_or(LocalRenderError::MissingGlobalImage { id })?;
            match image {
                GlobalImage::Primitive(_) => None,
                GlobalImage::Derived { command, image }
                    if image
                        .tile(tile_index)
                        .map_err(|source| LocalRenderError::Image { id, source })?
                        .is_none() =>
                {
                    Some(lower_global_command(
                        command,
                        id,
                        image.layout(),
                        self.global,
                    )?)
                }
                GlobalImage::Derived { .. } => None,
            }
        };

        if let Some(command) = command {
            command.exec_tile(self, tile_index)?;
        }

        self.read_global(id, tile_index)
    }

    fn read_global(
        &mut self,
        id: ImageId,
        tile_index: u32,
    ) -> Result<TileReadRef, LocalRenderError> {
        let image = self
            .global
            .image(id)
            .ok_or(LocalRenderError::MissingGlobalImage { id })?;
        match image {
            GlobalImage::Primitive(image) => {
                let tile = image
                    .tile(tile_index)
                    .map_err(|source| LocalRenderError::Image { id, source })?;
                self.global
                    .tiles()
                    .read_ref(tile)
                    .map_err(|source| LocalRenderError::Tile { id, source })
            }
            GlobalImage::Derived { image, .. } => {
                let tile = image
                    .tile(tile_index)
                    .map_err(|source| LocalRenderError::Image { id, source })?
                    .ok_or(LocalRenderError::MissingMaterializedTile { id })?;
                self.global
                    .tiles()
                    .read_ref(tile)
                    .map_err(|source| LocalRenderError::Tile { id, source })
            }
        }
    }

    fn write_image(
        &mut self,
        image: SessionImageId,
        tile_index: u32,
    ) -> Result<TilePos, LocalRenderError> {
        match image {
            SessionImageId::Current(id) => self.write_current(id, tile_index),
            SessionImageId::Global(id) => self.write_global(id, tile_index),
        }
    }

    fn write_current(&mut self, id: ImageId, tile_index: u32) -> Result<TilePos, LocalRenderError> {
        if !self.local.images.contains_key(&id) {
            return Err(LocalRenderError::DestinationNotWritable { id });
        }
        let (_, tiles, _) = self.global.resources_mut();
        let image = self
            .local
            .images
            .get_mut(&id)
            .ok_or(LocalRenderError::MissingLocalImage { id })?;
        match &mut image.content {
            SessionImageContent::Raw(raw) => {
                let tile = raw
                    .tile_mut(tile_index)
                    .map_err(|source| LocalRenderError::Image { id, source })?;
                tiles
                    .write_pos(tile)
                    .map_err(|source| LocalRenderError::Tile { id, source })
            }
            SessionImageContent::Edit(edit) => {
                if tile_index >= image.layout.tile_count() {
                    return Err(LocalRenderError::Image {
                        id,
                        source: ImageError::TileIndexOutOfBounds {
                            tile_index,
                            tile_count: image.layout.tile_count(),
                        },
                    });
                }
                let tile = if edit.tile(tile_index).is_some() {
                    edit.tile_mut(tile_index)
                        .expect("checked edit tile must exist")
                } else {
                    let tile = tiles
                        .reserve_for_format(image.format)
                        .map_err(|source| LocalRenderError::Tile { id, source })?;
                    edit.insert_tile(tile_index, tile)
                };
                tiles
                    .write_pos(tile)
                    .map_err(|source| LocalRenderError::Tile { id, source })
            }
        }
    }

    fn write_global(&mut self, id: ImageId, tile_index: u32) -> Result<TilePos, LocalRenderError> {
        let (images, tiles, _) = self.global.resources_mut();
        let image = images
            .get_mut(&id)
            .ok_or(LocalRenderError::MissingGlobalImage { id })?;
        match image {
            GlobalImage::Primitive(_) => Err(LocalRenderError::GlobalPrimitiveWrite { id }),
            GlobalImage::Derived { image, .. } => {
                if image
                    .tile(tile_index)
                    .map_err(|source| LocalRenderError::Image { id, source })?
                    .is_none()
                {
                    let tile = tiles
                        .reserve_for_format(image.format())
                        .map_err(|source| LocalRenderError::Tile { id, source })?;
                    if let Err(error) = image.replace_tile(tile_index, tile) {
                        return Err(release_replace_error(id, tiles, error));
                    }
                }

                let tile = image
                    .tile_mut(tile_index)
                    .map_err(|source| LocalRenderError::Image { id, source })?
                    .expect("global cache tile was materialized before write");
                tiles
                    .write_pos(tile)
                    .map_err(|source| LocalRenderError::Tile { id, source })
            }
        }
    }
}

impl RenderCtx for LocalRenderCtx<'_> {
    type ImageKey = SessionImageId;
    type Error = LocalRenderError;

    fn render(
        &mut self,
        image: Self::ImageKey,
        tile_index: u32,
    ) -> Result<TileReadRef, Self::Error> {
        self.render_image(image, tile_index)
    }

    fn write_pos(
        &mut self,
        image: Self::ImageKey,
        tile_index: u32,
    ) -> Result<TilePos, Self::Error> {
        self.write_image(image, tile_index)
    }

    fn renderer(&mut self) -> &mut gla_renderer::Renderer {
        self.global.renderer_mut()
    }
}

fn release_replace_error(
    id: ImageId,
    tiles: &mut Tiles,
    error: TileReplaceError,
) -> LocalRenderError {
    let (source, tile) = error.into_parts();
    tiles.release(tile);
    LocalRenderError::Image { id, source }
}

#[derive(Clone, Copy, Debug)]
struct LocalImageSpec {
    format: GlaFormat,
    layout: GlaImageLayout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionContentKind {
    Raw,
    Edit,
}

#[derive(Clone, Debug)]
struct SessionImagePlan {
    format: GlaFormat,
    layout: GlaImageLayout,
    content: SessionContentKind,
    writer: SessionImageWriter,
}

#[derive(Clone, Debug)]
enum PendingWriter {
    DrawOn(DrawOnWriter),
    Derive(SessionCommand),
}

fn collect_doc_access(
    ir: &DrawSessionIR,
    global: &GlobalStorage,
) -> Result<HashMap<ImageId, DocumentImageAccess>, LocalStorageError> {
    let mut doc_access = HashMap::new();
    for image_use in &ir.doc_images {
        if doc_access
            .insert(image_use.id, image_use.access.clone())
            .is_some()
        {
            return Err(LocalStorageError::DuplicateDocImage { id: image_use.id });
        }

        let image = global
            .image(image_use.id)
            .ok_or(LocalStorageError::MissingGlobalImage { id: image_use.id })?;
        if image_use.access == DocumentImageAccess::ReadWrite
            && !matches!(image, GlobalImage::Primitive(_))
        {
            return Err(LocalStorageError::ReadWriteRequiresPrimitive { id: image_use.id });
        }
    }
    Ok(doc_access)
}

fn resolve_session_specs(
    ir: &DrawSessionIR,
    global: &GlobalStorage,
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
) -> Result<HashMap<ImageId, LocalImageSpec>, LocalStorageError> {
    let mut session_specs = HashMap::new();
    for decl in &ir.session_images {
        let id = decl.id();
        if session_specs.contains_key(&id) {
            return Err(LocalStorageError::DuplicateSessionImage { id });
        }
        if doc_access.get(&id) == Some(&DocumentImageAccess::ReadWrite) {
            return Err(LocalStorageError::SessionImageConflictsWithReadWriteDoc { id });
        }

        let format_ref = match decl {
            SessionImageDecl::Primitive { format, .. }
            | SessionImageDecl::Derived { format, .. } => format,
        };
        let layout_ref = match decl {
            SessionImageDecl::Primitive { layout, .. }
            | SessionImageDecl::Derived { layout, .. } => layout,
        };
        let format = resolve_format(format_ref, &session_specs, global)?;
        let layout = resolve_layout(layout_ref, &session_specs, global)?;
        session_specs.insert(id, LocalImageSpec { format, layout });
    }
    Ok(session_specs)
}

fn resolve_format(
    format: &MetadataRef<GlaFormat>,
    session_specs: &HashMap<ImageId, LocalImageSpec>,
    global: &GlobalStorage,
) -> Result<GlaFormat, LocalStorageError> {
    match format {
        MetadataRef::Concrete(format) => Ok(*format),
        MetadataRef::Like(id) => session_specs
            .get(id)
            .map(|spec| spec.format)
            .or_else(|| global.image(*id).map(GlobalImage::format))
            .ok_or(LocalStorageError::MissingMetadataRef { id: *id }),
    }
}

fn resolve_layout(
    layout: &MetadataRef<GlaImageLayout>,
    session_specs: &HashMap<ImageId, LocalImageSpec>,
    global: &GlobalStorage,
) -> Result<GlaImageLayout, LocalStorageError> {
    match layout {
        MetadataRef::Concrete(layout) => Ok(*layout),
        MetadataRef::Like(id) => session_specs
            .get(id)
            .map(|spec| spec.layout)
            .or_else(|| global.image(*id).map(GlobalImage::layout))
            .ok_or(LocalStorageError::MissingMetadataRef { id: *id }),
    }
}

fn collect_writers(
    ir: &DrawSessionIR,
) -> Result<HashMap<ImageId, PendingWriter>, LocalStorageError> {
    let mut writers = HashMap::new();

    for decl in &ir.session_images {
        if let SessionImageDecl::Derived { id, command, .. } = decl {
            insert_writer(&mut writers, *id, PendingWriter::Derive(command.clone()))?;
        }
    }
    for command in &ir.draw_on {
        insert_writer(
            &mut writers,
            command.dst,
            PendingWriter::DrawOn(DrawOnWriter::from_command(command)),
        )?;
    }
    for command in &ir.derive {
        insert_writer(
            &mut writers,
            command.dst,
            PendingWriter::Derive(command.command.clone()),
        )?;
    }

    Ok(writers)
}

fn insert_writer(
    writers: &mut HashMap<ImageId, PendingWriter>,
    id: ImageId,
    writer: PendingWriter,
) -> Result<(), LocalStorageError> {
    if writers.insert(id, writer).is_some() {
        return Err(LocalStorageError::DuplicateWriter { id });
    }
    Ok(())
}

fn build_plans(
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    session_specs: &HashMap<ImageId, LocalImageSpec>,
    writers: HashMap<ImageId, PendingWriter>,
    global: &GlobalStorage,
) -> Result<HashMap<ImageId, SessionImagePlan>, LocalStorageError> {
    let mut plans = HashMap::new();

    for (id, pending_writer) in writers {
        let (content, spec) = if let Some(spec) = session_specs.get(&id).copied() {
            (SessionContentKind::Raw, spec)
        } else if doc_access.get(&id) == Some(&DocumentImageAccess::ReadWrite) {
            let image = global
                .image(id)
                .ok_or(LocalStorageError::MissingGlobalImage { id })?;
            if !matches!(image, GlobalImage::Primitive(_)) {
                return Err(LocalStorageError::ReadWriteRequiresPrimitive { id });
            }
            (
                SessionContentKind::Edit,
                LocalImageSpec {
                    format: image.format(),
                    layout: image.layout(),
                },
            )
        } else {
            return Err(LocalStorageError::DestinationNotWritable { id });
        };

        let writer = lower_writer(
            pending_writer,
            id,
            spec.layout,
            doc_access,
            session_specs,
            global,
        )?;
        plans.insert(
            id,
            SessionImagePlan {
                format: spec.format,
                layout: spec.layout,
                content,
                writer,
            },
        );
    }

    for id in session_specs.keys().copied() {
        if !plans.contains_key(&id) {
            return Err(LocalStorageError::MissingWriter { id });
        }
    }

    Ok(plans)
}

fn activate_global_derived_chain(
    plans: &mut HashMap<ImageId, SessionImagePlan>,
    session_specs: &HashMap<ImageId, LocalImageSpec>,
    global: &GlobalStorage,
) -> Result<(), LocalStorageError> {
    let mut frontier: Vec<ImageId> = plans.keys().copied().collect();
    let mut scanned = HashSet::new();

    while let Some(active_id) = frontier.pop() {
        if !scanned.insert(active_id) {
            continue;
        }

        for (id, image) in global.images() {
            if plans.contains_key(id) {
                continue;
            }
            let Some(command) = image.graph_command() else {
                continue;
            };
            if !command.reads.iter().any(|read| read.image == active_id) {
                continue;
            }

            let writer = lower_graph_command(command, *id, image.layout(), session_specs, global)?;
            plans.insert(
                *id,
                SessionImagePlan {
                    format: image.format(),
                    layout: image.layout(),
                    content: SessionContentKind::Edit,
                    writer: SessionImageWriter::Derive(writer),
                },
            );
            frontier.push(*id);
        }
    }

    Ok(())
}

fn lower_writer(
    writer: PendingWriter,
    dst: ImageId,
    dst_layout: GlaImageLayout,
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    session_specs: &HashMap<ImageId, LocalImageSpec>,
    global: &GlobalStorage,
) -> Result<SessionImageWriter, LocalStorageError> {
    match writer {
        PendingWriter::DrawOn(writer) => Ok(SessionImageWriter::DrawOn(writer)),
        PendingWriter::Derive(command) => {
            lower_session_command(command, dst, dst_layout, doc_access, session_specs, global)
                .map(SessionImageWriter::Derive)
        }
    }
}

fn lower_session_command(
    command: SessionCommand,
    dst: ImageId,
    dst_layout: GlaImageLayout,
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    session_specs: &HashMap<ImageId, LocalImageSpec>,
    global: &GlobalStorage,
) -> Result<ImageDeriveCommand<SessionImageId>, LocalStorageError> {
    let mut ops = Vec::with_capacity(command.reads.len());
    for read in command.reads {
        let (key, layout) = match read.image {
            SessionReadImage::Current(id) => {
                if !session_specs.contains_key(&id) && !doc_access.contains_key(&id) {
                    return Err(LocalStorageError::CurrentReadRequiresDeclaredImage { id });
                }
                let layout = image_layout(id, session_specs, global)?;
                (SessionImageId::Current(id), layout)
            }
            SessionReadImage::Backup(id) => {
                if !doc_access.contains_key(&id) {
                    return Err(LocalStorageError::BackupReadRequiresDocImage { id });
                }
                let image = global
                    .image(id)
                    .ok_or(LocalStorageError::MissingGlobalImage { id })?;
                (SessionImageId::Global(id), image.layout())
            }
        };
        ops.push(Derive::Copy(Copy::new(ImageRef::with_footprint(
            key,
            layout,
            read.mapping,
            read.modifier,
        ))));
    }

    Ok(ImageDeriveCommand::new(
        SessionImageId::Current(dst),
        dst_layout,
        ops,
    ))
}

fn lower_graph_command(
    command: &GraphCommand,
    dst: ImageId,
    dst_layout: GlaImageLayout,
    session_specs: &HashMap<ImageId, LocalImageSpec>,
    global: &GlobalStorage,
) -> Result<ImageDeriveCommand<SessionImageId>, LocalStorageError> {
    let mut ops = Vec::with_capacity(command.reads.len());
    for read in &command.reads {
        let layout = image_layout(read.image, session_specs, global)?;
        ops.push(Derive::Copy(Copy::new(ImageRef::with_footprint(
            SessionImageId::Current(read.image),
            layout,
            read.mapping,
            read.modifier,
        ))));
    }

    Ok(ImageDeriveCommand::new(
        SessionImageId::Current(dst),
        dst_layout,
        ops,
    ))
}

fn lower_global_command(
    command: &GraphCommand,
    dst: ImageId,
    dst_layout: GlaImageLayout,
    global: &GlobalStorage,
) -> Result<ImageDeriveCommand<SessionImageId>, LocalRenderError> {
    let mut ops = Vec::with_capacity(command.reads.len());
    for read in &command.reads {
        let image = global
            .image(read.image)
            .ok_or(LocalRenderError::MissingGlobalImage { id: read.image })?;
        ops.push(Derive::Copy(Copy::new(ImageRef::with_footprint(
            SessionImageId::Global(read.image),
            image.layout(),
            read.mapping,
            read.modifier,
        ))));
    }

    Ok(ImageDeriveCommand::new(
        SessionImageId::Global(dst),
        dst_layout,
        ops,
    ))
}

fn image_layout(
    id: ImageId,
    session_specs: &HashMap<ImageId, LocalImageSpec>,
    global: &GlobalStorage,
) -> Result<GlaImageLayout, LocalStorageError> {
    session_specs
        .get(&id)
        .map(|spec| spec.layout)
        .or_else(|| global.image(id).map(GlobalImage::layout))
        .ok_or(LocalStorageError::MissingGlobalImage { id })
}

fn validate_writer_cycles(
    plans: &HashMap<ImageId, SessionImagePlan>,
) -> Result<(), LocalStorageError> {
    let mut visited = HashSet::new();
    let mut visiting = HashSet::new();
    for id in plans.keys().copied() {
        visit_writer(id, plans, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn visit_writer(
    id: ImageId,
    plans: &HashMap<ImageId, SessionImagePlan>,
    visiting: &mut HashSet<ImageId>,
    visited: &mut HashSet<ImageId>,
) -> Result<(), LocalStorageError> {
    if visited.contains(&id) {
        return Ok(());
    }
    if !visiting.insert(id) {
        return Err(LocalStorageError::WriterCycle { id });
    }

    if let Some(SessionImagePlan {
        writer: SessionImageWriter::Derive(command),
        ..
    }) = plans.get(&id)
    {
        for op in command.ops.iter().copied() {
            if let Some(SessionImageId::Current(read_id)) = derive_read(op) {
                if plans.contains_key(&read_id) {
                    visit_writer(read_id, plans, visiting, visited)?;
                }
            }
        }
    }

    visiting.remove(&id);
    visited.insert(id);
    Ok(())
}

fn derive_read(op: Derive<SessionImageId>) -> Option<SessionImageId> {
    derive_image_ref(op).map(|read| read.key)
}

fn derive_image_ref(op: Derive<SessionImageId>) -> Option<ImageRef<SessionImageId>> {
    match op {
        Derive::Copy(op) => Some(op.src),
        Derive::RenderTo(op) => Some(op.src),
        Derive::Clear(_) => None,
    }
}

fn validate_primitive_edits(
    global: &GlobalStorage,
    edits: &HashMap<ImageId, ImageEdit>,
) -> Result<(), LocalCommitError> {
    for (id, edit) in edits {
        let image = global
            .image(*id)
            .ok_or(LocalCommitError::MissingGlobalImage { id: *id })?;
        let GlobalImage::Primitive(image) = image else {
            return Err(LocalCommitError::DestinationNotWritable { id: *id });
        };
        validate_dense_edit(*id, image, edit)?;
    }
    Ok(())
}

fn validate_dense_edit(
    id: ImageId,
    image: &DenseImage,
    edit: &ImageEdit,
) -> Result<(), LocalCommitError> {
    validate_edit_bounds(id, image.tile_count(), edit)
}

fn validate_cache_edit(
    id: ImageId,
    image: &CacheImage,
    edit: &ImageEdit,
) -> Result<(), LocalCommitError> {
    validate_edit_bounds(id, image.tile_count(), edit)
}

fn validate_edit_bounds(
    id: ImageId,
    tile_count: u32,
    edit: &ImageEdit,
) -> Result<(), LocalCommitError> {
    for (tile_index, _) in edit.edits() {
        if *tile_index >= tile_count {
            return Err(LocalCommitError::InvalidEditTile {
                id,
                tile_index: *tile_index,
            });
        }
    }
    Ok(())
}

fn apply_primitive_edits(
    global: &mut GlobalStorage,
    edits: HashMap<ImageId, ImageEdit>,
) -> HashMap<ImageId, ImageEdit> {
    let mut inverse = HashMap::new();
    for (id, edit) in edits {
        let image = global
            .images
            .get_mut(&id)
            .expect("primitive edit patch was validated against global storage");
        let GlobalImage::Primitive(image) = image else {
            panic!("primitive edit patch changed role after validation");
        };
        let old = apply_dense_edit(image, edit);
        if !old.is_empty() {
            inverse.insert(id, old);
        }
    }
    inverse
}

fn apply_dense_edit(image: &mut DenseImage, edit: ImageEdit) -> ImageEdit {
    let mut inverse = Vec::with_capacity(edit.edits().len());
    for (tile_index, new_tile) in edit.into_edits() {
        let old_tile = image
            .replace_tile(tile_index, new_tile)
            .expect("dense edit tile index was validated before apply");
        inverse.push((tile_index, old_tile));
    }
    ImageEdit { edits: inverse }
}

fn apply_cache_edit(image: &mut CacheImage, edit: ImageEdit) -> ImageEdit {
    let mut replaced = Vec::new();
    for (tile_index, new_tile) in edit.into_edits() {
        if let Some(old_tile) = image
            .replace_tile(tile_index, new_tile)
            .expect("cache edit tile index was validated before apply")
        {
            replaced.push((tile_index, old_tile));
        }
    }
    ImageEdit { edits: replaced }
}

fn allocate_plans(
    plans: HashMap<ImageId, SessionImagePlan>,
    global: &mut GlobalStorage,
) -> Result<HashMap<ImageId, SessionImage>, LocalStorageError> {
    let mut images = HashMap::new();
    for (id, plan) in plans {
        let content = match plan.content {
            SessionContentKind::Raw => {
                match DenseImage::allocate(plan.format, plan.layout, global.tiles_mut()) {
                    Ok(image) => SessionImageContent::Raw(image),
                    Err(source) => {
                        release_images(global.tiles_mut(), images);
                        return Err(LocalStorageError::ImageCreate { id, source });
                    }
                }
            }
            SessionContentKind::Edit => SessionImageContent::Edit(ImageEdit::new()),
        };
        images.insert(
            id,
            SessionImage {
                format: plan.format,
                layout: plan.layout,
                content,
                writer: plan.writer,
            },
        );
    }
    Ok(images)
}

fn release_images(tiles: &mut Tiles, images: HashMap<ImageId, SessionImage>) {
    for (_, image) in images {
        image.release_tiles(tiles);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GlobalStorage;
    use atlas::{AtlasLayout, NoAtlasTextures};
    use gla_color::{ChannelCount, ChannelType};
    use gla_core::CanvasCoordF;
    use gla_image_command::RenderCtx;
    use gla_ir::{
        Affine2D, DocImageUse, GraphRead, ImageRole, RegistryPatch, RegistryPatchOp, SessionRead,
        ToolParams,
    };
    use gla_renderer::Pass;
    use gla_renderer::Renderer;
    use tile_key::TileReadRef;

    fn rgba_format() -> GlaFormat {
        GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::U8,
        }
    }

    fn value_format() -> GlaFormat {
        GlaFormat {
            channel_count: ChannelCount::D1,
            channel_type: ChannelType::U8,
        }
    }

    fn layout() -> GlaImageLayout {
        GlaImageLayout::new(1, 1)
    }

    fn multi_tile_layout() -> GlaImageLayout {
        GlaImageLayout::new(IMAGE_TILE_SIZE * 3, IMAGE_TILE_SIZE * 2)
    }

    fn canvas_input(x: f32, y: f32, pressure: f32) -> CanvasInput {
        CanvasInput {
            time_ns: 0,
            position: CanvasCoordF::new(x, y),
            pressure,
            tilt: (0.0, 0.0),
            twist: 0.0,
        }
    }

    fn storage_with_atlases() -> GlobalStorage {
        let mut tiles = Tiles::new();
        let mut textures = NoAtlasTextures;
        tiles
            .new_atlas(AtlasLayout::TINY8, rgba_format(), &mut textures)
            .unwrap();
        tiles
            .new_atlas(AtlasLayout::TINY8, value_format(), &mut textures)
            .unwrap();
        GlobalStorage::new(tiles, Renderer::new())
    }

    fn add_global_primitive(storage: &mut GlobalStorage, id: ImageId, format: GlaFormat) {
        add_global_primitive_with_layout(storage, id, format, layout());
    }

    fn add_global_primitive_with_layout(
        storage: &mut GlobalStorage,
        id: ImageId,
        format: GlaFormat,
        layout: GlaImageLayout,
    ) {
        storage
            .apply_registry_patch(RegistryPatch::new(vec![RegistryPatchOp::NewImage {
                id,
                format,
                layout,
                role: ImageRole::Primitive,
            }]))
            .unwrap();
    }

    fn add_global_derived(storage: &mut GlobalStorage, id: ImageId, reads: Vec<GraphRead>) {
        storage
            .apply_registry_patch(RegistryPatch::new(vec![RegistryPatchOp::NewImage {
                id,
                format: rgba_format(),
                layout: layout(),
                role: ImageRole::Derived(gla_ir::GraphCommand::new(reads)),
            }]))
            .unwrap();
    }

    #[test]
    fn build_pixel_round_style_session_uses_raw_and_edit_content() {
        let base = ImageId::new(1);
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Like(base),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: vec![gla_ir::DeriveCommand::new(
                vec![SessionRead::backup(base), SessionRead::current(coverage)],
                base,
            )],
        };

        let local = LocalStorage::build(&ir, &mut global).unwrap();

        assert_eq!(local.expected_document_version(), Default::default());
        let coverage_image = local.image(coverage).unwrap();
        assert!(coverage_image.content().is_raw());
        assert!(matches!(
            coverage_image.writer(),
            SessionImageWriter::DrawOn(_)
        ));
        let base_image = local.image(base).unwrap();
        assert!(base_image.content().is_edit());
        let SessionImageWriter::Derive(command) = base_image.writer() else {
            panic!("base should be derive writer");
        };
        assert_eq!(command.dst, SessionImageId::Current(base));
        assert_eq!(command.ops.len(), 2);
        assert!(matches!(
            command.ops[0],
            Derive::Copy(Copy {
                src: ImageRef {
                    key: SessionImageId::Global(id),
                    ..
                }
            }) if id == base
        ));
        assert!(matches!(
            command.ops[1],
            Derive::Copy(Copy {
                src: ImageRef {
                    key: SessionImageId::Current(id),
                    ..
                }
            }) if id == coverage
        ));
    }

    #[test]
    fn raw_local_allocation_uses_matching_format_atlas() {
        let base = ImageId::new(1);
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: vec![DocImageUse::read(base)],
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Like(base),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: Vec::new(),
        };

        let local = LocalStorage::build(&ir, &mut global).unwrap();
        let SessionImageContent::Raw(image) = local.image(coverage).unwrap().content() else {
            panic!("coverage should be raw");
        };

        assert_eq!(
            global.tiles().read_ref(image.tile(0).unwrap()).unwrap(),
            TileReadRef::Zero
        );
    }

    #[test]
    fn readwrite_requires_global_primitive() {
        let primitive = ImageId::new(1);
        let derived = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, primitive, rgba_format());
        add_global_derived(&mut global, derived, vec![GraphRead::current(primitive)]);

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: vec![DocImageUse::read_write(derived)],
            session_images: Vec::new(),
            draw_on: vec![DrawOnCommand::new(derived)],
            derive: Vec::new(),
        };

        let err = LocalStorage::build(&ir, &mut global).unwrap_err();

        assert!(matches!(
            err,
            LocalStorageError::ReadWriteRequiresPrimitive { id } if id == derived
        ));
    }

    #[test]
    fn global_derived_dependents_are_activated_conservatively() {
        let base = ImageId::new(1);
        let group = ImageId::new(2);
        let root = ImageId::new(3);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());
        add_global_derived(&mut global, group, vec![GraphRead::current(base)]);
        add_global_derived(&mut global, root, vec![GraphRead::current(group)]);

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: Vec::new(),
            draw_on: vec![DrawOnCommand::new(base)],
            derive: Vec::new(),
        };

        let local = LocalStorage::build(&ir, &mut global).unwrap();

        assert!(local.image(base).unwrap().content().is_edit());
        let group_image = local.image(group).unwrap();
        assert!(group_image.content().is_edit());
        let SessionImageWriter::Derive(command) = group_image.writer() else {
            panic!("group should be active graph derive shadow");
        };
        assert_eq!(command.dst, SessionImageId::Current(group));
        assert!(matches!(
            command.ops[0],
            Derive::Copy(Copy {
                src: ImageRef {
                    key: SessionImageId::Current(id),
                    ..
                }
            }) if id == base
        ));

        let root_image = local.image(root).unwrap();
        assert!(root_image.content().is_edit());
        let SessionImageWriter::Derive(command) = root_image.writer() else {
            panic!("root should be active graph derive shadow");
        };
        assert_eq!(command.dst, SessionImageId::Current(root));
        assert!(matches!(
            command.ops[0],
            Derive::Copy(Copy {
                src: ImageRef {
                    key: SessionImageId::Current(id),
                    ..
                }
            }) if id == group
        ));
    }

    #[test]
    fn render_current_raw_drawon_returns_zero_content() {
        let base = ImageId::new(1);
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: vec![DocImageUse::read(base)],
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Like(base),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: Vec::new(),
        };
        let mut local = LocalStorage::build(&ir, &mut global).unwrap();

        let rendered = {
            let mut ctx = local.render_ctx(&mut global);
            ctx.render(SessionImageId::Current(coverage), 0).unwrap()
        };

        assert_eq!(rendered, TileReadRef::Zero);
        assert!(global.renderer().passes().is_empty());
    }

    #[test]
    fn render_current_edit_derive_materializes_replacement_tile() {
        let base = ImageId::new(1);
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Like(base),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: vec![gla_ir::DeriveCommand::new(
                vec![SessionRead::backup(base), SessionRead::current(coverage)],
                base,
            )],
        };
        let mut local = LocalStorage::build(&ir, &mut global).unwrap();

        let rendered = {
            let mut ctx = local.render_ctx(&mut global);
            ctx.render(SessionImageId::Current(base), 0).unwrap()
        };
        let TileReadRef::Physical(dst) = rendered else {
            panic!("derive render should materialize a physical destination");
        };

        assert_eq!(
            global.renderer().passes(),
            &[
                Pass::Clear { dst },
                Pass::Clear { dst },
                Pass::FixGutter { dst },
            ]
        );
        let SessionImageContent::Edit(edit) = local.image(base).unwrap().content() else {
            panic!("base should be an edit shadow");
        };
        assert_eq!(edit.edits().len(), 1);
    }

    #[test]
    fn render_global_derived_repairs_cache_miss() {
        let base = ImageId::new(1);
        let group = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());
        add_global_derived(&mut global, group, vec![GraphRead::current(base)]);

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: vec![DocImageUse::read(base)],
            session_images: Vec::new(),
            draw_on: Vec::new(),
            derive: Vec::new(),
        };
        let mut local = LocalStorage::build(&ir, &mut global).unwrap();

        let rendered = {
            let mut ctx = local.render_ctx(&mut global);
            ctx.render(SessionImageId::Global(group), 0).unwrap()
        };
        let TileReadRef::Physical(dst) = rendered else {
            panic!("global cache repair should materialize a physical destination");
        };

        assert_eq!(
            global.renderer().passes(),
            &[Pass::Clear { dst }, Pass::FixGutter { dst }]
        );
        assert!(
            global
                .image(group)
                .unwrap()
                .as_cache()
                .unwrap()
                .tile(0)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn draw_on_edit_first_write_copies_global_source_once() {
        let base = ImageId::new(1);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: Vec::new(),
            draw_on: vec![DrawOnCommand::new(base)],
            derive: Vec::new(),
        };
        let mut local = LocalStorage::build(&ir, &mut global).unwrap();

        let first = {
            let mut ctx = local.render_ctx(&mut global);
            ctx.draw_on_write_pos(base, 0).unwrap()
        };
        assert_eq!(global.renderer().passes(), &[Pass::Clear { dst: first }]);
        let SessionImageContent::Edit(edit) = local.image(base).unwrap().content() else {
            panic!("base should be an edit shadow");
        };
        assert_eq!(edit.edits().len(), 1);

        global.renderer_mut().clear_passes();
        let second = {
            let mut ctx = local.render_ctx(&mut global);
            ctx.draw_on_write_pos(base, 0).unwrap()
        };

        assert_eq!(second, first);
        assert!(global.renderer().passes().is_empty());
        let SessionImageContent::Edit(edit) = local.image(base).unwrap().content() else {
            panic!("base should still be an edit shadow");
        };
        assert_eq!(edit.edits().len(), 1);
    }

    #[test]
    fn draw_on_raw_write_does_not_emit_source_copy() {
        let base = ImageId::new(1);
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: vec![DocImageUse::read(base)],
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Like(base),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: Vec::new(),
        };
        let mut local = LocalStorage::build(&ir, &mut global).unwrap();

        let _dst = {
            let mut ctx = local.render_ctx(&mut global);
            ctx.draw_on_write_pos(coverage, 0).unwrap()
        };

        assert!(global.renderer().passes().is_empty());
    }

    #[test]
    fn draw_dab_applies_affine_input_mapping_to_touched_tile() {
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        let mut draw = DrawOnCommand::new(coverage);
        draw.input_mapping = Mapping::Matrix(Affine2D {
            m11: 1.0,
            m12: 0.0,
            m21: 0.0,
            m22: 1.0,
            tx: IMAGE_TILE_SIZE as f32 + 4.0,
            ty: IMAGE_TILE_SIZE as f32 + 4.0,
        });

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Concrete(multi_tile_layout()),
            }],
            draw_on: vec![draw],
            derive: Vec::new(),
        };
        let mut local = LocalStorage::build(&ir, &mut global).unwrap();

        local
            .draw_dab(&mut global, canvas_input(0.0, 0.0, 0.5))
            .unwrap();

        let passes = global.renderer().passes();
        assert_eq!(passes.len(), 2);
        assert!(matches!(
            passes[0],
            Pass::DrawRadialKernel1D {
                dst,
                center_in_tile_x,
                center_in_tile_y,
                radius,
                flow: _,
                ..
            } if center_in_tile_x == 4.0
                && center_in_tile_y == 4.0
                && radius == 1.0
                && matches!(passes[1], Pass::FixGutter { dst: gutter_dst } if gutter_dst == dst)
        ));
    }

    #[test]
    fn draw_dab_fallback_mapper_radius_footprint_touches_each_overlapped_tile() {
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        let mut draw = DrawOnCommand::new(coverage);
        draw.tool_params = ToolParams { radius: 2.0 };

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Concrete(multi_tile_layout()),
            }],
            draw_on: vec![draw],
            derive: Vec::new(),
        };
        let mut local = LocalStorage::build(&ir, &mut global).unwrap();

        local
            .draw_dab(&mut global, canvas_input(IMAGE_TILE_SIZE as f32, 4.0, 0.25))
            .unwrap();

        let centers = global
            .renderer()
            .passes()
            .iter()
            .filter_map(|pass| match *pass {
                Pass::DrawRadialKernel1D {
                    center_in_tile_x,
                    center_in_tile_y,
                    radius,
                    flow: _,
                    ..
                } => {
                    assert_eq!(radius, 2.0);
                    Some((center_in_tile_x, center_in_tile_y))
                }
                Pass::FixGutter { .. } => None,
                _ => {
                    panic!("draw dab should emit brush and gutter passes only for local raw images")
                }
            })
            .collect::<Vec<_>>();
        let gutter_count = global
            .renderer()
            .passes()
            .iter()
            .filter(|pass| matches!(pass, Pass::FixGutter { .. }))
            .count();

        assert_eq!(centers, vec![(IMAGE_TILE_SIZE as f32, 4.0), (0.0, 4.0)]);
        assert_eq!(gutter_count, 2);
    }

    #[test]
    fn draw_dab_tile_local_center_can_be_outside_touched_tile() {
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        let mut draw = DrawOnCommand::new(coverage);
        draw.tool_params = ToolParams { radius: 2.0 };

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Concrete(multi_tile_layout()),
            }],
            draw_on: vec![draw],
            derive: Vec::new(),
        };
        let mut local = LocalStorage::build(&ir, &mut global).unwrap();

        local
            .draw_dab(
                &mut global,
                canvas_input(IMAGE_TILE_SIZE as f32 - 1.0, 4.0, 0.25),
            )
            .unwrap();

        let centers = global
            .renderer()
            .passes()
            .iter()
            .map(|pass| match *pass {
                Pass::DrawRadialKernel1D {
                    center_in_tile_x,
                    center_in_tile_y,
                    radius: _,
                    flow: _,
                    ..
                } => Some((center_in_tile_x, center_in_tile_y)),
                Pass::FixGutter { .. } => None,
                _ => {
                    panic!("draw dab should emit brush and gutter passes only for local raw images")
                }
            })
            .flatten()
            .collect::<Vec<_>>();

        assert_eq!(
            centers,
            vec![(IMAGE_TILE_SIZE as f32 - 1.0, 4.0), (-1.0, 4.0)]
        );
    }

    #[test]
    fn primitive_execution_uses_engine_radius_not_writer_config() {
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        let mut draw = DrawOnCommand::new(coverage);
        draw.tool_params = ToolParams { radius: 40.0 };

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Concrete(multi_tile_layout()),
            }],
            draw_on: vec![draw],
            derive: Vec::new(),
        };
        let mut local = LocalStorage::build(&ir, &mut global).unwrap();
        let writer = *match local.image(coverage).unwrap().writer() {
            SessionImageWriter::DrawOn(writer) => writer,
            SessionImageWriter::Derive(_) => panic!("coverage should be a DrawOn target"),
        };

        {
            let mut ctx = local.render_ctx(&mut global);
            draw_on(
                &mut ctx,
                coverage,
                writer,
                multi_tile_layout(),
                DrawOnInput::RadialKernel1D {
                    center_x: IMAGE_TILE_SIZE as f32,
                    center_y: 4.0,
                    radius: 2.0,
                    flow: 0.5,
                },
            )
            .unwrap();
        }

        let radii = global
            .renderer()
            .passes()
            .iter()
            .filter_map(|pass| match *pass {
                Pass::DrawRadialKernel1D { radius, .. } => Some(radius),
                Pass::FixGutter { .. } => None,
                _ => panic!("primitive execution should emit brush and gutter passes"),
            })
            .collect::<Vec<_>>();

        assert_eq!(radii, vec![2.0, 2.0]);
    }

    #[test]
    fn draw_dab_edit_first_write_copy_precedes_brush_pass() {
        let base = ImageId::new(1);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: Vec::new(),
            draw_on: vec![DrawOnCommand::new(base)],
            derive: Vec::new(),
        };
        let mut local = LocalStorage::build(&ir, &mut global).unwrap();

        local
            .draw_dab(&mut global, canvas_input(0.0, 0.0, 0.4))
            .unwrap();

        let passes = global.renderer().passes();
        assert_eq!(passes.len(), 3);
        let Pass::Clear { dst } = passes[0] else {
            panic!("first edit write must copy or clear the source before brush mutation");
        };
        assert!(matches!(
            passes[1],
            Pass::DrawRadialKernel1D {
                dst: brush_dst,
                center_in_tile_x: 0.0,
                center_in_tile_y: 0.0,
                radius: 1.0,
                flow: _,
                ..
            } if brush_dst == dst
        ));
        assert!(matches!(passes[2], Pass::FixGutter { dst: gutter_dst } if gutter_dst == dst));
    }

    #[test]
    fn draw_dab_flush_frame_uploads_dirty_to_downstream_derive() {
        let base = ImageId::new(1);
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Like(base),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: vec![gla_ir::DeriveCommand::new(
                vec![SessionRead::backup(base), SessionRead::current(coverage)],
                base,
            )],
        };
        let mut local = LocalStorage::build(&ir, &mut global).unwrap();

        local
            .draw_dab(&mut global, canvas_input(0.0, 0.0, 0.6))
            .unwrap();
        local.flush_frame(&mut global).unwrap();

        assert_eq!(local.doc_dirty().get(&base), Some(&TileSet::single(0)));
        assert!(matches!(
            global.renderer().passes()[0],
            Pass::DrawRadialKernel1D { .. }
        ));
        assert!(
            global
                .renderer()
                .passes()
                .iter()
                .any(|pass| matches!(pass, Pass::FixGutter { .. }))
        );
    }

    #[test]
    fn flush_frame_uploads_draw_on_dirty_and_renders_terminal_derive() {
        let base = ImageId::new(1);
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Like(base),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: vec![gla_ir::DeriveCommand::new(
                vec![SessionRead::backup(base), SessionRead::current(coverage)],
                base,
            )],
        };
        let mut local = LocalStorage::build(&ir, &mut global).unwrap();

        let coverage_pos = {
            let mut ctx = local.render_ctx(&mut global);
            ctx.draw_on_write_pos(coverage, 0).unwrap()
        };
        global.renderer_mut().clear(coverage_pos);

        local.flush_frame(&mut global).unwrap();

        let base_pos = {
            let SessionImageContent::Edit(edit) = local.image(base).unwrap().content() else {
                panic!("base should be an edit shadow");
            };
            let (_, tile) = &edit.edits()[0];
            let TileReadRef::Physical(pos) = global.tiles().read_ref(tile).unwrap() else {
                panic!("flushed derive should materialize base");
            };
            pos
        };
        assert_eq!(local.doc_dirty().get(&base), Some(&TileSet::single(0)));
        assert_eq!(
            global.renderer().passes(),
            &[
                Pass::Clear { dst: coverage_pos },
                Pass::Clear { dst: base_pos },
                Pass::Copy {
                    src: coverage_pos,
                    dst: base_pos,
                },
                Pass::FixGutter { dst: base_pos },
            ]
        );
    }

    #[test]
    fn commit_applies_primitive_edit_and_stores_inverse_history() {
        let base = ImageId::new(1);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: Vec::new(),
            draw_on: vec![DrawOnCommand::new(base)],
            derive: Vec::new(),
        };
        let mut local = LocalStorage::build(&ir, &mut global).unwrap();
        let edited_pos = {
            let mut ctx = local.render_ctx(&mut global);
            ctx.draw_on_write_pos(base, 0).unwrap()
        };
        let mut history = DrawHistory::new();

        let commit = local.commit(&mut global, &mut history).unwrap();

        assert_eq!(commit.version, DocumentVersionId::new(1));
        assert_eq!(global.version(), DocumentVersionId::new(1));
        let image = global.image(base).unwrap().as_dense().unwrap();
        assert_eq!(
            global.tiles().read_ref(image.tile(0).unwrap()).unwrap(),
            TileReadRef::Physical(edited_pos)
        );
        let stored = history.patches.get(&commit.record_id).unwrap();
        assert_eq!(stored.version, DocumentVersionId::new(1));
        assert_eq!(stored.edits.get(&base).unwrap().edits().len(), 1);
    }

    #[test]
    fn commit_publishes_derived_cache_without_history_entry() {
        let base = ImageId::new(1);
        let group = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());
        add_global_derived(&mut global, group, vec![GraphRead::current(base)]);

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: Vec::new(),
            draw_on: vec![DrawOnCommand::new(base)],
            derive: Vec::new(),
        };
        let mut local = LocalStorage::build(&ir, &mut global).unwrap();
        let base_pos = {
            let mut ctx = local.render_ctx(&mut global);
            ctx.draw_on_write_pos(base, 0).unwrap()
        };
        global.renderer_mut().clear(base_pos);
        local.flush_frame(&mut global).unwrap();
        let mut history = DrawHistory::new();

        let commit = local.commit(&mut global, &mut history).unwrap();

        assert!(
            global
                .image(group)
                .unwrap()
                .as_cache()
                .unwrap()
                .tile(0)
                .unwrap()
                .is_some()
        );
        let stored = history.patches.get(&commit.record_id).unwrap();
        assert!(stored.edits.contains_key(&base));
        assert!(!stored.edits.contains_key(&group));
    }

    #[test]
    fn history_apply_patch_consumes_record_and_returns_redo_record() {
        let base = ImageId::new(1);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: Vec::new(),
            draw_on: vec![DrawOnCommand::new(base)],
            derive: Vec::new(),
        };
        let mut local = LocalStorage::build(&ir, &mut global).unwrap();
        let edited_pos = {
            let mut ctx = local.render_ctx(&mut global);
            ctx.draw_on_write_pos(base, 0).unwrap()
        };
        let mut history = DrawHistory::new();
        let commit = local.commit(&mut global, &mut history).unwrap();

        let undo_record = history
            .apply_stored_patch(commit.record_id, &mut global)
            .unwrap();

        assert_eq!(global.version(), DocumentVersionId::new(2));
        assert!(!history.patches.contains_key(&commit.record_id));
        let image = global.image(base).unwrap().as_dense().unwrap();
        assert_eq!(
            global.tiles().read_ref(image.tile(0).unwrap()).unwrap(),
            TileReadRef::Zero
        );

        let redo_record = history
            .apply_stored_patch(undo_record, &mut global)
            .unwrap();

        assert_eq!(global.version(), DocumentVersionId::new(3));
        assert!(!history.patches.contains_key(&undo_record));
        assert!(history.patches.contains_key(&redo_record));
        let image = global.image(base).unwrap().as_dense().unwrap();
        assert_eq!(
            global.tiles().read_ref(image.tile(0).unwrap()).unwrap(),
            TileReadRef::Physical(edited_pos)
        );
    }

    #[test]
    fn local_build_rejects_stale_expected_storage_version() {
        let mut global = storage_with_atlases();
        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: Vec::new(),
            session_images: Vec::new(),
            draw_on: Vec::new(),
            derive: Vec::new(),
        };
        let local = LocalStorage::build(&ir, &mut global).unwrap();
        let mut history = DrawHistory::new();
        local.commit(&mut global, &mut history).unwrap();

        let err = LocalStorage::build(&ir, &mut global).unwrap_err();

        assert!(matches!(
            err,
            LocalStorageError::ExpectedDocumentVersion {
                expected,
                actual
            } if expected == DocumentVersionId::new(0)
                && actual == DocumentVersionId::new(1)
        ));
    }

    #[test]
    fn duplicate_writer_is_rejected_before_allocation() {
        let base = ImageId::new(1);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: Vec::new(),
            draw_on: vec![DrawOnCommand::new(base)],
            derive: vec![gla_ir::DeriveCommand::new(
                vec![SessionRead::backup(base)],
                base,
            )],
        };

        let err = LocalStorage::build(&ir, &mut global).unwrap_err();

        assert!(matches!(
            err,
            LocalStorageError::DuplicateWriter { id } if id == base
        ));
    }

    #[test]
    fn derive_current_reads_must_be_declared() {
        let base = ImageId::new(1);
        let missing = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: Vec::new(),
            draw_on: Vec::new(),
            derive: vec![gla_ir::DeriveCommand::new(
                vec![SessionRead::current(missing)],
                base,
            )],
        };

        let err = LocalStorage::build(&ir, &mut global).unwrap_err();

        assert!(matches!(
            err,
            LocalStorageError::CurrentReadRequiresDeclaredImage { id } if id == missing
        ));
    }

    #[test]
    fn session_writer_cycles_are_rejected() {
        let a = ImageId::new(1);
        let b = ImageId::new(2);
        let mut global = storage_with_atlases();

        let ir = DrawSessionIR {
            expected_document_version: Default::default(),
            doc_images: Vec::new(),
            session_images: vec![
                SessionImageDecl::Derived {
                    id: a,
                    format: MetadataRef::Concrete(value_format()),
                    layout: MetadataRef::Concrete(layout()),
                    command: SessionCommand::new(vec![SessionRead::current(b)]),
                },
                SessionImageDecl::Derived {
                    id: b,
                    format: MetadataRef::Concrete(value_format()),
                    layout: MetadataRef::Concrete(layout()),
                    command: SessionCommand::new(vec![SessionRead::current(a)]),
                },
            ],
            draw_on: Vec::new(),
            derive: Vec::new(),
        };

        let err = LocalStorage::build(&ir, &mut global).unwrap_err();

        assert!(matches!(err, LocalStorageError::WriterCycle { .. }));
    }
}
