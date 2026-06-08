use gla_doc::{DocError, Document, DrawPatch};
use gla_image::{GlaImageKey, GlaImageLayout, GlaImages, GlaImagesError, ImagesSession, TileSet};
use gla_image_command::{Derive, DeriveCommand, ImageRef, RenderCtx};
use gla_ir::*;
use gla_renderer::Renderer;
use std::collections::{HashMap, HashSet};
use tile_key::{TileKey, Tiles, TilesError, TilesSession};

mod local;

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
    pub version: DocumentVersionId,
}

#[derive(Debug)]
pub enum SessionError {
    Doc(DocError),
    Image(GlaImagesError),
    Tile(TilesError),
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

#[derive(Clone, Copy, Debug)]
struct LocalKeyEntry {
    key: GlaImageKey,
    layout: GlaImageLayout,
}

#[derive(Clone, Debug)]
struct DrawOnInput {
    dst_key: GlaImageKey,
    input_mapping: Mapping,
    _tool: Tool,
    _tool_params: ToolParams,
}

pub struct DrawSession {
    doc_root: ImageId,
    doc_roles: HashMap<ImageId, ImageRole>,
    doc_bindings: HashMap<ImageId, GlaImageKey>,
    local_keys: HashMap<ImageId, LocalKeyEntry>,
    local_commands: HashMap<GlaImageKey, DeriveCommand>,
    key_to_id: HashMap<GlaImageKey, ImageId>,
    active_chain: HashSet<GlaImageKey>,
    draw_inputs: Vec<DrawOnInput>,
    root_dirty: TileSet,
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
        let session_decls =
            resolve_session_declarations(&doc_roles, &doc_access, &ir.session_images)?;

        let write_starts =
            collect_write_starts(&doc_roles, &doc_access, &session_decls, &ir)?;
        let active_ids = compute_active_chain(doc_root, &doc_roles, &write_starts);

        let mut self_ = Self {
            doc_root,
            doc_roles: doc_roles.clone(),
            doc_bindings: doc_bindings.clone(),
            local_keys: HashMap::new(),
            local_commands: HashMap::new(),
            key_to_id: HashMap::new(),
            active_chain: HashSet::new(),
            draw_inputs: Vec::new(),
            root_dirty: TileSet::default(),
            images,
            tiles,
            renderer,
            atlas_id,
        };

        {
            let mut img = ImagesSession::new(&mut self_.images);
            let mut t = TilesSession::new(&mut self_.tiles);
            for id in &active_ids {
                if let Some(old_key) = doc_bindings.get(id).copied() {
                    let new_key = img.copy_on_write(old_key)?;
                    let layout = img.get(new_key)?.layout;
                    self_.local_keys.insert(*id, LocalKeyEntry {
                        key: new_key,
                        layout,
                    });
                    self_.key_to_id.insert(new_key, *id);
                    self_.active_chain.insert(new_key);
                }
            }
            for (id, decl) in &session_decls {
                let (key, layout) = match decl {
                    LocalImageDeclaration::Primitive { format, layout } => {
                        let k = img.alloc(*format, *layout, &mut t, atlas_id)?;
                        (k, *layout)
                    }
                    LocalImageDeclaration::Derived { format, layout, .. } => {
                        let k = img.insert_invalid(*format, *layout)?;
                        (k, *layout)
                    }
                };
                self_.local_keys.insert(*id, LocalKeyEntry { key, layout });
                self_.key_to_id.insert(key, *id);
            }
        }

        for (id, key) in &doc_bindings {
            if !self_.key_to_id.contains_key(key) {
                self_.key_to_id.insert(*key, *id);
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
                    dst_key: entry.key,
                    input_mapping: cmd.input_mapping,
                    _tool: cmd.tool,
                    _tool_params: cmd.tool_params,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        self_.draw_inputs = draw_inputs;

        Ok(self_)
    }

    pub fn draw(&mut self, input: CanvasInput) -> Result<(), SessionError> {
        for di in self.draw_inputs.clone() {
            let layout = self.layout_of(di.dst_key)?;
            let tile_index = input_to_tile_index(di.input_mapping, input, layout);

            {
                let mut img = ImagesSession::new(&mut self.images);
                let mut t = TilesSession::new(&mut self.tiles);
                let existing = img.tile(di.dst_key, tile_index)?;
                let tile_key = if existing == TileKey::INVALID {
                    let new_key = t.alloc_from(self.atlas_id)?;
                    img.set_tile(di.dst_key, tile_index, new_key)?;
                    new_key
                } else {
                    existing
                };
                let pos = t.acquire_for_write(tile_key)?;
                self.renderer.clear(pos);
            }

            self.mark_dirty(di.dst_key, tile_index);
        }
        self.flush_dirty_to_root()
    }

    pub fn commit(self, doc: &mut Document) -> Result<DrawCommit, SessionError> {
        let mut bindings = HashMap::new();
        for (id, entry) in &self.local_keys {
            if self.active_chain.contains(&entry.key) {
                bindings.insert(*id, entry.key);
            }
        }
        let patch = DrawPatch::new(bindings, self.root_dirty.clone());
        doc.commit_draw(patch)?;
        Ok(DrawCommit {
            version: doc.version(),
        })
    }

    pub fn root_dirty(&self) -> &TileSet {
        &self.root_dirty
    }

    fn layout_of(&self, key: GlaImageKey) -> Result<GlaImageLayout, SessionError> {
        let image = self.images.get(key)?;
        Ok(image.layout)
    }

    fn mark_dirty(&mut self, _key: GlaImageKey, tile_index: u32) {
        self.root_dirty.union_assign(&TileSet::single(tile_index));
    }

    fn flush_dirty_to_root(&mut self) -> Result<(), SessionError> {
        let root_key = self
            .local_keys
            .get(&self.doc_root)
            .map(|e| e.key)
            .or_else(|| self.doc_bindings.get(&self.doc_root).copied())
            .ok_or(SessionError::MissingImage {
                id: self.doc_root,
            })?;

        let root_layout = self.layout_of(root_key)?;
        let tile_count = root_layout.tile_count();
        let tiles: Vec<u32> = match &self.root_dirty {
            TileSet::Full => (0..tile_count).collect(),
            TileSet::Tiles(t) => t.clone(),
        };
        for tile in tiles {
            self.render_impl(root_key, tile)?;
        }
        Ok(())
    }

    fn render_impl(
        &mut self,
        key: GlaImageKey,
        tile_index: u32,
    ) -> Result<TileKey, SessionError> {
        if let Some(cmd) = self.local_commands.get(&key).cloned() {
            cmd.exec_tile(self, tile_index)?;
            let tile = {
                let img = ImagesSession::new(&mut self.images);
                img.tile(key, tile_index)?
            };
            return Ok(tile);
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
                    let img = ImagesSession::new(&mut self.images);
                    return Ok(img.tile(key, tile_index)?);
                }
                ImageRole::Derived(command) => {
                    let ops = self.lower_graph_command(command)?;
                    let layout = self.layout_of(key)?;
                    let cmd = DeriveCommand::new(key, layout, ops);
                    self.local_commands.insert(key, cmd.clone());
                    cmd.exec_tile(self, tile_index)?;
                    let img = ImagesSession::new(&mut self.images);
                    return Ok(img.tile(key, tile_index)?);
                }
            }
        }

        Err(SessionError::MissingImage { id })
    }

    fn lower_graph_command(
        &self,
        command: &GraphCommand,
    ) -> Result<Vec<Derive>, SessionError> {
        let mut ops = Vec::new();
        for read in &command.reads {
            let src_key = self
                .local_keys
                .get(&read.image)
                .map(|e| e.key)
                .or_else(|| self.doc_bindings.get(&read.image).copied())
                .ok_or(SessionError::MissingImage { id: read.image })?;
            let layout = self.layout_of(src_key)?;
            let image_ref =
                ImageRef::with_footprint(src_key, layout, read.mapping, read.modifier);
            ops.push(Derive::Copy(gla_image_command::Copy::new(image_ref)));
        }
        Ok(ops)
    }

    fn lower_session_command(
        &self,
        command: &SessionCommand,
    ) -> Result<Vec<Derive>, SessionError> {
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
            let image_ref =
                ImageRef::with_footprint(src_key, layout, read.mapping, read.modifier);
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

    fn write_tile(
        &mut self,
        image: GlaImageKey,
        tile_index: u32,
    ) -> Result<TileKey, Self::Error> {
        let mut img = ImagesSession::new(&mut self.images);
        let mut t = TilesSession::new(&mut self.tiles);
        let existing = img.tile(image, tile_index)?;
        if existing == TileKey::INVALID {
            let new_key = t.alloc_from(self.atlas_id)?;
            img.set_tile(image, tile_index, new_key)?;
            return Ok(new_key);
        }
        Ok(existing)
    }

    fn acquire_for_read(&mut self, key: TileKey) -> Result<atlas::TilePos, Self::Error> {
        let t = TilesSession::new(&mut self.tiles);
        Ok(t.acquire_for_read(key)?)
    }

    fn acquire_for_write(&mut self, key: TileKey) -> Result<atlas::TilePos, Self::Error> {
        let mut t = TilesSession::new(&mut self.tiles);
        Ok(t.acquire_for_write(key)?)
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
            return Err(SessionError::ReadWriteRequiresPrimitive {
                id: image_use.id,
            });
        }
    }
    Ok(access)
}

fn resolve_session_declarations(
    doc_roles: &HashMap<ImageId, ImageRole>,
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
                let f = resolve_format(format, doc_roles, &resolved, &all_session_ids)?;
                let l = resolve_layout(layout, doc_roles, &resolved, &all_session_ids)?;
                LocalImageDeclaration::primitive(f, l)
            }
            SessionImageDecl::Derived {
                format, layout, command, ..
            } => {
                let f = resolve_format(format, doc_roles, &resolved, &all_session_ids)?;
                let l = resolve_layout(layout, doc_roles, &resolved, &all_session_ids)?;
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
    session_decls: &HashMap<ImageId, LocalImageDeclaration>,
    all_session_ids: &HashSet<ImageId>,
) -> Result<gla_color::GlaFormat, SessionError> {
    match format {
        MetadataRef::Concrete(f) => Ok(*f),
        MetadataRef::Like(id) => {
            if let Some(decl) = session_decls.get(id) {
                Ok(decl.format())
            } else if doc_roles.contains_key(id) {
                Err(SessionError::LikeReferenceUnknown { id: *id })
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
    session_decls: &HashMap<ImageId, LocalImageDeclaration>,
    all_session_ids: &HashSet<ImageId>,
) -> Result<GlaImageLayout, SessionError> {
    match layout {
        MetadataRef::Concrete(l) => Ok(*l),
        MetadataRef::Like(id) => {
            if let Some(decl) = session_decls.get(id) {
                Ok(decl.layout())
            } else if doc_roles.contains_key(id) {
                Err(SessionError::LikeReferenceUnknown { id: *id })
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
    let mut writers = HashSet::new();
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
                    if !session_decls.contains_key(&read_id)
                        && !doc_access.contains_key(&read_id)
                    {
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

fn input_to_tile_index(_mapping: Mapping, input: CanvasInput, layout: GlaImageLayout) -> u32 {
    let tx = (input.x / 64.0) as u32;
    let ty = (input.y / 64.0) as u32;
    let idx = ty * layout.tile_count_x() + tx;
    idx.min(layout.tile_count().saturating_sub(1))
}
