use gla_doc::{DocError, Document, DrawPatch, SessionId};
use gla_image::{GlaImageKey, GlaImageLayout, GlaImages, GlaImagesError, TileSet};
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
    pub session_id: SessionId,
    pub version: DocumentVersionId,
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

#[derive(Clone, Copy, Debug)]
enum LocalStorage {
    Owned,
    CopyOnWrite { source: GlaImageKey },
}

#[derive(Clone, Copy, Debug)]
struct LocalKeyEntry {
    key: GlaImageKey,
    layout: GlaImageLayout,
    storage: LocalStorage,
}

#[derive(Clone, Debug)]
struct DrawOnInput {
    dst_id: ImageId,
    dst_key: GlaImageKey,
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
    doc_roles: HashMap<ImageId, ImageRole>,
    doc_bindings: HashMap<ImageId, GlaImageKey>,
    doc_write_ids: HashSet<ImageId>,
    local_keys: HashMap<ImageId, LocalKeyEntry>,
    local_commands: HashMap<GlaImageKey, DeriveCommand>,
    key_to_id: HashMap<GlaImageKey, ImageId>,
    active_chain: HashSet<GlaImageKey>,
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
            doc_roles: doc_roles.clone(),
            doc_bindings: doc_bindings.clone(),
            doc_write_ids,
            local_keys: HashMap::new(),
            local_commands: HashMap::new(),
            key_to_id: HashMap::new(),
            active_chain: HashSet::new(),
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
                    let new_key = self_.images.copy_on_write(old_key)?;
                    let layout = self_.images.get(new_key)?.layout;
                    self_.local_keys.insert(
                        *id,
                        LocalKeyEntry {
                            key: new_key,
                            layout,
                            storage: LocalStorage::CopyOnWrite { source: old_key },
                        },
                    );
                    self_.key_to_id.insert(new_key, *id);
                    self_.active_chain.insert(new_key);
                }
            }
            for (id, decl) in &session_decls {
                let (key, layout) = match decl {
                    LocalImageDeclaration::Primitive { format, layout } => {
                        let k = self_
                            .images
                            .alloc(*format, *layout, &mut self_.tiles, atlas_id)?;
                        (k, *layout)
                    }
                    LocalImageDeclaration::Derived { format, layout, .. } => {
                        let k = self_.images.insert_invalid(*format, *layout)?;
                        (k, *layout)
                    }
                };
                self_.local_keys.insert(
                    *id,
                    LocalKeyEntry {
                        key,
                        layout,
                        storage: LocalStorage::Owned,
                    },
                );
                self_.key_to_id.insert(key, *id);
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
                let tile_key = self.write_cow_tile(di.dst_key, tile_index)?;
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

    pub fn commit(self, doc: &mut Document) -> Result<DrawCommit, SessionError> {
        let mut bindings = HashMap::new();
        let mut tile_keys = Vec::new();

        for (id, entry) in &self.local_keys {
            if self.active_chain.contains(&entry.key) {
                bindings.insert(*id, entry.key);
                if let Some(old_key) = self.doc_bindings.get(id).copied() {
                    let old = self.images.get(old_key)?;
                    let new = self.images.get(entry.key)?;
                    for (old_tile, new_tile) in old.tiles.iter().zip(new.tiles.iter()) {
                        if *old_tile != TileKey::INVALID && old_tile != new_tile {
                            tile_keys.push(*old_tile);
                        }
                    }
                }
            }
        }

        let patch = DrawPatch::new(bindings, self.doc_dirty.clone());
        let patch = DrawPatch { tile_keys, ..patch };
        let session_id = doc.commit_draw(patch)?;
        Ok(DrawCommit {
            session_id,
            version: doc.version(),
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

    fn layout_of(&self, key: GlaImageKey) -> Result<GlaImageLayout, SessionError> {
        let image = self.images.get(key)?;
        Ok(image.layout)
    }

    fn layout_of_id(&self, id: ImageId) -> Result<GlaImageLayout, SessionError> {
        let key = self
            .local_keys
            .get(&id)
            .map(|entry| entry.key)
            .or_else(|| self.doc_bindings.get(&id).copied())
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

    fn local_entry_for_key(&self, key: GlaImageKey) -> Option<&LocalKeyEntry> {
        self.key_to_id
            .get(&key)
            .and_then(|id| self.local_keys.get(id))
            .filter(|entry| entry.key == key)
    }

    fn local_storage_for_key(&self, key: GlaImageKey) -> LocalStorage {
        self.local_entry_for_key(key)
            .map(|entry| entry.storage)
            .unwrap_or(LocalStorage::Owned)
    }

    fn write_cow_tile(
        &mut self,
        image: GlaImageKey,
        tile_index: u32,
    ) -> Result<TileKey, SessionError> {
        let existing = self.images.tile(image, tile_index)?;
        if let LocalStorage::CopyOnWrite { source } = self.local_storage_for_key(image) {
            let source_tile = self.images.tile(source, tile_index)?;
            if existing == source_tile {
                let new_key = self.tiles.alloc_from(self.atlas_id)?;
                self.images.set_tile(image, tile_index, new_key)?;
                return Ok(new_key);
            }
        }

        if existing == TileKey::INVALID {
            let new_key = self.tiles.alloc_from(self.atlas_id)?;
            self.images.set_tile(image, tile_index, new_key)?;
            return Ok(new_key);
        }

        Ok(existing)
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
            .or_else(|| self.doc_bindings.get(&self.doc_root).copied())
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

    fn render_impl(&mut self, key: GlaImageKey, tile_index: u32) -> Result<TileKey, SessionError> {
        if let Some(entry) = self.local_entry_for_key(key) {
            if let Some(cmd) = self.local_commands.get(&key).cloned() {
                // Local shadows are session-owned execution results. A shadow
                // with a command must be recomputed on demand even when its tile
                // slot currently holds a valid key shared from the source image.
                // This keeps CoW resource sharing out of command semantics. The
                // tradeoff is possible repeated passes for expanded/matrix
                // mappings until local derived caching is made more precise.
                cmd.exec_tile(self, tile_index)?;
                return Ok(self.images.tile(key, tile_index)?);
            }
            if matches!(entry.storage, LocalStorage::Owned) {
                return Ok(self.images.tile(key, tile_index)?);
            }
            return Ok(self.images.tile(key, tile_index)?);
        }

        let id = self
            .key_to_id
            .get(&key)
            .copied()
            .ok_or(SessionError::MissingImage {
                id: ImageId::new(0),
            })?;

        if let Some(role) = self.doc_roles.get(&id) {
            match role {
                ImageRole::Primitive => {
                    return Ok(self.images.tile(key, tile_index)?);
                }
                ImageRole::Derived(command) => {
                    let tile = self.images.tile(key, tile_index)?;
                    if tile != TileKey::INVALID {
                        return Ok(tile);
                    }
                    let ops = self.lower_graph_command(command)?;
                    let layout = self.layout_of(key)?;
                    let cmd = DeriveCommand::new(key, layout, ops);
                    cmd.exec_tile(self, tile_index)?;
                    return Ok(self.images.tile(key, tile_index)?);
                }
            }
        }

        Err(SessionError::MissingImage { id })
    }

    fn lower_graph_command(&self, command: &GraphCommand) -> Result<Vec<Derive>, SessionError> {
        let mut ops = Vec::new();
        for read in &command.reads {
            let src_key = self
                .local_keys
                .get(&read.image)
                .map(|e| e.key)
                .or_else(|| self.doc_bindings.get(&read.image).copied())
                .ok_or(SessionError::MissingImage { id: read.image })?;
            let layout = self.layout_of(src_key)?;
            let image_ref = ImageRef::with_footprint(src_key, layout, read.mapping, read.modifier);
            ops.push(Derive::Copy(gla_image_command::Copy::new(image_ref)));
        }
        Ok(ops)
    }

    fn lower_session_command(&self, command: &SessionCommand) -> Result<Vec<Derive>, SessionError> {
        let mut ops = Vec::new();
        for read in &command.reads {
            let id = read.image.id();
            let src_key = match read.image {
                SessionReadImage::Current(id) => self
                    .local_keys
                    .get(&id)
                    .map(|e| e.key)
                    .or_else(|| self.doc_bindings.get(&id).copied()),
                SessionReadImage::Backup(id) => self.doc_bindings.get(&id).copied(),
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
    type Error = SessionError;

    fn render(&mut self, image: GlaImageKey, tile_index: u32) -> Result<TileKey, Self::Error> {
        self.render_impl(image, tile_index)
    }

    fn write_tile(&mut self, image: GlaImageKey, tile_index: u32) -> Result<TileKey, Self::Error> {
        self.write_cow_tile(image, tile_index)
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
    let tx = (input.x / 64.0) as u32;
    let ty = (input.y / 64.0) as u32;
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
        assert_ne!(shadow_key, doc_key);
        assert_eq!(session.images.tile(shadow_key, 0).unwrap(), source_tile);

        session
            .draw_dab(CanvasInput {
                x: 0.0,
                y: 0.0,
                pressure: 1.0,
            })
            .unwrap();

        assert_eq!(session.images.tile(doc_key, 0).unwrap(), source_tile);
        let shadow_tile = session.images.tile(shadow_key, 0).unwrap();
        assert_ne!(shadow_tile, source_tile);
        let shadow_pos = session.tiles.acquire_for_read(shadow_tile).unwrap();
        assert_eq!(
            session.pending_render_passes(),
            &[Pass::Clear { dst: shadow_pos }]
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
            0
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
            1
        );
        assert_eq!(
            session.doc_dirty(),
            &HashMap::from([(paint, TileSet::single(0))])
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
        let coverage_image = session.images.get(coverage_key).unwrap();
        assert_eq!(coverage_image.format, format());
        assert_eq!(coverage_image.layout, doc_layout);
        let soft_key = session.local_keys.get(&soft_coverage).unwrap().key;
        let soft_image = session.images.get(soft_key).unwrap();
        assert_eq!(soft_image.format, format());
        assert_eq!(soft_image.layout, doc_layout);
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
        assert_ne!(session.images.tile(soft_key, 0).unwrap(), TileKey::INVALID);
        let root_shadow_key = session.local_keys.get(&root).unwrap().key;
        assert_ne!(session.images.tile(root_shadow_key, 0).unwrap(), doc_tile);
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
}
