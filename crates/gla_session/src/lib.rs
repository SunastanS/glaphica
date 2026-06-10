use gla_doc::{DocError, Document};
use gla_image::{GlaImageKey, GlaImageLayout, GlaImages, GlaImagesError, IMAGE_TILE_SIZE, TileSet};
use gla_image_command::{Derive, DeriveCommand, ImageRef, RenderCtx};
use gla_ir::*;
use gla_renderer::Renderer;
use std::collections::{HashMap, HashSet};
use tile_key::{TileKey, Tiles, TilesError};

mod frame;
mod local;

pub use frame::FrameBudget;
pub use gla_doc::ImageRole;
pub use local::LocalImageDeclaration;

#[derive(Clone, Copy, Debug)]
pub struct CanvasInput {
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
}

#[derive(Clone, Debug)]
pub struct DrawCommit {
    pub record_id: DrawRecordId,
    pub version: DocumentVersionId,
}

pub type DrawRecordId = u64;

pub struct CommittedDraw {
    pub commit: DrawCommit,
    pub images: GlaImages,
    pub tiles: Tiles,
    pub renderer: Renderer,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageEdit {
    pub source: GlaImageKey,
    pub edits: Vec<(u32, TileKey)>,
}

#[derive(Clone, Debug)]
struct StoredImageEditPatch {
    version: DocumentVersionId,
    edits: HashMap<ImageId, ImageEdit>,
}

#[derive(Default, Debug)]
pub struct DrawHistory {
    patches: HashMap<DrawRecordId, StoredImageEditPatch>,
    next_id: DrawRecordId,
}

#[derive(Debug)]
pub enum SessionError {
    Doc(DocError),
    Image(GlaImagesError),
    Tile(TilesError),
    Renderer(gla_renderer::GpuRendererError),
    ExpectedDocumentVersion {
        expected: DocumentVersionId,
        actual: DocumentVersionId,
    },
    InvalidDrawRecord {
        id: DrawRecordId,
    },
    VersionMismatch {
        expected: DocumentVersionId,
        actual: DocumentVersionId,
    },
    ReadWriteRequiresPrimitive {
        id: ImageId,
    },
    DuplicateDocImageUse {
        id: ImageId,
    },
    DuplicateSessionImage {
        id: ImageId,
    },
    SessionImageConflictsWithReadWriteDoc {
        id: ImageId,
    },
    LikeReferenceNotDeclaredYet {
        id: ImageId,
    },
    LikeReferenceUnknown {
        id: ImageId,
    },
    CurrentReadNotDeclared {
        id: ImageId,
    },
    BackupReadNotDeclared {
        id: ImageId,
    },
    DuplicateWriter {
        id: ImageId,
    },
    DeriveReadsDestinationCurrent {
        id: ImageId,
    },
    CannotShadowDocDerived {
        id: ImageId,
    },
    DestinationNotWritable {
        id: ImageId,
    },
    MissingImage {
        id: ImageId,
    },
    TileIndexOutOfBounds {
        tile_index: u32,
        tile_count: u32,
    },
    EditMiss {
        source: GlaImageKey,
    },
    InvalidEditTile {
        id: ImageId,
        tile_index: u32,
    },
    PrimitiveImageHasInvalidTile {
        id: ImageId,
        tile_index: u32,
    },
}

impl From<DocError> for SessionError {
    fn from(e: DocError) -> Self {
        SessionError::Doc(e)
    }
}

impl From<GlaImagesError> for SessionError {
    fn from(e: GlaImagesError) -> Self {
        SessionError::Image(e)
    }
}

impl From<TilesError> for SessionError {
    fn from(e: TilesError) -> Self {
        SessionError::Tile(e)
    }
}

impl From<gla_renderer::GpuRendererError> for SessionError {
    fn from(e: gla_renderer::GpuRendererError) -> Self {
        SessionError::Renderer(e)
    }
}

impl DrawHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_stored_patch(
        &mut self,
        id: DrawRecordId,
        doc: &mut Document,
        images: &mut GlaImages,
    ) -> Result<DrawRecordId, SessionError> {
        let stored = self
            .patches
            .get(&id)
            .cloned()
            .ok_or(SessionError::InvalidDrawRecord { id })?;
        if stored.version != doc.version() {
            return Err(SessionError::VersionMismatch {
                expected: stored.version,
                actual: doc.version(),
            });
        }

        let inverse = apply_image_edit_patch(doc, images, &stored.edits)?;
        let version = doc.bump_version();
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlaLocalImageKey(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionImageKey {
    Doc(GlaImageKey),
    Local(GlaLocalImageKey),
}

#[derive(Clone, Debug)]
enum SessionImage {
    Raw {
        format: gla_color::GlaFormat,
        layout: GlaImageLayout,
        tiles: Box<[TileKey]>,
    },
    Edit {
        format: gla_color::GlaFormat,
        layout: GlaImageLayout,
        source: GlaImageKey,
        edits: Vec<(u32, TileKey)>,
    },
}

impl SessionImage {
    fn format(&self) -> gla_color::GlaFormat {
        match self {
            Self::Raw { format, .. } | Self::Edit { format, .. } => *format,
        }
    }

    fn layout(&self) -> GlaImageLayout {
        match self {
            Self::Raw { layout, .. } | Self::Edit { layout, .. } => *layout,
        }
    }

    fn raw_tile(&self, tile_index: u32) -> Result<TileKey, SessionError> {
        match self {
            Self::Raw { tiles, layout, .. } => {
                tiles
                    .get(tile_index as usize)
                    .copied()
                    .ok_or(SessionError::TileIndexOutOfBounds {
                        tile_index,
                        tile_count: layout.tile_count(),
                    })
            }
            Self::Edit { edits, source, .. } => {
                match edits.binary_search_by_key(&tile_index, |(index, _)| *index) {
                    Ok(index) => Ok(edits[index].1),
                    Err(_) => Err(SessionError::EditMiss { source: *source }),
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct LocalKeyEntry {
    key: SessionImageKey,
    layout: GlaImageLayout,
}

#[derive(Clone, Debug)]
struct DrawOnInput {
    dst_id: ImageId,
    dst_key: SessionImageKey,
    input_mapping: Mapping,
    _tool: Tool,
    _tool_params: ToolParams,
}

#[derive(Clone, Copy, Debug)]
struct DirtyEdge {
    src: ImageId,
    dst: ImageId,
    mapping: Mapping,
    modifier: FootprintModifier,
}

pub struct DrawSession {
    doc_root: ImageId,
    doc_version: DocumentVersionId,
    doc_roles: HashMap<ImageId, ImageRole>,
    doc_bindings: HashMap<ImageId, GlaImageKey>,
    doc_write_ids: HashSet<ImageId>,
    doc_shadow_ids: HashSet<ImageId>,
    local_images: Vec<SessionImage>,
    local_keys: HashMap<ImageId, LocalKeyEntry>,
    local_commands: HashMap<SessionImageKey, DeriveCommand<SessionImageKey>>,
    key_to_id: HashMap<GlaImageKey, ImageId>,
    draw_inputs: Vec<DrawOnInput>,
    dirty_edges: Vec<DirtyEdge>,
    frame_draw_dirty: Vec<TileSet>,
    doc_dirty: HashMap<ImageId, TileSet>,
    root_demand: TileSet,
    images: GlaImages,
    tiles: Tiles,
    renderer: Renderer,
    atlas_id: u8,
}

impl DrawSession {
    pub fn new(
        ir: DrawSessionIR,
        doc: &Document,
        images: GlaImages,
        tiles: Tiles,
        renderer: Renderer,
        atlas_id: u8,
    ) -> Result<Self, SessionError> {
        if ir.expected_document_version != doc.version() {
            return Err(SessionError::ExpectedDocumentVersion {
                expected: ir.expected_document_version,
                actual: doc.version(),
            });
        }

        let doc_roles = doc.roles().clone();
        let doc_bindings = doc.bindings().clone();
        let doc_root = doc.root();
        let doc_version = doc.version();

        let doc_access = validate_doc_image_uses(&doc_roles, &ir.doc_images)?;
        let session_decls = resolve_session_declarations(
            &doc_roles,
            &doc_bindings,
            &images,
            &doc_access,
            &ir.session_images,
        )?;

        let write_starts = collect_write_starts(&doc_roles, &doc_access, &session_decls, &ir)?;
        let active_ids = compute_active_chain(doc_root, &doc_roles, &write_starts);
        let doc_write_ids = doc_access
            .iter()
            .filter_map(|(id, access)| (*access == DocumentImageAccess::ReadWrite).then_some(*id))
            .collect::<HashSet<_>>();
        let dirty_edges = collect_dirty_edges(&doc_roles, &session_decls, &ir);

        let mut self_ = Self {
            doc_root,
            doc_version,
            doc_roles: doc_roles.clone(),
            doc_bindings: doc_bindings.clone(),
            doc_write_ids,
            doc_shadow_ids: HashSet::new(),
            local_images: Vec::new(),
            local_keys: HashMap::new(),
            local_commands: HashMap::new(),
            key_to_id: HashMap::new(),
            draw_inputs: Vec::new(),
            dirty_edges,
            frame_draw_dirty: Vec::new(),
            doc_dirty: HashMap::new(),
            root_demand: TileSet::default(),
            images,
            tiles,
            renderer,
            atlas_id,
        };

        {
            for id in &active_ids {
                if let Some(old_key) = doc_bindings.get(id).copied() {
                    let (format, layout) = {
                        let image = self_.images.get(old_key)?;
                        (image.format, image.layout)
                    };
                    let local_key = self_.push_local_image(SessionImage::Edit {
                        format,
                        layout,
                        source: old_key,
                        edits: Vec::new(),
                    });
                    self_.local_keys.insert(
                        *id,
                        LocalKeyEntry {
                            key: SessionImageKey::Local(local_key),
                            layout,
                        },
                    );
                    self_.doc_shadow_ids.insert(*id);
                }
            }
            for (id, decl) in &session_decls {
                let (format, layout) = match decl {
                    LocalImageDeclaration::Primitive { format, layout } => (*format, *layout),
                    LocalImageDeclaration::Derived { format, layout, .. } => (*format, *layout),
                };
                let tile_keys = self_
                    .tiles
                    .alloc_batch_from(atlas_id, layout.tile_count())?;
                let local_key = self_.push_local_image(SessionImage::Raw {
                    format,
                    layout,
                    tiles: tile_keys.into_boxed_slice(),
                });
                self_.local_keys.insert(
                    *id,
                    LocalKeyEntry {
                        key: SessionImageKey::Local(local_key),
                        layout,
                    },
                );
            }
        }

        for (id, key) in &doc_bindings {
            if !self_.key_to_id.contains_key(key) {
                self_.key_to_id.insert(*key, *id);
            }
        }

        for (id, decl) in &session_decls {
            if let LocalImageDeclaration::Derived { command, .. } = decl {
                let entry = self_
                    .local_keys
                    .get(id)
                    .ok_or(SessionError::MissingImage { id: *id })?;
                let ops = self_.lower_session_command(command)?;
                self_
                    .local_commands
                    .insert(entry.key, DeriveCommand::new(entry.key, entry.layout, ops));
            }
        }

        for id in active_ids.iter().rev() {
            if let Some(entry) = self_.local_keys.get(id) {
                let key = entry.key;
                if let Some(role) = doc_roles.get(id) {
                    if let Some(command) = role.graph_command() {
                        let cmd = self_.lower_graph_command(command)?;
                        let full_cmd = DeriveCommand::new(key, entry.layout, cmd);
                        self_.local_commands.insert(key, full_cmd);
                    }
                }
            }
        }

        for cmd in &ir.derive {
            if let Some(entry) = self_.local_keys.get(&cmd.dst) {
                let ops = self_.lower_session_command(&cmd.command)?;
                self_
                    .local_commands
                    .insert(entry.key, DeriveCommand::new(entry.key, entry.layout, ops));
            }
        }

        let draw_inputs = ir
            .draw_on
            .iter()
            .map(|cmd| -> Result<DrawOnInput, SessionError> {
                let id = resolve_draw_on_target(&doc_access, &session_decls, cmd.dst)?;
                let entry = self_
                    .local_keys
                    .get(&id)
                    .ok_or(SessionError::MissingImage { id })?;
                Ok(DrawOnInput {
                    dst_id: id,
                    dst_key: entry.key,
                    input_mapping: cmd.input_mapping,
                    _tool: cmd.tool,
                    _tool_params: cmd.tool_params,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        self_.draw_inputs = draw_inputs;
        self_.frame_draw_dirty = vec![TileSet::default(); self_.draw_inputs.len()];

        Ok(self_)
    }

    pub fn draw_dab(&mut self, input: CanvasInput) -> Result<(), SessionError> {
        for (draw_on_index, di) in self.draw_inputs.clone().into_iter().enumerate() {
            let layout = self.layout_of(di.dst_key)?;
            let tile_index = input_to_tile_index(di.input_mapping, input, layout);

            {
                let tile_key = self.write_session_tile(di.dst_key, tile_index)?;
                let pos = self.tiles.acquire_for_write(tile_key)?;
                self.renderer.clear(pos);
            }

            self.record_frame_draw_dirty(draw_on_index, tile_index);
        }
        Ok(())
    }

    pub fn flush_frame(&mut self) -> Result<(), SessionError> {
        if self.frame_draw_dirty.iter().all(TileSet::is_empty) {
            return Ok(());
        }

        self.root_demand.clear();
        for draw_on_index in 0..self.frame_draw_dirty.len() {
            let mut dirty = std::mem::take(&mut self.frame_draw_dirty[draw_on_index]);
            if !dirty.is_empty() {
                let dst_id = self.draw_inputs[draw_on_index].dst_id;
                self.upload_dirty_from(dst_id, &dirty)?;
            }
            dirty.clear();
            self.frame_draw_dirty[draw_on_index] = dirty;
        }

        self.render_root_demand()
    }

    pub fn commit(
        mut self,
        doc: &mut Document,
        history: &mut DrawHistory,
    ) -> Result<CommittedDraw, SessionError> {
        if self.doc_version != doc.version() {
            return Err(SessionError::ExpectedDocumentVersion {
                expected: self.doc_version,
                actual: doc.version(),
            });
        }

        let (primitive_edits, derived_edits) = self.take_doc_shadow_edits()?;
        self.validate_derived_edits(doc, &derived_edits)?;
        let inverse = apply_image_edit_patch(doc, &mut self.images, &primitive_edits)?;
        self.apply_derived_edits(doc, derived_edits)?;
        let version = doc.bump_version();
        let record_id = history.store_inverse(version, inverse);
        self.discard_remaining_local_tiles();

        Ok(CommittedDraw {
            commit: DrawCommit { record_id, version },
            images: self.images,
            tiles: self.tiles,
            renderer: self.renderer,
        })
    }

    pub fn doc_dirty(&self) -> &HashMap<ImageId, TileSet> {
        &self.doc_dirty
    }

    pub fn pending_render_passes(&self) -> &[gla_renderer::Pass] {
        self.renderer.passes()
    }

    pub fn renderer_mut(&mut self) -> &mut Renderer {
        &mut self.renderer
    }

    pub fn execute_renderer(
        &mut self,
        gpu: &mut gla_renderer::GpuRenderer,
    ) -> Result<(), SessionError> {
        self.renderer.execute(gpu)?;
        Ok(())
    }

    fn take_doc_shadow_edits(
        &mut self,
    ) -> Result<(HashMap<ImageId, ImageEdit>, Vec<(ImageId, ImageEdit)>), SessionError> {
        let mut primitive_edits = HashMap::new();
        let mut derived_edits = Vec::new();
        for id in self.doc_shadow_ids.clone() {
            let Some(entry) = self.local_keys.get(&id).copied() else {
                continue;
            };
            let SessionImageKey::Local(local_key) = entry.key else {
                continue;
            };
            let edit = self.take_local_edit(local_key)?;
            if edit.edits.is_empty() {
                continue;
            }
            match self.doc_roles.get(&id) {
                Some(ImageRole::Primitive) => {
                    primitive_edits.insert(id, edit);
                }
                Some(ImageRole::Derived(_)) => {
                    derived_edits.push((id, edit));
                }
                None => return Err(SessionError::MissingImage { id }),
            }
        }
        Ok((primitive_edits, derived_edits))
    }

    fn take_local_edit(&mut self, key: GlaLocalImageKey) -> Result<ImageEdit, SessionError> {
        match self.local_image_mut(key)? {
            SessionImage::Edit { source, edits, .. } => Ok(ImageEdit {
                source: *source,
                edits: std::mem::take(edits),
            }),
            SessionImage::Raw { .. } => Err(SessionError::DestinationNotWritable {
                id: ImageId::new(u64::from(key.0)),
            }),
        }
    }

    fn validate_derived_edits(
        &self,
        doc: &Document,
        edits: &[(ImageId, ImageEdit)],
    ) -> Result<(), SessionError> {
        for (id, edit) in edits {
            if !matches!(doc.role(*id), Some(ImageRole::Derived(_))) {
                return Err(SessionError::DestinationNotWritable { id: *id });
            }
            let key = doc
                .binding(*id)
                .ok_or(SessionError::MissingImage { id: *id })?;
            let image = self.images.get(key)?;
            let tile_count = image.layout.tile_count();
            let mut last_index = None;
            for (tile_index, new_tile) in edit.edits.iter().copied() {
                if last_index.is_some_and(|last| tile_index <= last) || tile_index >= tile_count {
                    return Err(SessionError::InvalidEditTile {
                        id: *id,
                        tile_index,
                    });
                }
                if new_tile == TileKey::INVALID {
                    return Err(SessionError::InvalidEditTile {
                        id: *id,
                        tile_index,
                    });
                }
                last_index = Some(tile_index);
            }
        }
        Ok(())
    }

    fn apply_derived_edits(
        &mut self,
        doc: &Document,
        edits: Vec<(ImageId, ImageEdit)>,
    ) -> Result<(), SessionError> {
        for (id, edit) in edits {
            let key = doc.binding(id).ok_or(SessionError::MissingImage { id })?;
            for (tile_index, new_tile) in edit.edits {
                let old_tile = self.images.tile(key, tile_index)?;
                self.images.set_tile(key, tile_index, new_tile)?;
                if old_tile != TileKey::INVALID && old_tile != new_tile {
                    self.tiles.discard(old_tile);
                }
            }
        }
        Ok(())
    }

    fn discard_remaining_local_tiles(&mut self) {
        for image in &mut self.local_images {
            match image {
                SessionImage::Raw { tiles, .. } => {
                    for tile in tiles.iter().copied() {
                        if tile != TileKey::INVALID {
                            self.tiles.discard(tile);
                        }
                    }
                    tiles.fill(TileKey::INVALID);
                }
                SessionImage::Edit { edits, .. } => {
                    for (_, tile) in edits.drain(..) {
                        if tile != TileKey::INVALID {
                            self.tiles.discard(tile);
                        }
                    }
                }
            }
        }
    }

    fn push_local_image(&mut self, image: SessionImage) -> GlaLocalImageKey {
        let index = self.local_images.len();
        self.local_images.push(image);
        GlaLocalImageKey(index as u32)
    }

    fn local_image(&self, key: GlaLocalImageKey) -> Result<&SessionImage, SessionError> {
        self.local_images
            .get(key.0 as usize)
            .ok_or(SessionError::MissingImage {
                id: ImageId::new(u64::from(key.0)),
            })
    }

    fn local_image_mut(
        &mut self,
        key: GlaLocalImageKey,
    ) -> Result<&mut SessionImage, SessionError> {
        self.local_images
            .get_mut(key.0 as usize)
            .ok_or(SessionError::MissingImage {
                id: ImageId::new(u64::from(key.0)),
            })
    }

    fn layout_of(&self, key: SessionImageKey) -> Result<GlaImageLayout, SessionError> {
        match key {
            SessionImageKey::Doc(key) => Ok(self.images.get(key)?.layout),
            SessionImageKey::Local(key) => Ok(self.local_image(key)?.layout()),
        }
    }

    fn layout_of_id(&self, id: ImageId) -> Result<GlaImageLayout, SessionError> {
        let key = self
            .local_keys
            .get(&id)
            .map(|entry| entry.key)
            .or_else(|| {
                self.doc_bindings
                    .get(&id)
                    .copied()
                    .map(SessionImageKey::Doc)
            })
            .ok_or(SessionError::MissingImage { id })?;
        self.layout_of(key)
    }

    fn record_frame_draw_dirty(&mut self, draw_on_index: usize, tile_index: u32) {
        if let Some(tiles) = self.frame_draw_dirty.get_mut(draw_on_index) {
            tiles.insert(tile_index);
        }
    }

    fn record_doc_dirty(&mut self, id: ImageId, dirty: &TileSet) {
        if self.doc_write_ids.contains(&id) {
            self.doc_dirty.entry(id).or_default().union_assign(dirty);
        }
    }

    fn read_session_tile(
        &self,
        image: SessionImageKey,
        tile_index: u32,
    ) -> Result<TileKey, SessionError> {
        match image {
            SessionImageKey::Doc(key) => Ok(self.images.tile(key, tile_index)?),
            SessionImageKey::Local(key) => match self.local_image(key)? {
                SessionImage::Raw { tiles, layout, .. } => tiles
                    .get(tile_index as usize)
                    .copied()
                    .ok_or(SessionError::TileIndexOutOfBounds {
                        tile_index,
                        tile_count: layout.tile_count(),
                    }),
                SessionImage::Edit { source, edits, .. } => {
                    match edits.binary_search_by_key(&tile_index, |(index, _)| *index) {
                        Ok(index) => Ok(edits[index].1),
                        Err(_) => Ok(self.images.tile(*source, tile_index)?),
                    }
                }
            },
        }
    }

    fn write_session_tile(
        &mut self,
        image: SessionImageKey,
        tile_index: u32,
    ) -> Result<TileKey, SessionError> {
        match image {
            SessionImageKey::Local(local_key) => {
                match self.local_image(local_key)? {
                    SessionImage::Raw { tiles, layout, .. } => {
                        return tiles.get(tile_index as usize).copied().ok_or(
                            SessionError::TileIndexOutOfBounds {
                                tile_index,
                                tile_count: layout.tile_count(),
                            },
                        );
                    }
                    SessionImage::Edit { source, edits, .. } => {
                        match edits.binary_search_by_key(&tile_index, |(index, _)| *index) {
                            Ok(index) => return Ok(edits[index].1),
                            Err(index) => {
                                let source = *source;
                                let source_tile = self.images.tile(source, tile_index)?;
                                let new_tile = self.tiles.alloc_from(self.atlas_id)?;
                                if source_tile != TileKey::INVALID {
                                    let src = self.tiles.acquire_for_read(source_tile)?;
                                    let dst = self.tiles.acquire_for_write(new_tile)?;
                                    self.renderer.copy(src, dst);
                                } else {
                                    // INVALID has no source content to copy. This is correct under
                                    // current invariants: document derived images are not shadowed
                                    // as DrawOn primitives, and derived commands fully overwrite
                                    // destination tiles. If either invariant changes, first-edit
                                    // initialization must become command-aware or materialize
                                    // source first.
                                }
                                if let SessionImage::Edit { edits, .. } =
                                    self.local_image_mut(local_key)?
                                {
                                    edits.insert(index, (tile_index, new_tile));
                                }
                                return Ok(new_tile);
                            }
                        }
                    }
                }
            }
            SessionImageKey::Doc(key) => {
                let id = self
                    .key_to_id
                    .get(&key)
                    .copied()
                    .ok_or(SessionError::MissingImage {
                        id: ImageId::new(0),
                    })?;
                if !matches!(self.doc_roles.get(&id), Some(ImageRole::Derived(_))) {
                    return Err(SessionError::DestinationNotWritable { id });
                }

                let existing = self.images.tile(key, tile_index)?;
                if existing != TileKey::INVALID {
                    return Ok(existing);
                }

                let new_key = self.tiles.alloc_from(self.atlas_id)?;
                self.images.set_tile(key, tile_index, new_key)?;
                Ok(new_key)
            }
        }
    }

    fn upload_dirty_from(&mut self, id: ImageId, dirty: &TileSet) -> Result<(), SessionError> {
        self.record_doc_dirty(id, dirty);
        if id == self.doc_root {
            self.root_demand.union_assign(dirty);
        }

        for index in 0..self.dirty_edges.len() {
            let edge = self.dirty_edges[index];
            if edge.src != id {
                continue;
            }

            if self.can_upload_dirty_without_projection(edge)? {
                self.upload_dirty_from(edge.dst, dirty)?;
            } else {
                let mut projected = TileSet::default();
                self.upload_dirty_edge(dirty, edge, &mut projected)?;
                if !projected.is_empty() {
                    self.upload_dirty_from(edge.dst, &projected)?;
                }
            }
        }

        Ok(())
    }

    fn can_upload_dirty_without_projection(&self, edge: DirtyEdge) -> Result<bool, SessionError> {
        Ok(matches!(
            (edge.mapping, edge.modifier),
            (Mapping::Identity, FootprintModifier::None)
        ) && self.layout_of_id(edge.src)? == self.layout_of_id(edge.dst)?)
    }

    fn upload_dirty_edge(
        &self,
        src_dirty: &TileSet,
        edge: DirtyEdge,
        dst_dirty: &mut TileSet,
    ) -> Result<(), SessionError> {
        dst_dirty.clear();
        match (edge.mapping, edge.modifier) {
            (Mapping::Identity, FootprintModifier::None) => {
                let dst_tile_count = self.layout_of_id(edge.dst)?.tile_count();
                match src_dirty {
                    TileSet::Full => *dst_dirty = TileSet::Full,
                    TileSet::Tiles(tiles) => {
                        for tile in tiles.iter().copied() {
                            if tile < dst_tile_count {
                                dst_dirty.insert(tile);
                            }
                        }
                    }
                }
            }
            // First version keeps the mapping seam explicit and falls back to a
            // conservative upload until expanded/matrix footprints are wired.
            (Mapping::Identity, FootprintModifier::Expand(_)) | (Mapping::Matrix(_), _) => {
                *dst_dirty = TileSet::Full;
            }
        }
        Ok(())
    }

    fn render_root_demand(&mut self) -> Result<(), SessionError> {
        let root_key = self
            .local_keys
            .get(&self.doc_root)
            .map(|e| e.key)
            .or_else(|| {
                self.doc_bindings
                    .get(&self.doc_root)
                    .copied()
                    .map(SessionImageKey::Doc)
            })
            .ok_or(SessionError::MissingImage { id: self.doc_root })?;

        let root_layout = self.layout_of(root_key)?;
        let tile_count = root_layout.tile_count();
        let demand = std::mem::take(&mut self.root_demand);
        match demand {
            TileSet::Full => {
                for tile in 0..tile_count {
                    self.render_impl(root_key, tile)?;
                }
                self.root_demand = TileSet::default();
            }
            TileSet::Tiles(mut tiles) => {
                for tile in tiles.iter().copied() {
                    self.render_impl(root_key, tile)?;
                }
                tiles.clear();
                self.root_demand = TileSet::Tiles(tiles);
            }
        }
        Ok(())
    }

    fn render_impl(
        &mut self,
        key: SessionImageKey,
        tile_index: u32,
    ) -> Result<TileKey, SessionError> {
        if let SessionImageKey::Local(_) = key {
            if let Some(cmd) = self.local_commands.get(&key).cloned() {
                // Local shadows are session-owned execution results. A shadow
                // with a command must be recomputed on demand even when its tile
                // slot currently holds a valid key shared from the source image.
                // This keeps CoW resource sharing out of command semantics. The
                // tradeoff is possible repeated passes for expanded/matrix
                // mappings until local derived caching is made more precise.
                cmd.exec_tile(self, tile_index)?;
            }
            return self.read_session_tile(key, tile_index);
        }

        let SessionImageKey::Doc(doc_key) = key else {
            unreachable!("local keys return above")
        };

        let id = self
            .key_to_id
            .get(&doc_key)
            .copied()
            .ok_or(SessionError::MissingImage {
                id: ImageId::new(0),
            })?;

        if let Some(role) = self.doc_roles.get(&id) {
            match role {
                ImageRole::Primitive => {
                    return Ok(self.images.tile(doc_key, tile_index)?);
                }
                ImageRole::Derived(command) => {
                    let tile = self.images.tile(doc_key, tile_index)?;
                    if tile != TileKey::INVALID {
                        return Ok(tile);
                    }
                    let ops = self.lower_graph_command(command)?;
                    let layout = self.layout_of(key)?;
                    let cmd = DeriveCommand::new(key, layout, ops);
                    cmd.exec_tile(self, tile_index)?;
                    return Ok(self.images.tile(doc_key, tile_index)?);
                }
            }
        }

        Err(SessionError::MissingImage { id })
    }

    fn lower_graph_command(
        &self,
        command: &GraphCommand,
    ) -> Result<Vec<Derive<SessionImageKey>>, SessionError> {
        let mut ops = Vec::new();
        for read in &command.reads {
            let src_key = self
                .local_keys
                .get(&read.image)
                .map(|e| e.key)
                .or_else(|| {
                    self.doc_bindings
                        .get(&read.image)
                        .copied()
                        .map(SessionImageKey::Doc)
                })
                .ok_or(SessionError::MissingImage { id: read.image })?;
            let layout = self.layout_of(src_key)?;
            let image_ref = ImageRef::with_footprint(src_key, layout, read.mapping, read.modifier);
            ops.push(Derive::Copy(gla_image_command::Copy::new(image_ref)));
        }
        Ok(ops)
    }

    fn lower_session_command(
        &self,
        command: &SessionCommand,
    ) -> Result<Vec<Derive<SessionImageKey>>, SessionError> {
        let mut ops = Vec::new();
        for read in &command.reads {
            let id = read.image.id();
            let src_key = match read.image {
                SessionReadImage::Current(id) => {
                    self.local_keys.get(&id).map(|e| e.key).or_else(|| {
                        self.doc_bindings
                            .get(&id)
                            .copied()
                            .map(SessionImageKey::Doc)
                    })
                }
                SessionReadImage::Backup(id) => self
                    .doc_bindings
                    .get(&id)
                    .copied()
                    .map(SessionImageKey::Doc),
            }
            .ok_or(SessionError::MissingImage { id })?;
            let layout = self.layout_of(src_key)?;
            let image_ref = ImageRef::with_footprint(src_key, layout, read.mapping, read.modifier);
            ops.push(Derive::Copy(gla_image_command::Copy::new(image_ref)));
        }
        Ok(ops)
    }
}

impl RenderCtx for DrawSession {
    type ImageKey = SessionImageKey;
    type Error = SessionError;

    fn render(&mut self, image: SessionImageKey, tile_index: u32) -> Result<TileKey, Self::Error> {
        self.render_impl(image, tile_index)
    }

    fn write_tile(
        &mut self,
        image: SessionImageKey,
        tile_index: u32,
    ) -> Result<TileKey, Self::Error> {
        self.write_session_tile(image, tile_index)
    }

    fn acquire_for_read(&mut self, key: TileKey) -> Result<atlas::TilePos, Self::Error> {
        Ok(self.tiles.acquire_for_read(key)?)
    }

    fn acquire_for_write(&mut self, key: TileKey) -> Result<atlas::TilePos, Self::Error> {
        Ok(self.tiles.acquire_for_write(key)?)
    }

    fn renderer(&mut self) -> &mut Renderer {
        &mut self.renderer
    }
}

fn apply_image_edit_patch(
    doc: &Document,
    images: &mut GlaImages,
    edits: &HashMap<ImageId, ImageEdit>,
) -> Result<HashMap<ImageId, ImageEdit>, SessionError> {
    struct PreparedEdit {
        id: ImageId,
        key: GlaImageKey,
        inverse: ImageEdit,
        forward: Vec<(u32, TileKey)>,
    }

    let mut prepared = Vec::new();
    for (id, edit) in edits {
        if !matches!(doc.role(*id), Some(ImageRole::Primitive)) {
            return Err(SessionError::DestinationNotWritable { id: *id });
        }
        let key = doc
            .binding(*id)
            .ok_or(SessionError::MissingImage { id: *id })?;
        let image = images.get(key)?;
        let tile_count = image.layout.tile_count();
        let mut inverse = ImageEdit {
            source: key,
            edits: Vec::with_capacity(edit.edits.len()),
        };
        let mut last_index = None;
        for (tile_index, new_tile) in edit.edits.iter().copied() {
            if last_index.is_some_and(|last| tile_index <= last) || tile_index >= tile_count {
                return Err(SessionError::InvalidEditTile {
                    id: *id,
                    tile_index,
                });
            }
            if new_tile == TileKey::INVALID {
                return Err(SessionError::InvalidEditTile {
                    id: *id,
                    tile_index,
                });
            }
            let old_tile = image.tiles[tile_index as usize];
            if old_tile == TileKey::INVALID {
                return Err(SessionError::PrimitiveImageHasInvalidTile {
                    id: *id,
                    tile_index,
                });
            }
            inverse.edits.push((tile_index, old_tile));
            last_index = Some(tile_index);
        }
        prepared.push(PreparedEdit {
            id: *id,
            key,
            inverse,
            forward: edit.edits.clone(),
        });
    }

    let mut inverse = HashMap::new();
    for edit in prepared {
        for (tile_index, new_tile) in edit.forward {
            images.set_tile(edit.key, tile_index, new_tile)?;
        }
        if !edit.inverse.edits.is_empty() {
            inverse.insert(edit.id, edit.inverse);
        }
    }
    Ok(inverse)
}

fn validate_doc_image_uses(
    doc_roles: &HashMap<ImageId, ImageRole>,
    doc_images: &[DocImageUse],
) -> Result<HashMap<ImageId, DocumentImageAccess>, SessionError> {
    let mut access = HashMap::new();
    for image_use in doc_images {
        if access
            .insert(image_use.id, image_use.access.clone())
            .is_some()
        {
            return Err(SessionError::DuplicateDocImageUse { id: image_use.id });
        }
        let role = doc_roles
            .get(&image_use.id)
            .ok_or(SessionError::MissingImage { id: image_use.id })?;
        if image_use.access == DocumentImageAccess::ReadWrite && !role.is_primitive() {
            return Err(SessionError::ReadWriteRequiresPrimitive { id: image_use.id });
        }
    }
    Ok(access)
}

fn resolve_session_declarations(
    doc_roles: &HashMap<ImageId, ImageRole>,
    doc_bindings: &HashMap<ImageId, GlaImageKey>,
    images: &GlaImages,
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    session_images: &[SessionImageDecl],
) -> Result<HashMap<ImageId, LocalImageDeclaration>, SessionError> {
    let all_session_ids: HashSet<ImageId> =
        session_images.iter().map(SessionImageDecl::id).collect();
    let mut resolved = HashMap::new();
    for declaration in session_images {
        let id = declaration.id();
        if resolved.contains_key(&id) {
            return Err(SessionError::DuplicateSessionImage { id });
        }
        if doc_access.get(&id) == Some(&DocumentImageAccess::ReadWrite) {
            return Err(SessionError::SessionImageConflictsWithReadWriteDoc { id });
        }
        let image_decl = match declaration {
            SessionImageDecl::Primitive { format, layout, .. } => {
                let f = resolve_format(
                    format,
                    doc_roles,
                    doc_bindings,
                    images,
                    &resolved,
                    &all_session_ids,
                )?;
                let l = resolve_layout(
                    layout,
                    doc_roles,
                    doc_bindings,
                    images,
                    &resolved,
                    &all_session_ids,
                )?;
                LocalImageDeclaration::primitive(f, l)
            }
            SessionImageDecl::Derived {
                format,
                layout,
                command,
                ..
            } => {
                let f = resolve_format(
                    format,
                    doc_roles,
                    doc_bindings,
                    images,
                    &resolved,
                    &all_session_ids,
                )?;
                let l = resolve_layout(
                    layout,
                    doc_roles,
                    doc_bindings,
                    images,
                    &resolved,
                    &all_session_ids,
                )?;
                LocalImageDeclaration::derived(f, l, command.clone())
            }
        };
        resolved.insert(id, image_decl);
    }
    Ok(resolved)
}

fn resolve_format(
    format: &MetadataRef<gla_color::GlaFormat>,
    doc_roles: &HashMap<ImageId, ImageRole>,
    doc_bindings: &HashMap<ImageId, GlaImageKey>,
    images: &GlaImages,
    session_decls: &HashMap<ImageId, LocalImageDeclaration>,
    all_session_ids: &HashSet<ImageId>,
) -> Result<gla_color::GlaFormat, SessionError> {
    match format {
        MetadataRef::Concrete(f) => Ok(*f),
        MetadataRef::Like(id) => {
            if let Some(decl) = session_decls.get(id) {
                Ok(decl.format())
            } else if doc_roles.contains_key(id) {
                let key = doc_bindings
                    .get(id)
                    .copied()
                    .ok_or(SessionError::MissingImage { id: *id })?;
                Ok(images.get(key)?.format)
            } else if all_session_ids.contains(id) {
                Err(SessionError::LikeReferenceNotDeclaredYet { id: *id })
            } else {
                Err(SessionError::LikeReferenceUnknown { id: *id })
            }
        }
    }
}

fn resolve_layout(
    layout: &MetadataRef<GlaImageLayout>,
    doc_roles: &HashMap<ImageId, ImageRole>,
    doc_bindings: &HashMap<ImageId, GlaImageKey>,
    images: &GlaImages,
    session_decls: &HashMap<ImageId, LocalImageDeclaration>,
    all_session_ids: &HashSet<ImageId>,
) -> Result<GlaImageLayout, SessionError> {
    match layout {
        MetadataRef::Concrete(l) => Ok(*l),
        MetadataRef::Like(id) => {
            if let Some(decl) = session_decls.get(id) {
                Ok(decl.layout())
            } else if doc_roles.contains_key(id) {
                let key = doc_bindings
                    .get(id)
                    .copied()
                    .ok_or(SessionError::MissingImage { id: *id })?;
                Ok(images.get(key)?.layout)
            } else if all_session_ids.contains(id) {
                Err(SessionError::LikeReferenceNotDeclaredYet { id: *id })
            } else {
                Err(SessionError::LikeReferenceUnknown { id: *id })
            }
        }
    }
}

fn collect_write_starts(
    doc_roles: &HashMap<ImageId, ImageRole>,
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    session_decls: &HashMap<ImageId, LocalImageDeclaration>,
    ir: &DrawSessionIR,
) -> Result<Vec<ImageId>, SessionError> {
    let mut writers: HashSet<ImageId> = session_decls
        .iter()
        .filter_map(|(id, decl)| match decl {
            LocalImageDeclaration::Derived { .. } => Some(*id),
            LocalImageDeclaration::Primitive { .. } => None,
        })
        .collect();
    let mut write_starts = Vec::new();

    for cmd in &ir.draw_on {
        let id = resolve_draw_on_target(doc_access, session_decls, cmd.dst)?;
        if !writers.insert(id) {
            return Err(SessionError::DuplicateWriter { id });
        }
        write_starts.push(id);
    }

    for cmd in &ir.derive {
        let id = if session_decls.contains_key(&cmd.dst) {
            cmd.dst
        } else if doc_access.get(&cmd.dst) == Some(&DocumentImageAccess::ReadWrite) {
            let role = doc_roles
                .get(&cmd.dst)
                .ok_or(SessionError::MissingImage { id: cmd.dst })?;
            if role.is_derived() {
                return Err(SessionError::CannotShadowDocDerived { id: cmd.dst });
            }
            cmd.dst
        } else {
            return Err(SessionError::DestinationNotWritable { id: cmd.dst });
        };

        for read in &cmd.command.reads {
            if let SessionReadImage::Current(read_id) = read.image {
                if read_id == id {
                    return Err(SessionError::DeriveReadsDestinationCurrent { id });
                }
            }
        }
        for read in &cmd.command.reads {
            match read.image {
                SessionReadImage::Current(read_id) => {
                    if !session_decls.contains_key(&read_id) && !doc_access.contains_key(&read_id) {
                        return Err(SessionError::CurrentReadNotDeclared { id: read_id });
                    }
                }
                SessionReadImage::Backup(read_id) => {
                    if !doc_access.contains_key(&read_id) {
                        return Err(SessionError::BackupReadNotDeclared { id: read_id });
                    }
                }
            }
        }

        if !writers.insert(id) {
            return Err(SessionError::DuplicateWriter { id });
        }
        write_starts.push(id);
    }

    Ok(write_starts)
}

fn resolve_draw_on_target(
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    session_decls: &HashMap<ImageId, LocalImageDeclaration>,
    id: ImageId,
) -> Result<ImageId, SessionError> {
    if session_decls.contains_key(&id) {
        return Ok(id);
    }
    if doc_access.get(&id) == Some(&DocumentImageAccess::ReadWrite) {
        return Ok(id);
    }
    Err(SessionError::DestinationNotWritable { id })
}

fn compute_active_chain(
    root: ImageId,
    doc_roles: &HashMap<ImageId, ImageRole>,
    write_starts: &[ImageId],
) -> Vec<ImageId> {
    let mut chain = HashSet::new();
    let mut stack: Vec<ImageId> = write_starts.iter().copied().collect();
    while let Some(id) = stack.pop() {
        if !chain.insert(id) {
            continue;
        }
        if id == root {
            continue;
        }
        for (did, role) in doc_roles {
            if let ImageRole::Derived(command) = role {
                if command.reads.iter().any(|r| r.image == id) {
                    stack.push(*did);
                }
            }
        }
    }
    chain.insert(root);
    let mut sorted: Vec<ImageId> = chain.into_iter().collect();
    sorted.sort_unstable_by_key(|id| id.value());
    sorted
}

fn collect_dirty_edges(
    doc_roles: &HashMap<ImageId, ImageRole>,
    session_decls: &HashMap<ImageId, LocalImageDeclaration>,
    ir: &DrawSessionIR,
) -> Vec<DirtyEdge> {
    let mut edges = Vec::new();

    for (dst, decl) in session_decls {
        if let LocalImageDeclaration::Derived { command, .. } = decl {
            collect_session_command_dirty_edges(*dst, command, &mut edges);
        }
    }
    for cmd in &ir.derive {
        collect_session_command_dirty_edges(cmd.dst, &cmd.command, &mut edges);
    }
    for (dst, role) in doc_roles {
        if let ImageRole::Derived(command) = role {
            for read in &command.reads {
                edges.push(DirtyEdge {
                    src: read.image,
                    dst: *dst,
                    mapping: read.mapping,
                    modifier: read.modifier,
                });
            }
        }
    }

    edges
}

fn collect_session_command_dirty_edges(
    dst: ImageId,
    command: &SessionCommand,
    edges: &mut Vec<DirtyEdge>,
) {
    for read in &command.reads {
        if let SessionReadImage::Current(src) = read.image {
            edges.push(DirtyEdge {
                src,
                dst,
                mapping: read.mapping,
                modifier: read.modifier,
            });
        }
    }
}

fn input_to_tile_index(_mapping: Mapping, input: CanvasInput, layout: GlaImageLayout) -> u32 {
    let tx = (input.x / IMAGE_TILE_SIZE as f32) as u32;
    let ty = (input.y / IMAGE_TILE_SIZE as f32) as u32;
    let idx = ty * layout.tile_count_x() + tx;
    idx.min(layout.tile_count().saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gla_color::{ChannelCount, ChannelType, GlaFormat};
    use gla_renderer::Pass;

    fn format() -> GlaFormat {
        GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::U8,
        }
    }

    fn layout() -> GlaImageLayout {
        GlaImageLayout::new(64, 64)
    }

    fn new_test_atlas(tiles: &mut Tiles) -> u8 {
        let mut textures = atlas::NoAtlasTextures;
        tiles
            .new_atlas(atlas::AtlasLayout::TINY8, format(), &mut textures)
            .unwrap()
    }

    #[test]
    fn draw_on_cow_shadow_allocates_fresh_tile_before_write() {
        let root = ImageId::new(1);
        let mut images = GlaImages::new();
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let doc_key = images
            .alloc(format(), layout(), &mut tiles, atlas_id)
            .unwrap();
        let source_tile = images.tile(doc_key, 0).unwrap();
        let source_pos = tiles.acquire_for_write(source_tile).unwrap();
        let doc = Document::new(
            root,
            HashMap::from([(root, ImageRole::Primitive)]),
            HashMap::from([(root, doc_key)]),
        )
        .unwrap();
        let ir = DrawSessionIR {
            expected_document_version: doc.version(),
            doc_images: vec![DocImageUse::read_write(root)],
            session_images: Vec::new(),
            draw_on: vec![DrawOnCommand::new(root)],
            derive: Vec::new(),
        };

        let mut session =
            DrawSession::new(ir, &doc, images, tiles, Renderer::new(), atlas_id).unwrap();
        let shadow_key = session.local_keys.get(&root).unwrap().key;
        assert!(matches!(shadow_key, SessionImageKey::Local(_)));
        assert_eq!(
            session.read_session_tile(shadow_key, 0).unwrap(),
            source_tile
        );

        session
            .draw_dab(CanvasInput {
                x: 0.0,
                y: 0.0,
                pressure: 1.0,
            })
            .unwrap();

        assert_eq!(session.images.tile(doc_key, 0).unwrap(), source_tile);
        let shadow_tile = session.read_session_tile(shadow_key, 0).unwrap();
        assert_ne!(shadow_tile, source_tile);
        let shadow_pos = session.tiles.acquire_for_read(shadow_tile).unwrap();
        assert_eq!(
            session.pending_render_passes(),
            &[
                Pass::Copy {
                    src: source_pos,
                    dst: shadow_pos,
                },
                Pass::Clear { dst: shadow_pos },
            ]
        );
        session.flush_frame().unwrap();
        assert_eq!(
            session.doc_dirty(),
            &HashMap::from([(root, TileSet::single(0))])
        );
    }

    #[test]
    fn draw_dab_defers_root_render_until_frame_flush_and_merges_frame_dirty() {
        let paint = ImageId::new(1);
        let root = ImageId::new(2);
        let mut images = GlaImages::new();
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let paint_key = images
            .alloc(format(), layout(), &mut tiles, atlas_id)
            .unwrap();
        let root_key = images.insert_invalid(format(), layout()).unwrap();
        let doc = Document::new(
            root,
            HashMap::from([
                (paint, ImageRole::Primitive),
                (
                    root,
                    ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(paint)])),
                ),
            ]),
            HashMap::from([(paint, paint_key), (root, root_key)]),
        )
        .unwrap();
        let ir = DrawSessionIR {
            expected_document_version: doc.version(),
            doc_images: vec![DocImageUse::read_write(paint)],
            session_images: Vec::new(),
            draw_on: vec![DrawOnCommand::new(paint)],
            derive: Vec::new(),
        };

        let mut session =
            DrawSession::new(ir, &doc, images, tiles, Renderer::new(), atlas_id).unwrap();
        session
            .draw_dab(CanvasInput {
                x: 0.0,
                y: 0.0,
                pressure: 1.0,
            })
            .unwrap();
        session
            .draw_dab(CanvasInput {
                x: 0.0,
                y: 0.0,
                pressure: 1.0,
            })
            .unwrap();

        assert_eq!(
            session
                .pending_render_passes()
                .iter()
                .filter(|pass| matches!(pass, Pass::Clear { .. }))
                .count(),
            2
        );
        assert_eq!(
            session
                .pending_render_passes()
                .iter()
                .filter(|pass| matches!(pass, Pass::Copy { .. }))
                .count(),
            1
        );

        session.flush_frame().unwrap();

        assert_eq!(
            session
                .pending_render_passes()
                .iter()
                .filter(|pass| matches!(pass, Pass::Clear { .. }))
                .count(),
            2
        );
        assert_eq!(
            session
                .pending_render_passes()
                .iter()
                .filter(|pass| matches!(pass, Pass::Copy { .. }))
                .count(),
            2
        );
        assert_eq!(
            session.doc_dirty(),
            &HashMap::from([(paint, TileSet::single(0))])
        );
    }

    #[test]
    fn commit_applies_edit_in_place_and_history_undo_redo_restores_tiles() {
        let root = ImageId::new(1);
        let mut images = GlaImages::new();
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let doc_key = images
            .alloc(format(), layout(), &mut tiles, atlas_id)
            .unwrap();
        let source_tile = images.tile(doc_key, 0).unwrap();
        let doc = Document::new(
            root,
            HashMap::from([(root, ImageRole::Primitive)]),
            HashMap::from([(root, doc_key)]),
        )
        .unwrap();
        let ir = DrawSessionIR {
            expected_document_version: doc.version(),
            doc_images: vec![DocImageUse::read_write(root)],
            session_images: Vec::new(),
            draw_on: vec![DrawOnCommand::new(root)],
            derive: Vec::new(),
        };
        let mut doc = doc;
        let mut history = DrawHistory::new();
        let mut session =
            DrawSession::new(ir, &doc, images, tiles, Renderer::new(), atlas_id).unwrap();

        session
            .draw_dab(CanvasInput {
                x: 0.0,
                y: 0.0,
                pressure: 1.0,
            })
            .unwrap();
        session.flush_frame().unwrap();
        let committed = session.commit(&mut doc, &mut history).unwrap();
        let record_id = committed.commit.record_id;
        let mut images = committed.images;

        assert_eq!(doc.binding(root), Some(doc_key));
        assert_eq!(doc.version(), DocumentVersionId::new(1));
        let edited_tile = images.tile(doc_key, 0).unwrap();
        assert_ne!(edited_tile, source_tile);
        let stored = history.patches.get(&record_id).unwrap();
        assert_eq!(
            stored.edits.get(&root).unwrap().edits,
            vec![(0, source_tile)]
        );

        let redo_id = history
            .apply_stored_patch(record_id, &mut doc, &mut images)
            .unwrap();
        assert_eq!(doc.version(), DocumentVersionId::new(2));
        assert_eq!(images.tile(doc_key, 0).unwrap(), source_tile);

        history
            .apply_stored_patch(redo_id, &mut doc, &mut images)
            .unwrap();
        assert_eq!(doc.version(), DocumentVersionId::new(3));
        assert_eq!(images.tile(doc_key, 0).unwrap(), edited_tile);
    }

    #[test]
    fn commit_discards_session_raw_tiles_after_applying_doc_edit() {
        let root = ImageId::new(1);
        let coverage = ImageId::new(2);
        let mut images = GlaImages::new();
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let doc_key = images
            .alloc(format(), layout(), &mut tiles, atlas_id)
            .unwrap();
        let doc = Document::new(
            root,
            HashMap::from([(root, ImageRole::Primitive)]),
            HashMap::from([(root, doc_key)]),
        )
        .unwrap();
        let ir = DrawSessionIR {
            expected_document_version: doc.version(),
            doc_images: vec![DocImageUse::read_write(root)],
            session_images: vec![SessionImageDecl::Primitive {
                id: coverage,
                format: MetadataRef::Concrete(format()),
                layout: MetadataRef::Concrete(layout()),
            }],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: vec![gla_ir::DeriveCommand::new(
                vec![SessionRead::current(coverage)],
                root,
            )],
        };
        let mut doc = doc;
        let mut history = DrawHistory::new();
        let mut session =
            DrawSession::new(ir, &doc, images, tiles, Renderer::new(), atlas_id).unwrap();

        session
            .draw_dab(CanvasInput {
                x: 0.0,
                y: 0.0,
                pressure: 1.0,
            })
            .unwrap();
        session.flush_frame().unwrap();
        let coverage_key = session.local_keys.get(&coverage).unwrap().key;
        let coverage_tile = session.read_session_tile(coverage_key, 0).unwrap();

        let committed = session.commit(&mut doc, &mut history).unwrap();

        assert!(committed.tiles.ensure_valid(coverage_tile).is_err());
        assert_eq!(doc.version(), DocumentVersionId::new(1));
        assert!(
            history
                .patches
                .get(&committed.commit.record_id)
                .unwrap()
                .edits
                .contains_key(&root)
        );
    }

    #[test]
    fn session_image_like_doc_uses_document_metadata() {
        let root = ImageId::new(1);
        let coverage = ImageId::new(2);
        let soft_coverage = ImageId::new(3);
        let doc_layout = GlaImageLayout::new(128, 64);
        let mut images = GlaImages::new();
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let doc_key = images
            .alloc(format(), doc_layout, &mut tiles, atlas_id)
            .unwrap();
        let doc = Document::new(
            root,
            HashMap::from([(root, ImageRole::Primitive)]),
            HashMap::from([(root, doc_key)]),
        )
        .unwrap();
        let ir = DrawSessionIR {
            expected_document_version: doc.version(),
            doc_images: Vec::new(),
            session_images: vec![
                SessionImageDecl::Primitive {
                    id: coverage,
                    format: MetadataRef::Like(root),
                    layout: MetadataRef::Like(root),
                },
                SessionImageDecl::Derived {
                    id: soft_coverage,
                    format: MetadataRef::Like(root),
                    layout: MetadataRef::Like(root),
                    command: SessionCommand::new(Vec::new()),
                },
            ],
            draw_on: Vec::new(),
            derive: Vec::new(),
        };

        let session = DrawSession::new(ir, &doc, images, tiles, Renderer::new(), atlas_id).unwrap();
        let coverage_key = session.local_keys.get(&coverage).unwrap().key;
        let SessionImageKey::Local(coverage_key) = coverage_key else {
            panic!("session image should use local storage")
        };
        let coverage_image = session.local_image(coverage_key).unwrap();
        assert_eq!(coverage_image.format(), format());
        assert_eq!(coverage_image.layout(), doc_layout);
        let soft_key = session.local_keys.get(&soft_coverage).unwrap().key;
        let SessionImageKey::Local(soft_key) = soft_key else {
            panic!("session image should use local storage")
        };
        let soft_image = session.local_image(soft_key).unwrap();
        assert_eq!(soft_image.format(), format());
        assert_eq!(soft_image.layout(), doc_layout);
    }

    #[test]
    fn session_derived_declaration_materializes_when_read() {
        let root = ImageId::new(1);
        let coverage = ImageId::new(2);
        let soft_coverage = ImageId::new(3);
        let mut images = GlaImages::new();
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let doc_key = images
            .alloc(format(), layout(), &mut tiles, atlas_id)
            .unwrap();
        let doc_tile = images.tile(doc_key, 0).unwrap();
        let doc = Document::new(
            root,
            HashMap::from([(root, ImageRole::Primitive)]),
            HashMap::from([(root, doc_key)]),
        )
        .unwrap();
        let ir = DrawSessionIR {
            expected_document_version: doc.version(),
            doc_images: vec![DocImageUse::read_write(root)],
            session_images: vec![
                SessionImageDecl::Primitive {
                    id: coverage,
                    format: MetadataRef::Concrete(format()),
                    layout: MetadataRef::Concrete(layout()),
                },
                SessionImageDecl::Derived {
                    id: soft_coverage,
                    format: MetadataRef::Concrete(format()),
                    layout: MetadataRef::Concrete(layout()),
                    command: SessionCommand::new(vec![SessionRead::current(coverage)]),
                },
            ],
            draw_on: vec![DrawOnCommand::new(coverage)],
            derive: vec![gla_ir::DeriveCommand::new(
                vec![SessionRead::current(soft_coverage)],
                root,
            )],
        };

        let mut session =
            DrawSession::new(ir, &doc, images, tiles, Renderer::new(), atlas_id).unwrap();

        session
            .draw_dab(CanvasInput {
                x: 0.0,
                y: 0.0,
                pressure: 1.0,
            })
            .unwrap();
        session.flush_frame().unwrap();

        let soft_key = session.local_keys.get(&soft_coverage).unwrap().key;
        assert_ne!(
            session.read_session_tile(soft_key, 0).unwrap(),
            TileKey::INVALID
        );
        let root_shadow_key = session.local_keys.get(&root).unwrap().key;
        assert_ne!(
            session.read_session_tile(root_shadow_key, 0).unwrap(),
            doc_tile
        );
        assert_eq!(session.images.tile(doc_key, 0).unwrap(), doc_tile);
    }

    #[test]
    fn session_derived_declaration_is_a_writer() {
        let root = ImageId::new(1);
        let soft_coverage = ImageId::new(2);
        let mut images = GlaImages::new();
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let doc_key = images
            .alloc(format(), layout(), &mut tiles, atlas_id)
            .unwrap();
        let doc = Document::new(
            root,
            HashMap::from([(root, ImageRole::Primitive)]),
            HashMap::from([(root, doc_key)]),
        )
        .unwrap();
        let ir = DrawSessionIR {
            expected_document_version: doc.version(),
            doc_images: Vec::new(),
            session_images: vec![SessionImageDecl::Derived {
                id: soft_coverage,
                format: MetadataRef::Concrete(format()),
                layout: MetadataRef::Concrete(layout()),
                command: SessionCommand::new(Vec::new()),
            }],
            draw_on: Vec::new(),
            derive: vec![gla_ir::DeriveCommand::new(Vec::new(), soft_coverage)],
        };

        let err = match DrawSession::new(ir, &doc, images, tiles, Renderer::new(), atlas_id) {
            Ok(_) => panic!("expected duplicate writer error"),
            Err(err) => err,
        };
        assert!(matches!(
            err,
            SessionError::DuplicateWriter { id } if id == soft_coverage
        ));
    }

    #[test]
    fn input_to_tile_index_uses_image_tile_size_as_divisor() {
        let layout = GlaImageLayout::new(186, 1);

        let input_at = |x: f32| {
            input_to_tile_index(
                Mapping::Identity,
                CanvasInput {
                    x,
                    y: 0.0,
                    pressure: 1.0,
                },
                layout,
            )
        };

        assert_eq!(input_at(0.0), 0, "x=0 in tile 0");
        assert_eq!(input_at(30.0), 0, "x=30 in tile 0");
        assert_eq!(input_at(61.0), 0, "x=61 in tile 0");
        assert_eq!(input_at(62.0), 1, "x=62 in tile 1 (boundary)");
        assert_eq!(input_at(100.0), 1, "x=100 in tile 1");
        assert_eq!(input_at(123.0), 1, "x=123 in tile 1");
        assert_eq!(input_at(124.0), 2, "x=124 in tile 2 (boundary)");
        assert_eq!(input_at(185.0), 2, "x=185 in tile 2");
    }

    #[test]
    fn input_to_tile_index_multi_row_tile_grid() {
        let layout = GlaImageLayout::new(186, 124);
        let tile_count_x = layout.tile_count_x();

        let tile_of = |x: f32, y: f32| -> (u32, u32) {
            let idx = input_to_tile_index(
                Mapping::Identity,
                CanvasInput {
                    x,
                    y,
                    pressure: 1.0,
                },
                layout,
            );
            (idx / tile_count_x, idx % tile_count_x)
        };

        assert_eq!(tile_of(0.0, 0.0), (0, 0));
        assert_eq!(tile_of(61.0, 0.0), (0, 0));
        assert_eq!(tile_of(62.0, 0.0), (0, 1));
        assert_eq!(tile_of(124.0, 0.0), (0, 2));
        assert_eq!(tile_of(0.0, 62.0), (1, 0));
        assert_eq!(tile_of(62.0, 62.0), (1, 1));
        assert_eq!(tile_of(124.0, 62.0), (1, 2));
        assert_eq!(tile_of(62.0, 123.0), (1, 1));
        assert_eq!(tile_of(124.0, 123.0), (1, 2));
    }

    #[test]
    fn input_to_tile_index_clamps_out_of_bounds() {
        let layout = GlaImageLayout::new(62, 62);

        let idx = |x: f32, y: f32| -> u32 {
            input_to_tile_index(
                Mapping::Identity,
                CanvasInput {
                    x,
                    y,
                    pressure: 1.0,
                },
                layout,
            )
        };

        assert_eq!(idx(100.0, 0.0), 0, "x beyond image clamped");
        assert_eq!(idx(0.0, 100.0), 0, "y beyond image clamped");
        assert_eq!(idx(-1.0, 0.0), 0, "negative x clamped");
        assert_eq!(idx(0.0, -1.0), 0, "negative y clamped");
    }
}
