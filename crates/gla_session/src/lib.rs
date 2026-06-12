use atlas::TilePos;
use gla_color::{BlendMode, ChannelCount, ChannelType, GlaFormat};
use gla_core::{CanvasInput, TileGridError, tile_rect, tiles_covering_rect};
use gla_image::{
    DenseImage, GlaImageLayout, IMAGE_TILE_SIZE, ImageError, ImageLayoutError, TileSet,
};
use gla_image_command::{
    Copy, Derive, DeriveCommand as ImageDeriveCommand, ImageRef, RenderCtx, SourceFootprintError,
};
use gla_ir::{
    DocumentImageAccess, DocumentVersionId, DrawOnCommand, DrawOnInput, DrawOnInputKind,
    DrawOnToolKind, DrawSessionIR, FootprintModifier, GraphCommand, ImageId, Mapping, MetadataRef,
    SessionCommand, SessionImageDecl, SessionReadImage,
};
use gla_renderer::{Pass, RenderBackend};
use gla_storage::{GlobalEditError, GlobalImage, GlobalStorage, GlobalTileError, ImageEdit};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use tile_key::{TileReadRef, Tiles, TilesError};

mod frame;

pub use frame::FrameBudget;

const CANVAS_DAB_COMPAT_RADIUS_PX: f32 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrawCommit {
    pub record_id: DrawRecordId,
    pub version: DocumentVersionId,
}

pub type DrawRecordId = u64;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawOnRoute {
    pub target: ImageId,
    pub tool: DrawOnToolKind,
    pub target_x: f32,
    pub target_y: f32,
}

#[derive(Debug)]
struct StoredImageEditPatch {
    version: DocumentVersionId,
    edits: HashMap<ImageId, ImageEdit>,
    dirty: HashMap<ImageId, TileSet>,
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
        backend: &mut impl RenderBackend,
    ) -> Result<DrawRecordId, SessionError> {
        let stored = self
            .patches
            .get(&id)
            .ok_or(SessionError::InvalidDrawRecord { id })?;
        if stored.version != global.version() {
            return Err(SessionError::VersionMismatch {
                expected: stored.version,
                actual: global.version(),
            });
        }
        global.validate_primitive_edits(&stored.edits)?;

        let stored = self
            .patches
            .remove(&id)
            .expect("validated history patch must still exist");
        let version = stored.version;
        let dirty = stored.dirty;
        let mut session = match build_stored_patch_session(global, version, stored.edits) {
            Ok(session) => session,
            Err((source, edits)) => {
                self.restore_patch(
                    id,
                    StoredImageEditPatch {
                        version,
                        edits,
                        dirty,
                    },
                );
                return Err(source);
            }
        };
        let flush = {
            let mut frame = DrawFrame::from_dirty(&mut session, dirty.clone());
            frame.flush(backend)
        };
        if let Err(error) = flush {
            let edits = session.take_doc_edits();
            self.restore_patch(
                id,
                StoredImageEditPatch {
                    version,
                    edits,
                    dirty,
                },
            );
            return Err(error);
        }

        let commit = session
            .commit(self)?
            .expect("stored patch session must produce a commit");
        Ok(commit.record_id)
    }

    fn store_inverse(
        &mut self,
        version: DocumentVersionId,
        edits: HashMap<ImageId, ImageEdit>,
    ) -> DrawRecordId {
        let id = self.next_id;
        self.next_id += 1;
        let dirty = dirty_from_edits(&edits);
        self.patches.insert(
            id,
            StoredImageEditPatch {
                version,
                edits,
                dirty,
            },
        );
        id
    }

    fn restore_patch(&mut self, id: DrawRecordId, patch: StoredImageEditPatch) {
        debug_assert!(self.patches.insert(id, patch).is_none());
    }
}

#[derive(Debug)]
pub struct DrawFrame<'s, 'g> {
    session: &'s mut DrawSession<'g>,
    frame_dirty: HashMap<ImageId, TileSet>,
    dab_passes: Vec<Pass>,
    pending_flush_passes: Option<Vec<Pass>>,
}

impl<'s, 'g> DrawFrame<'s, 'g> {
    fn new(session: &'s mut DrawSession<'g>) -> Self {
        Self {
            session,
            frame_dirty: HashMap::new(),
            dab_passes: Vec::new(),
            pending_flush_passes: None,
        }
    }

    fn from_dirty(
        session: &'s mut DrawSession<'g>,
        frame_dirty: HashMap<ImageId, TileSet>,
    ) -> Self {
        Self {
            session,
            frame_dirty,
            dab_passes: Vec::new(),
            pending_flush_passes: None,
        }
    }

    pub fn is_clean(&self) -> bool {
        self.frame_dirty.values().all(TileSet::is_empty)
            && self.dab_passes.is_empty()
            && self.pending_flush_passes.is_none()
    }

    pub fn dab_passes(&self) -> &[Pass] {
        &self.dab_passes
    }

    pub fn route_draw_targets(
        &self,
        shown_image: ImageId,
        x: f32,
        y: f32,
    ) -> Result<Vec<DrawOnRoute>, SessionError> {
        self.session.route_draw_targets(shown_image, x, y)
    }

    pub fn draw_on(&mut self, target: ImageId, input: DrawOnInput) -> Result<(), SessionError> {
        if self.pending_flush_passes.is_some() {
            return Err(SessionError::PendingFrameSubmit);
        }
        self.session
            .draw_on_into_frame(&mut self.frame_dirty, &mut self.dab_passes, target, input)
    }

    pub fn draw_dab(
        &mut self,
        shown_image: ImageId,
        input: CanvasInput,
    ) -> Result<(), SessionError> {
        if self.pending_flush_passes.is_some() {
            return Err(SessionError::PendingFrameSubmit);
        }
        let routes = self.route_draw_targets(shown_image, input.position.x, input.position.y)?;
        let draws = routes
            .into_iter()
            .map(|route| {
                compat_draw_on_input_from_route(route, input).map(|input| (route.target, input))
            })
            .collect::<Result<Vec<_>, SessionError>>()?;
        for (target, input) in draws {
            self.draw_on(target, input)?;
        }
        Ok(())
    }

    pub fn flush<B: RenderBackend>(&mut self, backend: &mut B) -> Result<(), SessionError> {
        if self.is_clean() {
            return Ok(());
        }

        if self.pending_flush_passes.is_none() {
            let frame_dirty = self.frame_dirty.clone();
            let mut passes = self.dab_passes.clone();
            self.session.flush_frame_dirty(&frame_dirty, &mut passes)?;
            self.pending_flush_passes = Some(passes);
        }

        let passes = self
            .pending_flush_passes
            .as_ref()
            .expect("dirty frame must have pending flush passes before submit");
        backend
            .submit(passes)
            .map_err(|source| SessionError::RenderBackend {
                source: Box::new(source),
            })?;
        self.frame_dirty.clear();
        self.dab_passes.clear();
        self.pending_flush_passes = None;
        Ok(())
    }
}

impl Drop for DrawFrame<'_, '_> {
    fn drop(&mut self) {}
}

#[derive(Debug)]
pub enum SessionError {
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
    DrawOnFormatMismatch {
        id: ImageId,
        tool: DrawOnToolKind,
        format: GlaFormat,
    },
    DrawOnInputMismatch {
        id: ImageId,
        tool: DrawOnToolKind,
        input: DrawOnToolKind,
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
    MissingLocalImage {
        id: ImageId,
    },
    InputImageNotActive {
        id: ImageId,
    },
    AmbiguousInputRoute {
        shown: ImageId,
        target: ImageId,
    },
    MissingMaterializedTile {
        id: ImageId,
    },
    GlobalPrimitiveWrite {
        id: ImageId,
    },
    InvalidEditTile {
        id: ImageId,
        tile_index: u32,
    },
    Image {
        id: ImageId,
        source: ImageError,
    },
    Tile {
        id: ImageId,
        source: TilesError,
    },
    TileFootprint {
        source: TileGridError,
    },
    GpuRenderer(gla_renderer::GpuRendererError),
    RenderBackend {
        source: Box<dyn Error + 'static>,
    },
    PendingFrameSubmit,
    ImageCommandFootprint {
        source: SourceFootprintError,
    },
    UnsupportedZeroSourceRenderTo {
        blend_mode: BlendMode,
    },
}

impl Display for SessionError {
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
            Self::DrawOnFormatMismatch { id, tool, format } => {
                write!(
                    f,
                    "DrawOn target {id:?} with format {format:?} is not supported by {tool:?}"
                )
            }
            Self::DrawOnInputMismatch { id, tool, input } => {
                write!(
                    f,
                    "DrawOn target {id:?} uses {tool:?}, but received {input:?} input"
                )
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
            Self::MissingLocalImage { id } => write!(f, "local image {id:?} is not declared"),
            Self::InputImageNotActive { id } => {
                write!(f, "input image {id:?} is not active in this session")
            }
            Self::AmbiguousInputRoute { shown, target } => write!(
                f,
                "input image {shown:?} reaches draw target {target:?} through multiple routes"
            ),
            Self::MissingMaterializedTile { id } => {
                write!(f, "image {id:?} did not materialize a tile")
            }
            Self::GlobalPrimitiveWrite { id } => {
                write!(
                    f,
                    "global primitive image {id:?} cannot be written by render"
                )
            }
            Self::InvalidEditTile { id, tile_index } => {
                write!(f, "edit tile {tile_index} is invalid for image {id:?}")
            }
            Self::Image { id, source } => write!(f, "image {id:?} access failed: {source}"),
            Self::Tile { id, source } => write!(f, "tile access for image {id:?} failed: {source}"),
            Self::TileFootprint { source } => write!(f, "tile footprint failed: {source}"),
            Self::GpuRenderer(source) => write!(f, "GPU renderer execution failed: {source}"),
            Self::RenderBackend { source } => write!(f, "render backend submit failed: {source}"),
            Self::PendingFrameSubmit => {
                write!(f, "frame has pending passes from a failed submit")
            }
            Self::ImageCommandFootprint { source } => {
                write!(f, "image command footprint failed: {source}")
            }
            Self::UnsupportedZeroSourceRenderTo { blend_mode } => {
                write!(
                    f,
                    "RenderTo with zero source is unsupported for {blend_mode:?}"
                )
            }
        }
    }
}

impl Error for SessionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Image { source, .. } => Some(source),
            Self::Tile { source, .. } => Some(source),
            Self::TileFootprint { source } => Some(source),
            Self::GpuRenderer(source) => Some(source),
            Self::RenderBackend { source } => Some(source.as_ref()),
            Self::ImageCommandFootprint { source } => Some(source),
            _ => None,
        }
    }
}

impl From<gla_renderer::GpuRendererError> for SessionError {
    fn from(source: gla_renderer::GpuRendererError) -> Self {
        Self::GpuRenderer(source)
    }
}

impl From<GlobalTileError> for SessionError {
    fn from(error: GlobalTileError) -> Self {
        match error {
            GlobalTileError::MissingImage { id } => Self::MissingGlobalImage { id },
            GlobalTileError::MissingMaterializedTile { id } => Self::MissingMaterializedTile { id },
            GlobalTileError::GlobalPrimitiveWrite { id } => Self::GlobalPrimitiveWrite { id },
            GlobalTileError::Image { id, source } => Self::Image { id, source },
            GlobalTileError::Tile { id, source } => Self::Tile { id, source },
        }
    }
}

impl From<GlobalEditError> for SessionError {
    fn from(error: GlobalEditError) -> Self {
        match error {
            GlobalEditError::MissingImage { id } => Self::MissingGlobalImage { id },
            GlobalEditError::DestinationNotWritable { id } => Self::DestinationNotWritable { id },
            GlobalEditError::InvalidEditTile { id, tile_index } => {
                Self::InvalidEditTile { id, tile_index }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum SessionImageId {
    Current(ImageId),
    Global(ImageId),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DrawOnWriter {
    tool: DrawOnToolKind,
}

impl DrawOnWriter {
    fn from_command(command: &DrawOnCommand) -> Self {
        Self { tool: command.tool }
    }
}

#[derive(Debug)]
enum SessionImageContent {
    Raw(DenseImage),
    Edit(ImageEdit),
}

impl SessionImageContent {
    #[cfg(test)]
    fn is_raw(&self) -> bool {
        matches!(self, Self::Raw(_))
    }

    #[cfg(test)]
    fn is_edit(&self) -> bool {
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
enum SessionImageWriter {
    DrawOn(DrawOnWriter),
    Patch,
    Derive(ImageDeriveCommand<SessionImageId>),
}

#[derive(Debug)]
struct SessionImage {
    format: GlaFormat,
    layout: GlaImageLayout,
    content: SessionImageContent,
    writer: SessionImageWriter,
}

impl SessionImage {
    fn layout(&self) -> GlaImageLayout {
        self.layout
    }

    fn content(&self) -> &SessionImageContent {
        &self.content
    }

    fn writer(&self) -> &SessionImageWriter {
        &self.writer
    }

    fn release_tiles(self, tiles: &mut Tiles) {
        self.content.release_tiles(tiles);
    }
}

pub struct DrawSession<'g> {
    global: &'g mut GlobalStorage,
    expected_document_version: DocumentVersionId,
    doc_write_ids: HashSet<ImageId>,
    draw_on_order: Vec<ImageId>,
    doc_dirty: HashMap<ImageId, TileSet>,
    images: HashMap<ImageId, SessionImage>,
}

impl std::fmt::Debug for DrawSession<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DrawSession")
            .field("expected_document_version", &self.expected_document_version)
            .field("doc_write_ids", &self.doc_write_ids)
            .field("draw_on_order", &self.draw_on_order)
            .field("doc_dirty", &self.doc_dirty)
            .field("images", &self.images)
            .finish_non_exhaustive()
    }
}

impl<'g> DrawSession<'g> {
    pub fn begin(ir: &DrawSessionIR, global: &'g mut GlobalStorage) -> Result<Self, SessionError> {
        if ir.expected_document_version != global.version() {
            return Err(SessionError::ExpectedDocumentVersion {
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
        let mut pending_images = build_images(&doc_access, &session_specs, writers, global)?;
        activate_global_derived_chain(&mut pending_images, &session_specs, global)?;
        validate_writer_cycles(&pending_images)?;
        let images = allocate_images(pending_images, global)?;
        Ok(Self {
            global,
            expected_document_version: ir.expected_document_version,
            doc_write_ids,
            draw_on_order,
            doc_dirty: HashMap::new(),
            images,
        })
    }

    pub fn expected_document_version(&self) -> DocumentVersionId {
        self.expected_document_version
    }

    pub fn doc_dirty(&self) -> &HashMap<ImageId, TileSet> {
        &self.doc_dirty
    }

    pub fn begin_frame(&mut self) -> DrawFrame<'_, 'g> {
        DrawFrame::new(self)
    }

    fn draw_on_into_frame(
        &mut self,
        frame_dirty: &mut HashMap<ImageId, TileSet>,
        dab_passes: &mut Vec<Pass>,
        target: ImageId,
        input: DrawOnInput,
    ) -> Result<(), SessionError> {
        let image = self
            .images
            .get(&target)
            .ok_or(SessionError::MissingLocalImage { id: target })?;
        let SessionImageWriter::DrawOn(writer) = image.writer() else {
            return Err(SessionError::DestinationNotWritable { id: target });
        };
        let writer = *writer;
        let layout = image.layout();

        let mut ctx = self.render_ctx(dab_passes, Some(frame_dirty));
        draw_on(&mut ctx, target, writer, layout, input)
    }

    fn route_draw_targets(
        &self,
        shown_image: ImageId,
        x: f32,
        y: f32,
    ) -> Result<Vec<DrawOnRoute>, SessionError> {
        if !self.images.contains_key(&shown_image) {
            return Err(SessionError::InputImageNotActive { id: shown_image });
        }

        let mut targets = HashMap::new();
        let mut stack = vec![(shown_image, finite_or_zero(x), finite_or_zero(y))];
        while let Some((id, x, y)) = stack.pop() {
            let image = self
                .images
                .get(&id)
                .ok_or(SessionError::MissingLocalImage { id })?;
            match image.writer() {
                SessionImageWriter::DrawOn(_) => {
                    if targets.insert(id, (x, y)).is_some() {
                        return Err(SessionError::AmbiguousInputRoute {
                            shown: shown_image,
                            target: id,
                        });
                    }
                }
                SessionImageWriter::Patch => {}
                SessionImageWriter::Derive(command) => {
                    let SessionImageId::Current(dst) = command.dst else {
                        continue;
                    };
                    for (src, read) in self.current_reads_from_dst(dst) {
                        let (src_x, src_y) = map_point(read.mapping, x, y);
                        stack.push((src, src_x, src_y));
                    }
                }
            }
        }

        let mut draws = Vec::new();
        for id in self.draw_on_order.iter().copied() {
            let Some((x, y)) = targets.remove(&id) else {
                continue;
            };
            let image = self
                .images
                .get(&id)
                .ok_or(SessionError::MissingLocalImage { id })?;
            let SessionImageWriter::DrawOn(writer) = image.writer() else {
                return Err(SessionError::DestinationNotWritable { id });
            };
            draws.push(DrawOnRoute {
                target: id,
                tool: writer.tool,
                target_x: x,
                target_y: y,
            });
        }
        Ok(draws)
    }

    fn flush_frame_dirty(
        &mut self,
        frame_dirty: &HashMap<ImageId, TileSet>,
        passes: &mut Vec<Pass>,
    ) -> Result<(), SessionError> {
        if frame_dirty.values().all(TileSet::is_empty) {
            return Ok(());
        }

        let mut render_demand = HashMap::new();
        for (id, dirty) in frame_dirty {
            if !dirty.is_empty() {
                self.upload_dirty_from(*id, dirty, &mut render_demand)?;
            }
        }

        self.render_terminal_demand(render_demand, passes)
    }

    pub fn commit(mut self, history: &mut DrawHistory) -> Result<Option<DrawCommit>, SessionError> {
        if self.expected_document_version != self.global.version() {
            let expected = self.expected_document_version;
            let actual = self.global.version();
            self.release_local_tiles();
            return Err(SessionError::ExpectedDocumentVersion { expected, actual });
        }

        let edits = self.take_commit_edits();
        if edits.is_empty() {
            self.release_local_tiles();
            return Ok(None);
        }

        match self.global.apply_session_edits(edits) {
            Ok(inverse) => {
                let version = self.global.bump_version();
                let record_id = history.store_inverse(version, inverse);
                self.release_local_tiles();
                Ok(Some(DrawCommit { record_id, version }))
            }
            Err(error) => {
                let (error, edits) = error.into_parts();
                release_image_edits(self.global.tiles_mut(), edits);
                self.release_local_tiles();
                Err(error.into())
            }
        }
    }

    pub fn discard(mut self) {
        self.release_local_tiles();
    }

    fn render_ctx<'a>(
        &'a mut self,
        passes: &'a mut Vec<Pass>,
        frame_dirty: Option<&'a mut HashMap<ImageId, TileSet>>,
    ) -> SessionRenderCtx<'a, 'g> {
        SessionRenderCtx {
            session: self,
            passes,
            frame_dirty,
        }
    }

    fn take_commit_edits(&mut self) -> HashMap<ImageId, ImageEdit> {
        let mut edits = HashMap::new();
        for (id, image) in &mut self.images {
            let SessionImageContent::Edit(edit) = &mut image.content else {
                continue;
            };
            if !edit.is_empty() {
                edits.insert(*id, edit.take());
            }
        }
        edits
    }

    fn take_doc_edits(&mut self) -> HashMap<ImageId, ImageEdit> {
        let mut edits = HashMap::new();
        for id in self.doc_write_ids.iter().copied() {
            let Some(image) = self.images.get_mut(&id) else {
                continue;
            };
            let SessionImageContent::Edit(edit) = &mut image.content else {
                continue;
            };
            if !edit.is_empty() {
                edits.insert(id, edit.take());
            }
        }
        edits
    }

    fn release_local_tiles(&mut self) {
        let images = std::mem::take(&mut self.images);
        release_session_images(self.global.tiles_mut(), images);
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
        render_demand: &mut HashMap<ImageId, TileSet>,
    ) -> Result<(), SessionError> {
        self.record_doc_dirty(id, dirty);
        if self.is_local_derive(id) {
            render_demand.entry(id).or_default().union_assign(dirty);
        }

        for (dst, read) in self.current_reads_to_src(id) {
            let projected = self.project_dirty_read(id, dirty, dst, read)?;
            if !projected.is_empty() {
                self.upload_dirty_from(dst, &projected, render_demand)?;
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
        demand: HashMap<ImageId, TileSet>,
        passes: &mut Vec<Pass>,
    ) -> Result<(), SessionError> {
        let terminals = demand
            .iter()
            .filter_map(|(id, dirty)| {
                (!dirty.is_empty() && !self.has_demand_successor(*id, &demand))
                    .then(|| (*id, dirty.clone()))
            })
            .collect::<Vec<_>>();

        let mut ctx = self.render_ctx(passes, None);
        for (id, dirty) in terminals {
            let layout = ctx
                .session
                .images
                .get(&id)
                .ok_or(SessionError::MissingLocalImage { id })?
                .layout();
            let tile_count = checked_layout_tile_count(id, layout)?;
            match dirty {
                TileSet::Full => {
                    for tile_index in 0..tile_count {
                        ctx.render(SessionImageId::Current(id), tile_index)?;
                    }
                }
                TileSet::Tiles(tiles) => {
                    for tile_index in tiles {
                        if tile_index < tile_count {
                            ctx.render(SessionImageId::Current(id), tile_index)?;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn has_demand_successor(&self, id: ImageId, demand: &HashMap<ImageId, TileSet>) -> bool {
        self.current_reads_to_src(id)
            .into_iter()
            .any(|(dst, _)| demand.contains_key(&dst))
    }

    fn current_reads_from_dst(&self, dst: ImageId) -> Vec<(ImageId, ImageRef<SessionImageId>)> {
        let mut reads = Vec::new();
        self.for_each_current_read(|read_dst, src, read| {
            if read_dst == dst {
                reads.push((src, read));
            }
        });
        reads
    }

    fn current_reads_to_src(&self, src: ImageId) -> Vec<(ImageId, ImageRef<SessionImageId>)> {
        let mut reads = Vec::new();
        self.for_each_current_read(|dst, read_src, read| {
            if read_src == src {
                reads.push((dst, read));
            }
        });
        reads
    }

    fn for_each_current_read(&self, mut f: impl FnMut(ImageId, ImageId, ImageRef<SessionImageId>)) {
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
                let SessionImageId::Current(src) = read.key else {
                    continue;
                };
                f(dst, src, read);
            }
        }
    }

    fn project_dirty_read(
        &self,
        src: ImageId,
        src_dirty: &TileSet,
        dst: ImageId,
        read: ImageRef<SessionImageId>,
    ) -> Result<TileSet, SessionError> {
        if matches!(
            (read.mapping, read.modifier),
            (Mapping::Identity, FootprintModifier::None)
        ) && self.layout_of_id(src)? == self.layout_of_id(dst)?
        {
            return Ok(src_dirty.clone());
        }

        match (read.mapping, read.modifier) {
            (Mapping::Identity, FootprintModifier::None) => {
                let src_layout = self.layout_of_id(src)?;
                let dst_layout = self.layout_of_id(dst)?;
                match src_dirty {
                    TileSet::Full => Ok(TileSet::Full),
                    TileSet::Tiles(tiles) => {
                        let mut projected = TileSet::default();
                        for tile_index in tiles.iter().copied() {
                            let rect = tile_rect(src_layout.tile_grid(), tile_index)
                                .map_err(|source| SessionError::TileFootprint { source })?;
                            for dst_tile_index in tiles_covering_rect(dst_layout.tile_grid(), rect)
                                .map_err(|source| SessionError::TileFootprint { source })?
                            {
                                projected.insert(dst_tile_index);
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

    fn layout_of_id(&self, id: ImageId) -> Result<GlaImageLayout, SessionError> {
        self.images
            .get(&id)
            .map(SessionImage::layout)
            .or_else(|| self.global.image(id).map(GlobalImage::layout))
            .ok_or(SessionError::MissingGlobalImage { id })
    }
}

impl Drop for DrawSession<'_> {
    fn drop(&mut self) {
        self.release_local_tiles();
    }
}

struct SessionRenderCtx<'a, 'g> {
    session: &'a mut DrawSession<'g>,
    passes: &'a mut Vec<Pass>,
    frame_dirty: Option<&'a mut HashMap<ImageId, TileSet>>,
}

impl SessionRenderCtx<'_, '_> {
    fn draw_on_write_pos(&mut self, id: ImageId, tile_index: u32) -> Result<TilePos, SessionError> {
        let first_edit_write = {
            let image = self
                .session
                .images
                .get(&id)
                .ok_or(SessionError::MissingLocalImage { id })?;
            if !matches!(image.writer(), SessionImageWriter::DrawOn(_)) {
                return Err(SessionError::DestinationNotWritable { id });
            }
            match image.content() {
                SessionImageContent::Raw(_) => false,
                SessionImageContent::Edit(edit) => edit.tile(tile_index).is_none(),
            }
        };

        let dst = self.write_current(id, tile_index)?;

        if first_edit_write {
            match self.session.global.read_global_ref(id, tile_index)? {
                TileReadRef::Zero => self.clear(dst),
                TileReadRef::Physical(src) => self.copy(src, dst),
            }
        }

        if let Some(frame_dirty) = self.frame_dirty.as_deref_mut() {
            frame_dirty.entry(id).or_default().insert(tile_index);
        }
        Ok(dst)
    }

    fn render_image(
        &mut self,
        image: SessionImageId,
        tile_index: u32,
    ) -> Result<TileReadRef, SessionError> {
        match image {
            SessionImageId::Current(id) if self.session.images.contains_key(&id) => {
                self.render_local(id, tile_index)
            }
            SessionImageId::Current(id) | SessionImageId::Global(id) => {
                self.render_global(id, tile_index)
            }
        }
    }

    fn render_local(&mut self, id: ImageId, tile_index: u32) -> Result<TileReadRef, SessionError> {
        let command = match self
            .session
            .images
            .get(&id)
            .ok_or(SessionError::MissingLocalImage { id })?
            .writer()
        {
            SessionImageWriter::DrawOn(_) => None,
            SessionImageWriter::Patch => None,
            SessionImageWriter::Derive(command) => Some(command.clone()),
        };

        if let Some(command) = command {
            command.exec_tile(self, tile_index)?;
        }

        self.read_local(id, tile_index)
    }

    fn read_local(&mut self, id: ImageId, tile_index: u32) -> Result<TileReadRef, SessionError> {
        let image = self
            .session
            .images
            .get(&id)
            .ok_or(SessionError::MissingLocalImage { id })?;
        match image.content() {
            SessionImageContent::Raw(raw) => {
                let tile = raw
                    .tile(tile_index)
                    .map_err(|source| SessionError::Image { id, source })?;
                self.session
                    .global
                    .read_tile_ref(tile)
                    .map_err(|source| SessionError::Tile { id, source })
            }
            SessionImageContent::Edit(edit) => {
                if let Some(tile) = edit.tile(tile_index) {
                    self.session
                        .global
                        .read_tile_ref(tile)
                        .map_err(|source| SessionError::Tile { id, source })
                } else {
                    self.render_global(id, tile_index)
                }
            }
        }
    }

    fn render_global(&mut self, id: ImageId, tile_index: u32) -> Result<TileReadRef, SessionError> {
        let command = {
            let image = self
                .session
                .global
                .image(id)
                .ok_or(SessionError::MissingGlobalImage { id })?;
            match image {
                GlobalImage::Primitive(_) => None,
                GlobalImage::Derived { command, image }
                    if image
                        .tile(tile_index)
                        .map_err(|source| SessionError::Image { id, source })?
                        .is_none() =>
                {
                    Some(lower_global_command(
                        command,
                        id,
                        image.layout(),
                        self.session.global,
                    )?)
                }
                GlobalImage::Derived { .. } => None,
            }
        };

        if let Some(command) = command {
            command.exec_tile(self, tile_index)?;
        }

        Ok(self.session.global.read_global_ref(id, tile_index)?)
    }

    fn write_image(
        &mut self,
        image: SessionImageId,
        tile_index: u32,
    ) -> Result<TilePos, SessionError> {
        match image {
            SessionImageId::Current(id) => self.write_current(id, tile_index),
            SessionImageId::Global(id) => {
                let passes = &mut *self.passes;
                Ok(self.session.global.write_global_cache_pos_with_zero_init(
                    id,
                    tile_index,
                    |dst| passes.push(Pass::Clear { dst }),
                )?)
            }
        }
    }

    fn write_current(&mut self, id: ImageId, tile_index: u32) -> Result<TilePos, SessionError> {
        if !self.session.images.contains_key(&id) {
            return Err(SessionError::DestinationNotWritable { id });
        }
        let image = self
            .session
            .images
            .get_mut(&id)
            .ok_or(SessionError::MissingLocalImage { id })?;
        match &mut image.content {
            SessionImageContent::Raw(raw) => {
                let tile = raw
                    .tile_mut(tile_index)
                    .map_err(|source| SessionError::Image { id, source })?;
                let passes = &mut *self.passes;
                self.session
                    .global
                    .write_tile_pos_with_zero_init(tile, |dst| {
                        passes.push(Pass::Clear { dst });
                    })
                    .map_err(|source| SessionError::Tile { id, source })
            }
            SessionImageContent::Edit(edit) => {
                let tile_count = checked_layout_tile_count(id, image.layout)?;
                if tile_index >= tile_count {
                    return Err(SessionError::Image {
                        id,
                        source: ImageError::TileIndexOutOfBounds {
                            tile_index,
                            tile_count,
                        },
                    });
                }
                let tile = if edit.tile(tile_index).is_some() {
                    edit.tile_mut(tile_index)
                        .expect("checked edit tile must exist")
                } else {
                    let tile = self
                        .session
                        .global
                        .reserve_tile_for_format(image.format)
                        .map_err(|source| SessionError::Tile { id, source })?;
                    edit.insert_tile(tile_index, tile)
                };
                let passes = &mut *self.passes;
                self.session
                    .global
                    .write_tile_pos_with_zero_init(tile, |dst| {
                        passes.push(Pass::Clear { dst });
                    })
                    .map_err(|source| SessionError::Tile { id, source })
            }
        }
    }
}

impl RenderCtx for SessionRenderCtx<'_, '_> {
    type ImageKey = SessionImageId;
    type Error = SessionError;

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

    fn clear(&mut self, dst: TilePos) {
        self.passes.push(Pass::Clear { dst });
    }

    fn copy(&mut self, src: TilePos, dst: TilePos) {
        self.passes.push(Pass::Copy { src, dst });
    }

    fn render_to(
        &mut self,
        src: TilePos,
        dst: TilePos,
        blend_mode: gla_color::BlendMode,
        opacity: f32,
    ) {
        self.passes.push(Pass::RenderTo {
            src,
            dst,
            blend_mode,
            opacity,
        });
    }

    fn fix_gutter(&mut self, dst: TilePos) {
        self.passes.push(Pass::FixGutter { dst });
    }

    fn footprint_error(&mut self, source: SourceFootprintError) -> Self::Error {
        SessionError::ImageCommandFootprint { source }
    }

    fn unsupported_zero_source_render_to(&mut self, blend_mode: BlendMode) -> Self::Error {
        SessionError::UnsupportedZeroSourceRenderTo { blend_mode }
    }
}

#[derive(Clone, Copy, Debug)]
struct DabTile {
    index: u32,
    origin_x: u32,
    origin_y: u32,
}

fn draw_on(
    ctx: &mut SessionRenderCtx<'_, '_>,
    id: ImageId,
    writer: DrawOnWriter,
    layout: GlaImageLayout,
    input: DrawOnInput,
) -> Result<(), SessionError> {
    if !input.center_x.is_finite()
        || !input.center_y.is_finite()
        || !input.footprint_radius_px.is_finite()
        || input.footprint_radius_px <= 0.0
    {
        return Ok(());
    }

    match (writer.tool, input.kind) {
        (DrawOnToolKind::RadialKernel1D, DrawOnInputKind::RadialKernel1D { .. }) => {
            draw_radial_kernel_1d(ctx, id, layout, input)
        }
        (DrawOnToolKind::ReplaceCircle4D, DrawOnInputKind::ReplaceCircle4D { .. }) => {
            draw_replace_circle_4d(ctx, id, layout, input)
        }
        (tool, kind) => Err(SessionError::DrawOnInputMismatch {
            id,
            tool,
            input: input_kind_tool(kind),
        }),
    }
}

fn compat_draw_on_input_from_route(
    route: DrawOnRoute,
    input: CanvasInput,
) -> Result<DrawOnInput, SessionError> {
    match route.tool {
        DrawOnToolKind::RadialKernel1D => Ok(DrawOnInput::radial_kernel_1d(
            finite_or_zero(route.target_x),
            finite_or_zero(route.target_y),
            CANVAS_DAB_COMPAT_RADIUS_PX,
            CANVAS_DAB_COMPAT_RADIUS_PX,
            finite_or_zero(input.pressure).clamp(0.0, 1.0),
        )),
        tool => Err(SessionError::DrawOnInputMismatch {
            id: route.target,
            tool,
            input: DrawOnToolKind::RadialKernel1D,
        }),
    }
}

fn input_kind_tool(kind: DrawOnInputKind) -> DrawOnToolKind {
    match kind {
        DrawOnInputKind::RadialKernel1D { .. } => DrawOnToolKind::RadialKernel1D,
        DrawOnInputKind::ReplaceCircle4D { .. } => DrawOnToolKind::ReplaceCircle4D,
    }
}

fn draw_radial_kernel_1d(
    ctx: &mut SessionRenderCtx<'_, '_>,
    id: ImageId,
    layout: GlaImageLayout,
    input: DrawOnInput,
) -> Result<(), SessionError> {
    let DrawOnInput {
        center_x,
        center_y,
        footprint_radius_px,
        kind:
            DrawOnInputKind::RadialKernel1D {
                radius_px,
                amplitude,
            },
    } = input
    else {
        unreachable!("draw_radial_kernel_1d called with non-radial input");
    };

    if !radius_px.is_finite() || radius_px <= 0.0 || !amplitude.is_finite() || amplitude <= 0.0 {
        return Ok(());
    }

    let writes = radial_footprint_tiles(layout, center_x, center_y, footprint_radius_px)
        .map_err(|source| SessionError::Image {
            id,
            source: ImageError::InvalidLayout { source },
        })?
        .into_iter()
        .map(|tile| {
            let dst = ctx.draw_on_write_pos(id, tile.index)?;
            Ok((tile, dst))
        })
        .collect::<Result<Vec<_>, SessionError>>()?;

    for (tile, dst) in writes {
        let center_in_tile_x = center_x - tile.origin_x as f32;
        let center_in_tile_y = center_y - tile.origin_y as f32;
        ctx.passes.push(Pass::DrawRadialKernel1D {
            dst,
            center_in_tile_x,
            center_in_tile_y,
            radius_px,
            amplitude,
        });
        ctx.fix_gutter(dst);
    }

    Ok(())
}

fn draw_replace_circle_4d(
    ctx: &mut SessionRenderCtx<'_, '_>,
    id: ImageId,
    layout: GlaImageLayout,
    input: DrawOnInput,
) -> Result<(), SessionError> {
    let DrawOnInput {
        center_x,
        center_y,
        footprint_radius_px,
        kind: DrawOnInputKind::ReplaceCircle4D { radius_px, color },
    } = input
    else {
        unreachable!("draw_replace_circle_4d called with non-replace input");
    };

    if !radius_px.is_finite() || radius_px <= 0.0 {
        return Ok(());
    }

    let writes = radial_footprint_tiles(layout, center_x, center_y, footprint_radius_px)
        .map_err(|source| SessionError::Image {
            id,
            source: ImageError::InvalidLayout { source },
        })?
        .into_iter()
        .map(|tile| {
            let dst = ctx.draw_on_write_pos(id, tile.index)?;
            Ok((tile, dst))
        })
        .collect::<Result<Vec<_>, SessionError>>()?;

    for (tile, dst) in writes {
        let center_in_tile_x = center_x - tile.origin_x as f32;
        let center_in_tile_y = center_y - tile.origin_y as f32;
        ctx.passes.push(Pass::ReplaceCircle4D {
            dst,
            center_in_tile_x,
            center_in_tile_y,
            radius_px,
            color,
        });
        ctx.fix_gutter(dst);
    }

    Ok(())
}

fn map_point(mapping: Mapping, x: f32, y: f32) -> (f32, f32) {
    let x = finite_or_zero(x);
    let y = finite_or_zero(y);
    let (x, y) = match mapping {
        Mapping::Identity => (x, y),
        Mapping::Matrix(m) => (m.m11 * x + m.m12 * y + m.tx, m.m21 * x + m.m22 * y + m.ty),
    };
    (finite_or_zero(x), finite_or_zero(y))
}

fn radial_footprint_tiles(
    layout: GlaImageLayout,
    center_x: f32,
    center_y: f32,
    radius: f32,
) -> Result<Vec<DabTile>, ImageLayoutError> {
    layout.checked_tile_count()?;
    if !footprint_intersects_layout(layout, center_x, center_y, radius) {
        return Ok(Vec::new());
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

    Ok(tiles)
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

fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

fn checked_layout_tile_count(id: ImageId, layout: GlaImageLayout) -> Result<u32, SessionError> {
    layout
        .checked_tile_count()
        .map_err(|source| SessionError::Image {
            id,
            source: ImageError::InvalidLayout { source },
        })
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
struct PendingSessionImage {
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
) -> Result<HashMap<ImageId, DocumentImageAccess>, SessionError> {
    let mut doc_access = HashMap::new();
    for image_use in &ir.doc_images {
        if doc_access
            .insert(image_use.id, image_use.access.clone())
            .is_some()
        {
            return Err(SessionError::DuplicateDocImage { id: image_use.id });
        }

        let image = global
            .image(image_use.id)
            .ok_or(SessionError::MissingGlobalImage { id: image_use.id })?;
        if image_use.access == DocumentImageAccess::ReadWrite
            && !matches!(image, GlobalImage::Primitive(_))
        {
            return Err(SessionError::ReadWriteRequiresPrimitive { id: image_use.id });
        }
    }
    Ok(doc_access)
}

fn resolve_session_specs(
    ir: &DrawSessionIR,
    global: &GlobalStorage,
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
) -> Result<HashMap<ImageId, LocalImageSpec>, SessionError> {
    let mut session_specs = HashMap::new();
    for decl in &ir.session_images {
        let id = decl.id();
        if session_specs.contains_key(&id) {
            return Err(SessionError::DuplicateSessionImage { id });
        }
        if doc_access.get(&id) == Some(&DocumentImageAccess::ReadWrite) {
            return Err(SessionError::SessionImageConflictsWithReadWriteDoc { id });
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
) -> Result<GlaFormat, SessionError> {
    match format {
        MetadataRef::Concrete(format) => Ok(*format),
        MetadataRef::Like(id) => session_specs
            .get(id)
            .map(|spec| spec.format)
            .or_else(|| global.image(*id).map(GlobalImage::format))
            .ok_or(SessionError::MissingMetadataRef { id: *id }),
    }
}

fn resolve_layout(
    layout: &MetadataRef<GlaImageLayout>,
    session_specs: &HashMap<ImageId, LocalImageSpec>,
    global: &GlobalStorage,
) -> Result<GlaImageLayout, SessionError> {
    match layout {
        MetadataRef::Concrete(layout) => Ok(*layout),
        MetadataRef::Like(id) => session_specs
            .get(id)
            .map(|spec| spec.layout)
            .or_else(|| global.image(*id).map(GlobalImage::layout))
            .ok_or(SessionError::MissingMetadataRef { id: *id }),
    }
}

fn collect_writers(ir: &DrawSessionIR) -> Result<HashMap<ImageId, PendingWriter>, SessionError> {
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
) -> Result<(), SessionError> {
    if writers.insert(id, writer).is_some() {
        return Err(SessionError::DuplicateWriter { id });
    }
    Ok(())
}

fn build_images(
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    session_specs: &HashMap<ImageId, LocalImageSpec>,
    writers: HashMap<ImageId, PendingWriter>,
    global: &GlobalStorage,
) -> Result<HashMap<ImageId, PendingSessionImage>, SessionError> {
    let mut images = HashMap::new();

    for (id, pending_writer) in writers {
        let (content, spec) = if let Some(spec) = session_specs.get(&id).copied() {
            (SessionContentKind::Raw, spec)
        } else if doc_access.get(&id) == Some(&DocumentImageAccess::ReadWrite) {
            let image = global
                .image(id)
                .ok_or(SessionError::MissingGlobalImage { id })?;
            if !matches!(image, GlobalImage::Primitive(_)) {
                return Err(SessionError::ReadWriteRequiresPrimitive { id });
            }
            (
                SessionContentKind::Edit,
                LocalImageSpec {
                    format: image.format(),
                    layout: image.layout(),
                },
            )
        } else {
            return Err(SessionError::DestinationNotWritable { id });
        };

        let writer = lower_writer(
            pending_writer,
            id,
            spec.format,
            spec.layout,
            doc_access,
            session_specs,
            global,
        )?;
        images.insert(
            id,
            PendingSessionImage {
                format: spec.format,
                layout: spec.layout,
                content,
                writer,
            },
        );
    }

    for id in session_specs.keys().copied() {
        if !images.contains_key(&id) {
            return Err(SessionError::MissingWriter { id });
        }
    }

    Ok(images)
}

fn activate_global_derived_chain(
    images: &mut HashMap<ImageId, PendingSessionImage>,
    session_specs: &HashMap<ImageId, LocalImageSpec>,
    global: &GlobalStorage,
) -> Result<(), SessionError> {
    let frontier: Vec<ImageId> = images.keys().copied().collect();
    activate_global_derived_chain_from(frontier, images, session_specs, global)
}

fn activate_global_derived_chain_from(
    mut frontier: Vec<ImageId>,
    images: &mut HashMap<ImageId, PendingSessionImage>,
    session_specs: &HashMap<ImageId, LocalImageSpec>,
    global: &GlobalStorage,
) -> Result<(), SessionError> {
    let mut scanned = HashSet::new();

    while let Some(active_id) = frontier.pop() {
        if !scanned.insert(active_id) {
            continue;
        }

        for (id, image) in global.images() {
            if images.contains_key(id) {
                continue;
            }
            let Some(command) = image.graph_command() else {
                continue;
            };
            if !command.reads.iter().any(|read| read.image == active_id) {
                continue;
            }

            let writer = lower_graph_command(command, *id, image.layout(), session_specs, global)?;
            images.insert(
                *id,
                PendingSessionImage {
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

fn build_stored_patch_session<'g>(
    global: &'g mut GlobalStorage,
    version: DocumentVersionId,
    mut edits: HashMap<ImageId, ImageEdit>,
) -> Result<DrawSession<'g>, (SessionError, HashMap<ImageId, ImageEdit>)> {
    let session_specs = HashMap::new();
    let mut pending = HashMap::new();
    let doc_write_ids = edits.keys().copied().collect::<HashSet<_>>();

    for id in doc_write_ids.iter().copied() {
        let image = match global.image(id) {
            Some(GlobalImage::Primitive(image)) => image,
            Some(GlobalImage::Derived { .. }) => {
                return Err((SessionError::DestinationNotWritable { id }, edits));
            }
            None => return Err((SessionError::MissingGlobalImage { id }, edits)),
        };
        pending.insert(
            id,
            PendingSessionImage {
                format: image.format(),
                layout: image.layout(),
                content: SessionContentKind::Edit,
                writer: SessionImageWriter::Patch,
            },
        );
    }

    let frontier = doc_write_ids.iter().copied().collect();
    if let Err(error) =
        activate_global_derived_chain_from(frontier, &mut pending, &session_specs, global)
    {
        return Err((error, edits));
    }
    if let Err(error) = validate_writer_cycles(&pending) {
        return Err((error, edits));
    }

    let mut images = HashMap::new();
    for (id, image) in pending {
        let content = SessionImageContent::Edit(edits.remove(&id).unwrap_or_default());
        images.insert(
            id,
            SessionImage {
                format: image.format,
                layout: image.layout,
                content,
                writer: image.writer,
            },
        );
    }
    debug_assert!(edits.is_empty());

    Ok(DrawSession {
        global,
        expected_document_version: version,
        doc_write_ids,
        draw_on_order: Vec::new(),
        doc_dirty: HashMap::new(),
        images,
    })
}

fn lower_writer(
    writer: PendingWriter,
    dst: ImageId,
    dst_format: GlaFormat,
    dst_layout: GlaImageLayout,
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    session_specs: &HashMap<ImageId, LocalImageSpec>,
    global: &GlobalStorage,
) -> Result<SessionImageWriter, SessionError> {
    match writer {
        PendingWriter::DrawOn(writer) => {
            validate_draw_on_format(dst, writer.tool, dst_format)?;
            Ok(SessionImageWriter::DrawOn(writer))
        }
        PendingWriter::Derive(command) => {
            lower_session_command(command, dst, dst_layout, doc_access, session_specs, global)
                .map(SessionImageWriter::Derive)
        }
    }
}

fn validate_draw_on_format(
    id: ImageId,
    tool: DrawOnToolKind,
    format: GlaFormat,
) -> Result<(), SessionError> {
    let supported = match tool {
        DrawOnToolKind::RadialKernel1D => {
            format.channel_count == ChannelCount::D1 && format.channel_type == ChannelType::F32
        }
        DrawOnToolKind::ReplaceCircle4D => {
            format.channel_count == ChannelCount::D4 && format.channel_type == ChannelType::F32
        }
    };
    if supported {
        Ok(())
    } else {
        Err(SessionError::DrawOnFormatMismatch { id, tool, format })
    }
}

fn lower_session_command(
    command: SessionCommand,
    dst: ImageId,
    dst_layout: GlaImageLayout,
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    session_specs: &HashMap<ImageId, LocalImageSpec>,
    global: &GlobalStorage,
) -> Result<ImageDeriveCommand<SessionImageId>, SessionError> {
    let mut ops = Vec::with_capacity(command.reads.len());
    for read in command.reads {
        let (key, layout) = match read.image {
            SessionReadImage::Current(id) => {
                if !session_specs.contains_key(&id) && !doc_access.contains_key(&id) {
                    return Err(SessionError::CurrentReadRequiresDeclaredImage { id });
                }
                let layout = image_layout(id, session_specs, global)?;
                (SessionImageId::Current(id), layout)
            }
            SessionReadImage::Backup(id) => {
                if !doc_access.contains_key(&id) {
                    return Err(SessionError::BackupReadRequiresDocImage { id });
                }
                let image = global
                    .image(id)
                    .ok_or(SessionError::MissingGlobalImage { id })?;
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
) -> Result<ImageDeriveCommand<SessionImageId>, SessionError> {
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
) -> Result<ImageDeriveCommand<SessionImageId>, SessionError> {
    let mut ops = Vec::with_capacity(command.reads.len());
    for read in &command.reads {
        let image = global
            .image(read.image)
            .ok_or(SessionError::MissingGlobalImage { id: read.image })?;
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
) -> Result<GlaImageLayout, SessionError> {
    session_specs
        .get(&id)
        .map(|spec| spec.layout)
        .or_else(|| global.image(id).map(GlobalImage::layout))
        .ok_or(SessionError::MissingGlobalImage { id })
}

fn validate_writer_cycles(
    images: &HashMap<ImageId, PendingSessionImage>,
) -> Result<(), SessionError> {
    let mut visited = HashSet::new();
    let mut visiting = HashSet::new();
    for id in images.keys().copied() {
        visit_writer(id, images, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn visit_writer(
    id: ImageId,
    images: &HashMap<ImageId, PendingSessionImage>,
    visiting: &mut HashSet<ImageId>,
    visited: &mut HashSet<ImageId>,
) -> Result<(), SessionError> {
    if visited.contains(&id) {
        return Ok(());
    }
    if !visiting.insert(id) {
        return Err(SessionError::WriterCycle { id });
    }

    if let Some(PendingSessionImage {
        writer: SessionImageWriter::Derive(command),
        ..
    }) = images.get(&id)
    {
        for op in command.ops.iter().copied() {
            if let Some(SessionImageId::Current(read_id)) = derive_read(op) {
                if images.contains_key(&read_id) {
                    visit_writer(read_id, images, visiting, visited)?;
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

fn allocate_images(
    pending: HashMap<ImageId, PendingSessionImage>,
    global: &mut GlobalStorage,
) -> Result<HashMap<ImageId, SessionImage>, SessionError> {
    let mut images = HashMap::new();
    for (id, image) in pending {
        let content = match image.content {
            SessionContentKind::Raw => {
                match DenseImage::allocate(image.format, image.layout, global.tiles_mut()) {
                    Ok(image) => SessionImageContent::Raw(image),
                    Err(source) => {
                        release_session_images(global.tiles_mut(), images);
                        return Err(SessionError::Image { id, source });
                    }
                }
            }
            SessionContentKind::Edit => SessionImageContent::Edit(ImageEdit::new()),
        };
        images.insert(
            id,
            SessionImage {
                format: image.format,
                layout: image.layout,
                content,
                writer: image.writer,
            },
        );
    }
    Ok(images)
}

fn release_session_images(tiles: &mut Tiles, images: HashMap<ImageId, SessionImage>) {
    for (_, image) in images {
        image.release_tiles(tiles);
    }
}

fn release_image_edits(tiles: &mut Tiles, edits: HashMap<ImageId, ImageEdit>) {
    for (_, edit) in edits {
        edit.release_tiles(tiles);
    }
}

fn dirty_from_edits(edits: &HashMap<ImageId, ImageEdit>) -> HashMap<ImageId, TileSet> {
    edits
        .iter()
        .filter_map(|(id, edit)| {
            let dirty = TileSet::tiles(edit.edits().iter().map(|(tile_index, _)| *tile_index));
            (!dirty.is_empty()).then_some((*id, dirty))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas::{AtlasLayout, NoAtlasTextures};
    use gla_color::{ChannelCount, ChannelType, PremultipliedRgbaF32};
    use gla_core::CanvasCoordF;
    use gla_ir::{
        Affine2D, DocImageUse, DrawOnToolKind, GraphRead, ImageRole, RegistryPatch,
        RegistryPatchOp, SessionRead,
    };
    use gla_renderer::{Pass, RenderBackend};
    use std::fmt::{Display, Formatter};
    use tile_key::{TileReadRef, Tiles};

    #[derive(Debug)]
    struct TestBackendError;

    impl Display for TestBackendError {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            f.write_str("test backend submit failed")
        }
    }

    impl Error for TestBackendError {}

    #[derive(Default)]
    struct TestBackend {
        submitted: Vec<Vec<Pass>>,
        fail: bool,
    }

    impl TestBackend {
        fn clear(&mut self) {
            self.submitted.clear();
        }

        fn submitted_passes(&self) -> Vec<Pass> {
            self.submitted.iter().flatten().copied().collect()
        }
    }

    impl RenderBackend for TestBackend {
        type Error = TestBackendError;

        fn submit(&mut self, passes: &[Pass]) -> Result<(), Self::Error> {
            if self.fail {
                return Err(TestBackendError);
            }
            self.submitted.push(passes.to_vec());
            Ok(())
        }
    }

    fn rgba_format() -> GlaFormat {
        GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::F32,
        }
    }

    fn value_format() -> GlaFormat {
        GlaFormat {
            channel_count: ChannelCount::D1,
            channel_type: ChannelType::F32,
        }
    }

    fn rgba_u8_format() -> GlaFormat {
        GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::U8,
        }
    }

    fn value_u8_format() -> GlaFormat {
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

    fn layout_with_tiles(width_tiles: u32, height_tiles: u32) -> GlaImageLayout {
        GlaImageLayout::new(
            width_tiles * IMAGE_TILE_SIZE,
            height_tiles * IMAGE_TILE_SIZE,
        )
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

    fn replace_input(center_x: f32, center_y: f32, radius_px: f32) -> DrawOnInput {
        DrawOnInput::replace_circle_4d(
            center_x,
            center_y,
            radius_px,
            radius_px,
            PremultipliedRgbaF32::new(0.25, 0.5, 0.75, 1.0),
        )
    }

    fn replace_draw(id: ImageId) -> DrawOnCommand {
        DrawOnCommand::with_tool(id, DrawOnToolKind::ReplaceCircle4D)
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
        GlobalStorage::new(tiles)
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
                role: ImageRole::Derived(GraphCommand::new(reads)),
            }]))
            .unwrap();
    }

    #[test]
    fn begin_builds_private_local_table_for_pixel_round_style_session() {
        let base = ImageId::new(1);
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive_with_layout(&mut global, base, rgba_format(), layout_with_tiles(1, 1));

        let ir = DrawSessionIR {
            expected_document_version: global.version(),
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

        let session = DrawSession::begin(&ir, &mut global).unwrap();

        assert!(session.images.get(&coverage).unwrap().content().is_raw());
        assert!(session.images.get(&base).unwrap().content().is_edit());
    }

    #[test]
    fn begin_rejects_draw_on_format_mismatch() {
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_u8_format()),
                layout: MetadataRef::Concrete(layout()),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: Vec::new(),
        };

        let err = DrawSession::begin(&ir, &mut global).unwrap_err();

        assert!(matches!(
            err,
            SessionError::DrawOnFormatMismatch {
                id,
                tool: DrawOnToolKind::RadialKernel1D,
                format,
            } if id == coverage && format == value_u8_format()
        ));
    }

    #[test]
    fn begin_rejects_replace_circle_format_mismatch() {
        let paint = ImageId::new(2);
        let mut global = storage_with_atlases();
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: paint,
                format: MetadataRef::Concrete(rgba_u8_format()),
                layout: MetadataRef::Concrete(layout()),
            }],
            draw_on: vec![DrawOnCommand::with_tool(
                paint,
                DrawOnToolKind::ReplaceCircle4D,
            )],
            derive: Vec::new(),
        };

        let err = DrawSession::begin(&ir, &mut global).unwrap_err();

        assert!(matches!(
            err,
            SessionError::DrawOnFormatMismatch {
                id,
                tool: DrawOnToolKind::ReplaceCircle4D,
                format,
            } if id == paint && format == rgba_u8_format()
        ));
    }

    #[test]
    fn draw_dab_routes_canvas_input_and_records_brush_passes() {
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Concrete(multi_tile_layout()),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: Vec::new(),
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();

        frame
            .draw_dab(coverage, canvas_input(IMAGE_TILE_SIZE as f32, 4.0, 0.25))
            .unwrap();

        let brush_passes = frame
            .dab_passes()
            .iter()
            .filter(|pass| matches!(pass, Pass::DrawRadialKernel1D { .. }))
            .count();
        let gutter_passes = frame
            .dab_passes()
            .iter()
            .filter(|pass| matches!(pass, Pass::FixGutter { .. }))
            .count();
        assert_eq!(brush_passes, 2);
        assert_eq!(gutter_passes, 2);
    }

    #[test]
    fn route_draw_targets_returns_target_space_coordinates_in_draw_order() {
        let base = ImageId::new(1);
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive_with_layout(&mut global, base, rgba_format(), layout_with_tiles(1, 1));
        let mut read = SessionRead::current(coverage);
        read.mapping = Mapping::Matrix(Affine2D {
            m11: 1.0,
            m12: 0.0,
            m21: 0.0,
            m22: 1.0,
            tx: 5.0,
            ty: 7.0,
        });
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Like(base),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: vec![gla_ir::DeriveCommand::new(
                vec![SessionRead::backup(base), read],
                base,
            )],
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let frame = session.begin_frame();

        let routes = frame.route_draw_targets(base, 0.0, 0.0).unwrap();

        assert_eq!(
            routes,
            vec![DrawOnRoute {
                target: coverage,
                tool: DrawOnToolKind::RadialKernel1D,
                target_x: 5.0,
                target_y: 7.0,
            }]
        );
    }

    #[test]
    fn draw_on_records_typed_radial_input_without_canvas_lowering() {
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Concrete(layout()),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: Vec::new(),
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();

        frame
            .draw_on(
                coverage,
                DrawOnInput::radial_kernel_1d(0.0, 0.0, 1.0, 3.0, 0.25),
            )
            .unwrap();

        assert!(frame.dab_passes().iter().any(|pass| matches!(
            pass,
            Pass::DrawRadialKernel1D {
                radius_px,
                amplitude,
                ..
            } if *radius_px == 3.0 && *amplitude == 0.25
        )));
    }

    #[test]
    fn draw_on_does_not_clamp_typed_radial_amplitude() {
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Concrete(layout()),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: Vec::new(),
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();

        frame
            .draw_on(
                coverage,
                DrawOnInput::radial_kernel_1d(0.0, 0.0, 1.0, 3.0, 4.0),
            )
            .unwrap();

        assert!(frame.dab_passes().iter().any(
            |pass| matches!(pass, Pass::DrawRadialKernel1D { amplitude, .. } if *amplitude == 4.0)
        ));
    }

    #[test]
    fn draw_on_zero_effect_input_is_noop() {
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Concrete(layout()),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: Vec::new(),
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();

        frame
            .draw_on(
                coverage,
                DrawOnInput::radial_kernel_1d(0.0, 0.0, 1.0, 0.0, 1.0),
            )
            .unwrap();

        assert!(frame.is_clean());
    }

    #[test]
    fn draw_on_zero_footprint_is_noop() {
        let paint = ImageId::new(2);
        let color = PremultipliedRgbaF32::new(0.25, 0.5, 0.75, 1.0);
        let mut global = storage_with_atlases();
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: paint,
                format: MetadataRef::Concrete(rgba_format()),
                layout: MetadataRef::Concrete(layout()),
            }],
            draw_on: vec![DrawOnCommand::with_tool(
                paint,
                DrawOnToolKind::ReplaceCircle4D,
            )],
            derive: Vec::new(),
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();

        frame
            .draw_on(
                paint,
                DrawOnInput::replace_circle_4d(0.0, 0.0, 0.0, 2.0, color),
            )
            .unwrap();

        assert!(frame.is_clean());
    }

    #[test]
    fn draw_on_records_typed_replace_circle_input() {
        let paint = ImageId::new(2);
        let color = PremultipliedRgbaF32::new(0.25, 0.5, 0.75, 1.0);
        let mut global = storage_with_atlases();
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: paint,
                format: MetadataRef::Concrete(rgba_format()),
                layout: MetadataRef::Concrete(layout()),
            }],
            draw_on: vec![DrawOnCommand::with_tool(
                paint,
                DrawOnToolKind::ReplaceCircle4D,
            )],
            derive: Vec::new(),
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();

        frame
            .draw_on(
                paint,
                DrawOnInput::replace_circle_4d(0.0, 0.0, 1.0, 2.0, color),
            )
            .unwrap();

        assert!(matches!(frame.dab_passes()[0], Pass::Clear { .. }));
        assert!(matches!(
            frame.dab_passes()[1],
            Pass::ReplaceCircle4D {
                radius_px,
                color: pass_color,
                ..
            } if radius_px == 2.0 && pass_color == color
        ));
        assert!(matches!(frame.dab_passes()[2], Pass::FixGutter { .. }));
    }

    #[test]
    fn draw_dab_clamps_radial_flow_to_unit_interval() {
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Concrete(layout()),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: Vec::new(),
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();

        frame
            .draw_dab(coverage, canvas_input(0.0, 0.0, 4.0))
            .unwrap();

        assert!(frame.dab_passes().iter().any(
            |pass| matches!(pass, Pass::DrawRadialKernel1D { amplitude, .. } if *amplitude == 1.0)
        ));
    }

    #[test]
    fn draw_dab_routes_from_shown_image_through_matrix_to_draw_target() {
        let base = ImageId::new(1);
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive_with_layout(&mut global, base, rgba_format(), layout_with_tiles(1, 1));
        let mut read = SessionRead::current(coverage);
        read.mapping = Mapping::Matrix(Affine2D {
            m11: 1.0,
            m12: 0.0,
            m21: 0.0,
            m22: 1.0,
            tx: 5.0,
            ty: 7.0,
        });
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Like(base),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: vec![gla_ir::DeriveCommand::new(
                vec![SessionRead::backup(base), read],
                base,
            )],
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();

        frame.draw_dab(base, canvas_input(0.0, 0.0, 0.5)).unwrap();

        assert!(frame.dab_passes().iter().any(|pass| matches!(
            pass,
            Pass::DrawRadialKernel1D {
                center_in_tile_x,
                center_in_tile_y,
                ..
            } if *center_in_tile_x == 5.0 && *center_in_tile_y == 7.0
        )));
    }

    #[test]
    fn draw_dab_rejects_ambiguous_route_to_same_target() {
        let paint = ImageId::new(1);
        let left = ImageId::new(2);
        let right = ImageId::new(3);
        let shown = ImageId::new(4);
        let mut global = storage_with_atlases();
        let local_image = |id| SessionImageDecl::Primitive {
            id,
            format: MetadataRef::Concrete(value_format()),
            layout: MetadataRef::Concrete(layout()),
        };
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: Vec::new(),
            session_images: vec![
                local_image(paint),
                local_image(left),
                local_image(right),
                local_image(shown),
            ],
            draw_on: vec![DrawOnCommand::new(paint)],
            derive: vec![
                gla_ir::DeriveCommand::new(vec![SessionRead::current(paint)], left),
                gla_ir::DeriveCommand::new(vec![SessionRead::current(paint)], right),
                gla_ir::DeriveCommand::new(
                    vec![SessionRead::current(left), SessionRead::current(right)],
                    shown,
                ),
            ],
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();

        let err = frame
            .draw_dab(shown, canvas_input(0.0, 0.0, 0.5))
            .unwrap_err();

        assert!(matches!(
            err,
            SessionError::AmbiguousInputRoute {
                shown: err_shown,
                target,
            } if err_shown == shown && target == paint
        ));
    }

    #[test]
    fn draw_dab_rejects_input_image_outside_active_session() {
        let coverage = ImageId::new(2);
        let missing = ImageId::new(99);
        let mut global = storage_with_atlases();
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Concrete(layout()),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: Vec::new(),
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();

        let err = frame
            .draw_dab(missing, canvas_input(0.0, 0.0, 0.5))
            .unwrap_err();

        assert!(matches!(
            err,
            SessionError::InputImageNotActive { id } if id == missing
        ));
    }

    #[test]
    fn draw_dab_rejects_non_radial_tool_during_compat_lowering() {
        let paint = ImageId::new(2);
        let mut global = storage_with_atlases();
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: paint,
                format: MetadataRef::Concrete(rgba_format()),
                layout: MetadataRef::Concrete(layout()),
            }],
            draw_on: vec![DrawOnCommand::with_tool(
                paint,
                DrawOnToolKind::ReplaceCircle4D,
            )],
            derive: Vec::new(),
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();

        let err = frame
            .draw_dab(paint, canvas_input(0.0, 0.0, 0.5))
            .unwrap_err();

        assert!(matches!(
            err,
            SessionError::DrawOnInputMismatch {
                id,
                tool: DrawOnToolKind::ReplaceCircle4D,
                input: DrawOnToolKind::RadialKernel1D,
            } if id == paint
        ));
        assert!(frame.is_clean());
    }

    #[test]
    fn raw_first_draw_records_clear_before_radial_kernel() {
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Concrete(layout()),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: Vec::new(),
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();

        frame
            .draw_dab(coverage, canvas_input(0.0, 0.0, 0.4))
            .unwrap();

        let passes = frame.dab_passes();
        let Pass::Clear { dst } = passes[0] else {
            panic!("first raw draw must clear newly materialized tile before additive draw");
        };
        assert!(matches!(
            passes[1],
            Pass::DrawRadialKernel1D { dst: draw_dst, .. } if draw_dst == dst
        ));
        assert_eq!(passes[2], Pass::FixGutter { dst });
    }

    #[test]
    fn repeated_raw_draw_does_not_repeat_materialization_clear() {
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Concrete(layout()),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: Vec::new(),
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();

        frame
            .draw_dab(coverage, canvas_input(0.0, 0.0, 0.4))
            .unwrap();
        frame
            .draw_dab(coverage, canvas_input(0.0, 0.0, 0.4))
            .unwrap();

        let clear_count = frame
            .dab_passes()
            .iter()
            .filter(|pass| matches!(pass, Pass::Clear { .. }))
            .count();
        let draw_count = frame
            .dab_passes()
            .iter()
            .filter(|pass| matches!(pass, Pass::DrawRadialKernel1D { .. }))
            .count();
        assert_eq!(clear_count, 1);
        assert_eq!(draw_count, 2);
    }

    #[test]
    fn flush_frame_uploads_dirty_and_materializes_downstream_derive() {
        let base = ImageId::new(1);
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
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
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();
        let mut backend = TestBackend::default();

        frame.draw_dab(base, canvas_input(0.0, 0.0, 0.6)).unwrap();
        frame.flush(&mut backend).unwrap();

        assert!(frame.is_clean());
        assert!(!backend.submitted_passes().is_empty());
        drop(frame);
        assert_eq!(session.doc_dirty().get(&base), Some(&TileSet::single(0)));
        let SessionImageContent::Edit(edit) = session.images.get(&base).unwrap().content() else {
            panic!("base should be edit content");
        };
        assert_eq!(edit.edits().len(), 1);
    }

    #[test]
    fn dirty_upload_identity_uses_layout_aware_tile_rects() {
        let base = ImageId::new(1);
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive_with_layout(&mut global, base, rgba_format(), layout_with_tiles(1, 2));
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Concrete(layout_with_tiles(2, 2)),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: vec![gla_ir::DeriveCommand::new(
                vec![SessionRead::backup(base), SessionRead::current(coverage)],
                base,
            )],
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();
        let mut backend = TestBackend::default();

        frame
            .draw_dab(base, canvas_input(1.0, IMAGE_TILE_SIZE as f32 + 1.0, 0.6))
            .unwrap();
        frame.flush(&mut backend).unwrap();
        drop(frame);

        assert_eq!(session.doc_dirty().get(&base), Some(&TileSet::single(1)));
    }

    #[test]
    fn flush_failure_keeps_frame_dirty_and_dab_passes_for_retry() {
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Concrete(layout()),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: Vec::new(),
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();
        frame
            .draw_dab(coverage, canvas_input(0.0, 0.0, 0.6))
            .unwrap();
        let dab_pass_count = frame.dab_passes().len();
        let mut backend = TestBackend {
            fail: true,
            ..Default::default()
        };

        let err = frame.flush(&mut backend).unwrap_err();

        assert!(matches!(err, SessionError::RenderBackend { .. }));
        assert!(!frame.is_clean());
        assert_eq!(frame.dab_passes().len(), dab_pass_count);
        assert!(backend.submitted.is_empty());
    }

    #[test]
    fn failed_flush_retries_same_generated_passes_without_mutating_session_again() {
        let base = ImageId::new(1);
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
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
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();
        let mut backend = TestBackend {
            fail: true,
            ..Default::default()
        };
        frame.draw_dab(base, canvas_input(0.0, 0.0, 0.6)).unwrap();

        let err = frame.flush(&mut backend).unwrap_err();
        let pending = frame
            .pending_flush_passes
            .as_ref()
            .expect("failed flush should keep generated passes")
            .clone();

        assert!(matches!(err, SessionError::RenderBackend { .. }));

        backend.fail = false;
        frame.flush(&mut backend).unwrap();

        assert_eq!(backend.submitted, vec![pending]);
        assert!(frame.is_clean());
        drop(frame);
        assert_eq!(session.doc_dirty().get(&base), Some(&TileSet::single(0)));
    }

    #[test]
    fn draw_dab_after_failed_flush_is_rejected_until_pending_submit_succeeds() {
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(value_format()),
                layout: MetadataRef::Concrete(layout()),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: Vec::new(),
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();
        let mut backend = TestBackend {
            fail: true,
            ..Default::default()
        };
        frame
            .draw_dab(coverage, canvas_input(0.0, 0.0, 0.6))
            .unwrap();
        frame.flush(&mut backend).unwrap_err();

        let err = frame
            .draw_dab(coverage, canvas_input(1.0, 1.0, 0.6))
            .unwrap_err();

        assert!(matches!(err, SessionError::PendingFrameSubmit));

        backend.fail = false;
        frame.flush(&mut backend).unwrap();
        frame
            .draw_dab(coverage, canvas_input(1.0, 1.0, 0.6))
            .unwrap();
    }

    #[test]
    fn dropping_session_releases_materialized_local_tiles() {
        let base = ImageId::new(1);
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());
        let rgba_atlas = global.tiles().atlas_for_format(rgba_format()).unwrap();
        let value_atlas = global.tiles().atlas_for_format(value_format()).unwrap();

        {
            let ir = DrawSessionIR {
                expected_document_version: global.version(),
                doc_images: Vec::new(),
                session_images: vec![SessionImageDecl::Primitive {
                    id: coverage,
                    format: MetadataRef::Concrete(value_format()),
                    layout: MetadataRef::Concrete(layout()),
                }],
                draw_on: vec![DrawOnCommand::new(coverage)],
                derive: Vec::new(),
            };
            let mut session = DrawSession::begin(&ir, &mut global).unwrap();
            let mut frame = session.begin_frame();

            frame
                .draw_dab(coverage, canvas_input(0.0, 0.0, 0.4))
                .unwrap();

            assert!(matches!(frame.dab_passes()[0], Pass::Clear { .. }));
        }

        assert_eq!(global.tiles().atlas(value_atlas).unwrap().remaining(), 256);

        {
            let ir = DrawSessionIR {
                expected_document_version: global.version(),
                doc_images: vec![DocImageUse::read_write(base)],
                session_images: Vec::new(),
                draw_on: vec![replace_draw(base)],
                derive: Vec::new(),
            };
            let mut session = DrawSession::begin(&ir, &mut global).unwrap();
            let mut frame = session.begin_frame();

            frame.draw_on(base, replace_input(0.0, 0.0, 1.0)).unwrap();

            assert!(matches!(frame.dab_passes()[0], Pass::Clear { .. }));
        }

        assert_eq!(global.tiles().atlas(rgba_atlas).unwrap().remaining(), 256);
        let image = global.image(base).unwrap().as_dense().unwrap();
        assert_eq!(
            global.tiles().read_ref(image.tile(0).unwrap()).unwrap(),
            TileReadRef::Zero
        );
    }

    #[test]
    fn empty_commit_returns_none_without_bumping_version_or_history() {
        let mut global = storage_with_atlases();
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: Vec::new(),
            session_images: Vec::new(),
            draw_on: Vec::new(),
            derive: Vec::new(),
        };
        let session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut history = DrawHistory::new();

        let commit = session.commit(&mut history).unwrap();

        assert_eq!(commit, None);
        assert_eq!(global.version(), DocumentVersionId::default());
        assert!(history.patches.is_empty());
    }

    #[test]
    fn commit_applies_primitive_edit_and_history_patch_consumes_record() {
        let base = ImageId::new(1);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());
        let begin_version = global.version();
        let ir = DrawSessionIR {
            expected_document_version: begin_version,
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: Vec::new(),
            draw_on: vec![replace_draw(base)],
            derive: Vec::new(),
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();
        let mut backend = TestBackend::default();
        frame.draw_on(base, replace_input(0.0, 0.0, 1.0)).unwrap();
        frame.flush(&mut backend).unwrap();
        drop(frame);
        let mut history = DrawHistory::new();

        let commit = session.commit(&mut history).unwrap().unwrap();
        assert_eq!(commit.version, begin_version.next());
        let undo_record = history
            .apply_stored_patch(commit.record_id, &mut global, &mut backend)
            .unwrap();

        assert_eq!(global.version(), commit.version.next());
        assert!(history.patches.contains_key(&undo_record));
        let image = global.image(base).unwrap().as_dense().unwrap();
        assert_eq!(
            global.tiles().read_ref(image.tile(0).unwrap()).unwrap(),
            TileReadRef::Zero
        );
    }

    #[test]
    fn stored_patch_flush_failure_keeps_history_record_and_global_truth() {
        let base = ImageId::new(1);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());
        let begin_version = global.version();
        let ir = DrawSessionIR {
            expected_document_version: begin_version,
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: Vec::new(),
            draw_on: vec![replace_draw(base)],
            derive: Vec::new(),
        };
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();
        let mut backend = TestBackend::default();
        frame.draw_on(base, replace_input(0.0, 0.0, 1.0)).unwrap();
        frame.flush(&mut backend).unwrap();
        drop(frame);
        let mut history = DrawHistory::new();
        let commit = session.commit(&mut history).unwrap().unwrap();
        let image = global.image(base).unwrap().as_dense().unwrap();
        assert!(matches!(
            global.tiles().read_ref(image.tile(0).unwrap()).unwrap(),
            TileReadRef::Physical(_)
        ));

        let mut failing_backend = TestBackend {
            fail: true,
            ..Default::default()
        };
        let err = history.apply_stored_patch(commit.record_id, &mut global, &mut failing_backend);

        assert!(matches!(err, Err(SessionError::RenderBackend { .. })));
        assert_eq!(global.version(), commit.version);
        assert!(history.patches.contains_key(&commit.record_id));
        let image = global.image(base).unwrap().as_dense().unwrap();
        assert!(matches!(
            global.tiles().read_ref(image.tile(0).unwrap()).unwrap(),
            TileReadRef::Physical(_)
        ));

        let undo_record = history
            .apply_stored_patch(commit.record_id, &mut global, &mut backend)
            .unwrap();
        assert!(history.patches.contains_key(&undo_record));
        let image = global.image(base).unwrap().as_dense().unwrap();
        assert_eq!(
            global.tiles().read_ref(image.tile(0).unwrap()).unwrap(),
            TileReadRef::Zero
        );
    }

    #[test]
    fn commit_applies_explicitly_flushed_scratch_derive() {
        let base = ImageId::new(1);
        let coverage = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());
        let begin_version = global.version();
        let ir = DrawSessionIR {
            expected_document_version: begin_version,
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
        let mut session = DrawSession::begin(&ir, &mut global).unwrap();
        let mut frame = session.begin_frame();
        let mut backend = TestBackend::default();
        frame.draw_dab(base, canvas_input(0.0, 0.0, 0.6)).unwrap();
        frame.flush(&mut backend).unwrap();
        drop(frame);
        let mut history = DrawHistory::new();

        let commit = session.commit(&mut history).unwrap().unwrap();

        assert_eq!(commit.version, begin_version.next());
        assert!(history.patches.contains_key(&commit.record_id));
        let image = global.image(base).unwrap().as_dense().unwrap();
        assert!(matches!(
            global.tiles().read_ref(image.tile(0).unwrap()).unwrap(),
            TileReadRef::Physical(_)
        ));
    }

    #[test]
    fn stored_patch_replay_refreshes_downstream_derived_cache() {
        let base = ImageId::new(1);
        let group = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());
        add_global_derived(&mut global, group, vec![GraphRead::current(base)]);

        let reader_ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: vec![DocImageUse::read(base)],
            session_images: Vec::new(),
            draw_on: Vec::new(),
            derive: Vec::new(),
        };
        {
            let mut reader = DrawSession::begin(&reader_ir, &mut global).unwrap();
            let mut passes = Vec::new();
            let mut ctx = reader.render_ctx(&mut passes, None);
            let TileReadRef::Physical(pos) = ctx.render(SessionImageId::Global(group), 0).unwrap()
            else {
                panic!("global derived cache should materialize before undo");
            };
            let _old_group_pos = pos;
        }

        let draw_ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: Vec::new(),
            draw_on: vec![replace_draw(base)],
            derive: Vec::new(),
        };
        let mut session = DrawSession::begin(&draw_ir, &mut global).unwrap();
        let mut frame = session.begin_frame();
        let mut backend = TestBackend::default();
        frame.draw_on(base, replace_input(0.0, 0.0, 1.0)).unwrap();
        frame.flush(&mut backend).unwrap();
        drop(frame);
        let mut history = DrawHistory::new();
        let commit = session.commit(&mut history).unwrap().unwrap();

        backend.clear();
        let undo_record = history
            .apply_stored_patch(commit.record_id, &mut global, &mut backend)
            .unwrap();
        let TileReadRef::Physical(new_group_pos) = global.read_global_ref(group, 0).unwrap() else {
            panic!("global derived cache should remain materialized after undo refresh");
        };

        assert_eq!(
            backend.submitted_passes(),
            vec![
                Pass::Clear { dst: new_group_pos },
                Pass::Clear { dst: new_group_pos },
                Pass::FixGutter { dst: new_group_pos },
            ]
        );
        assert!(history.patches.contains_key(&undo_record));
    }

    #[test]
    fn active_chain_global_derived_is_session_private_edit() {
        let base = ImageId::new(1);
        let group = ImageId::new(2);
        let mut global = storage_with_atlases();
        add_global_primitive(&mut global, base, rgba_format());
        add_global_derived(&mut global, group, vec![GraphRead::current(base)]);
        let ir = DrawSessionIR {
            expected_document_version: global.version(),
            doc_images: vec![DocImageUse::read_write(base)],
            session_images: Vec::new(),
            draw_on: vec![replace_draw(base)],
            derive: Vec::new(),
        };

        let session = DrawSession::begin(&ir, &mut global).unwrap();

        assert!(session.images.get(&group).unwrap().content().is_edit());
    }
}
