use gla_core::IMAGE_TILE_SIZE;
use gla_image::{GlaImageKey, GlaImageLayout, ImagesSession};
use gla_image_command::{DrawCommand, ImageCommand, ImageCommandRead};
use std::collections::{HashMap, HashSet};
use tile_key::{TileKey, TilesSession};

mod local;

pub use gla_doc::*;
pub use gla_image_command::TileSet;
pub use gla_ir::*;
pub use local::*;

#[derive(Clone, Debug, PartialEq)]
pub struct DrawCommit {
    pub record: Option<SessionRecord>,
    pub root_repaint: TileSet,
    pub discarded_images: Vec<GlaImageKey>,
    pub discarded_tiles: Vec<TileKey>,
}

#[derive(Debug)]
pub struct SessionDocument {
    doc: gla_doc::SessionDocument,
}

impl SessionDocument {
    pub fn new(graph: RegistryGraph, bindings: ImageBindingTable) -> Result<Self, SessionError> {
        Ok(Self {
            doc: gla_doc::SessionDocument::new(graph, bindings)?,
        })
    }

    pub fn active(&self) -> ActiveDocumentState {
        self.doc.active()
    }

    pub fn active_graph(&self) -> Result<&RegistryGraph, SessionError> {
        self.doc.active_graph()
    }

    pub fn active_bindings(&self) -> Result<&ImageBindingTable, SessionError> {
        self.doc.active_bindings()
    }

    pub fn graph(&self, key: RegistryGraphKey) -> Result<&RegistryGraph, SessionError> {
        self.doc.graph(key)
    }

    pub fn bindings(&self, key: ImageBindingTableKey) -> Result<&ImageBindingTable, SessionError> {
        self.doc.bindings(key)
    }

    pub fn root_cache(&self) -> Result<GlaImageKey, SessionError> {
        self.doc.root_cache()
    }

    pub fn apply_registry_patch(
        &mut self,
        patch: &RegistryPatch,
        images: &mut ImagesSession<'_>,
        tiles: &mut TilesSession<'_>,
        options: RegistryPatchOptions,
    ) -> Result<Option<SessionRecord>, SessionError> {
        self.doc.apply_registry_patch(patch, images, tiles, options)
    }

    pub fn apply_registry_patch_with(
        &mut self,
        patch: &RegistryPatch,
        images: &mut ImagesSession<'_>,
        tiles: &mut TilesSession<'_>,
        options: RegistryPatchOptions,
        materializer: &mut impl DerivedMaterializer,
    ) -> Result<Option<SessionRecord>, SessionError> {
        self.doc
            .apply_registry_patch_with(patch, images, tiles, options, materializer)
    }

    pub fn begin_draw_session(
        &self,
        ir: DrawSessionIR,
        images: &mut ImagesSession<'_>,
        tiles: &mut TilesSession<'_>,
        atlas_id: u8,
    ) -> Result<DrawSession, SessionError> {
        let active = self.doc.active();
        if ir.expected_document_version != active.version {
            return Err(SessionError::ExpectedDocumentVersion {
                expected: ir.expected_document_version,
                actual: active.version,
            });
        }

        let graph = self.doc.active_graph()?.clone();
        let bindings_before_key = active.bindings;
        let bindings_before = self.doc.active_bindings()?.clone();
        bindings_before.validate_against_graph(&graph)?;
        gla_doc::validate_bound_images(&graph, &bindings_before, images)?;

        let doc_access = validate_doc_image_uses(&graph, &ir.doc_images)?;
        let session_decls = resolve_session_declarations(&graph, &doc_access, &ir.session_images)?;
        let mut all_derive = derived_session_image_commands(&session_decls);
        all_derive.extend(ir.derive.clone());

        let command_plan = validate_draw_commands(
            &graph,
            &doc_access,
            &session_decls,
            &ir.draw_on,
            &all_derive,
        )?;

        let doc_write_closure = compute_doc_write_closure(&graph, &command_plan.doc_write_starts);
        let root_cache_before = bindings_before
            .get(graph.root())
            .ok_or(SessionError::BindingMissing { id: graph.root() })?;

        let mut doc_current = bindings_before.clone();
        let mut cow_images = HashMap::new();
        for id in &doc_write_closure {
            let old_key = doc_current
                .get(*id)
                .ok_or(SessionError::BindingMissing { id: *id })?;
            let new_key = images.copy_on_write(old_key)?;
            doc_current.insert(*id, new_key);
            cow_images.insert(*id, new_key);
        }

        let mut local = LocalImageTable::default();
        for (id, declaration) in &session_decls {
            let key = match declaration {
                LocalImageDeclaration::Primitive { format, layout } => {
                    images.alloc(*format, *layout, tiles, atlas_id)?
                }
                LocalImageDeclaration::Derived { format, layout, .. } => {
                    images.insert_invalid(*format, *layout)?
                }
            };
            local.insert(
                *id,
                LocalImage {
                    key,
                    declaration: declaration.clone(),
                },
            );
        }

        let reader_edges = build_session_reader_edges(&graph, &doc_access, &local, &all_derive)?;
        let registry_command_dst = graph
            .analysis()
            .commands
            .iter()
            .enumerate()
            .map(|(index, node)| (CommandIndex(index), node.dst))
            .collect();
        let registry_commands = graph
            .analysis()
            .commands
            .iter()
            .enumerate()
            .map(|(index, node)| (CommandIndex(index), node.command.clone()))
            .collect();
        let execution_order =
            build_session_execution_order(&graph, &doc_access, &local, &all_derive)?;

        Ok(DrawSession {
            graph: active.graph,
            bindings_before: bindings_before_key,
            root: graph.root(),
            doc_start: bindings_before,
            doc_current,
            doc_access,
            local,
            draw_on: ir.draw_on,
            derive_commands: all_derive,
            reader_edges,
            registry_command_dst,
            registry_commands,
            execution_order,
            pending_by_command: HashMap::new(),
            doc_write_closure,
            cow_images,
            document_dirty: HashMap::new(),
            record_dirty: HashMap::new(),
            local_dirty: HashMap::new(),
            root_repaint: TileSet::default(),
            root_cache_before,
        })
    }

    pub fn undo(&mut self, record: &SessionRecord) -> Result<RepaintDemand, SessionError> {
        self.doc.undo(record)
    }

    pub fn redo(&mut self, record: &SessionRecord) -> Result<RepaintDemand, SessionError> {
        self.doc.redo(record)
    }

    fn commit_draw(
        &mut self,
        graph: RegistryGraphKey,
        bindings_before: ImageBindingTableKey,
        bindings_after: ImageBindingTable,
        doc_dirty: Vec<(ImageId, TileSet)>,
        root_cache_before: GlaImageKey,
    ) -> Result<SessionRecord, SessionError> {
        self.doc.commit_draw(
            graph,
            bindings_before,
            bindings_after,
            doc_dirty,
            root_cache_before,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum ResolvedTarget {
    Local(ImageId),
    Document(ImageId),
}

impl ResolvedTarget {
    fn id(self) -> ImageId {
        match self {
            Self::Local(id) | Self::Document(id) => id,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum SessionCommandRef {
    LocalDerive(usize),
    Registry(CommandIndex),
}

#[derive(Clone, Debug)]
struct SessionReadEdge {
    command: SessionCommandRef,
    mapping: Mapping,
    modifier: FootprintModifier,
    dst_layout: GlaImageLayout,
}

#[derive(Debug)]
pub struct DrawSession {
    graph: RegistryGraphKey,
    bindings_before: ImageBindingTableKey,
    root: ImageId,
    doc_start: ImageBindingTable,
    doc_current: ImageBindingTable,
    doc_access: HashMap<ImageId, DocumentImageAccess>,
    local: LocalImageTable,
    draw_on: Vec<DrawOnCommand>,
    derive_commands: Vec<DeriveCommand>,
    reader_edges: HashMap<ResolvedTarget, Vec<SessionReadEdge>>,
    registry_command_dst: HashMap<CommandIndex, ImageId>,
    registry_commands: HashMap<CommandIndex, GraphCommand>,
    execution_order: Vec<SessionCommandRef>,
    pending_by_command: HashMap<SessionCommandRef, TileSet>,
    doc_write_closure: HashSet<ImageId>,
    cow_images: HashMap<ImageId, GlaImageKey>,
    document_dirty: HashMap<ImageId, TileSet>,
    record_dirty: HashMap<ImageId, TileSet>,
    local_dirty: HashMap<ImageId, TileSet>,
    root_repaint: TileSet,
    root_cache_before: GlaImageKey,
}

impl DrawSession {
    pub fn draw_on_commands(&self) -> &[DrawOnCommand] {
        &self.draw_on
    }

    pub fn derive_commands(&self) -> &[DeriveCommand] {
        &self.derive_commands
    }

    pub fn local_images(&self) -> &HashMap<ImageId, LocalImage> {
        self.local.as_map()
    }

    pub fn local_table(&self) -> &LocalImageTable {
        &self.local
    }

    pub fn current_doc_bindings(&self) -> &ImageBindingTable {
        &self.doc_current
    }

    pub fn resolve_image_for_local_command(
        &self,
        read: SessionReadImage,
    ) -> Result<GlaImageKey, SessionError> {
        match read {
            SessionReadImage::Current(id) => self
                .local
                .key(id)
                .or_else(|| self.doc_current.get(id))
                .ok_or(SessionError::CurrentReadNotDeclared { id }),
            SessionReadImage::Backup(id) => self
                .doc_start
                .get(id)
                .ok_or(SessionError::BackupReadNotDeclared { id }),
        }
    }

    pub fn resolve_image_for_registry_command(
        &self,
        id: ImageId,
    ) -> Result<GlaImageKey, SessionError> {
        self.doc_current
            .get(id)
            .ok_or(SessionError::MissingImage { id })
    }

    pub fn lower_draw_on_command(&self, draw_on_index: usize) -> Result<DrawCommand, SessionError> {
        let command = self
            .draw_on
            .get(draw_on_index)
            .ok_or(SessionError::InvalidDrawOnIndex {
                index: draw_on_index,
            })?;
        let target = resolve_destination_target(&self.local, &self.doc_access, command.dst)?;
        Ok(DrawCommand {
            dst: self.resolve_target_key(target)?,
            input_mapping: command.input_mapping,
            op: command.op,
            params: command.params.clone(),
        })
    }

    pub fn lower_derive_command(&self, derive_index: usize) -> Result<ImageCommand, SessionError> {
        let command = self
            .derive_commands
            .get(derive_index)
            .ok_or(SessionError::MissingImage {
                id: ImageId::new(derive_index as u64),
            })?;
        self.lower_session_command(&command.command, command.dst)
    }

    pub fn lower_registry_command(
        &self,
        command_index: CommandIndex,
    ) -> Result<ImageCommand, SessionError> {
        let command = self
            .registry_commands
            .get(&command_index)
            .ok_or(SessionError::MissingImage { id: self.root })?;
        let dst = self
            .registry_command_dst
            .get(&command_index)
            .copied()
            .ok_or(SessionError::MissingImage { id: self.root })?;
        let reads = command
            .reads
            .iter()
            .map(|read| {
                self.doc_current
                    .get(read.image)
                    .ok_or(SessionError::MissingImage { id: read.image })
                    .map(|image| ImageCommandRead {
                        image,
                        mapping: read.mapping,
                        modifier: read.modifier,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let dst = self
            .doc_current
            .get(dst)
            .ok_or(SessionError::MissingImage { id: dst })?;
        Ok(ImageCommand {
            reads,
            dst,
            op: command.op,
            params: command.params.clone(),
        })
    }

    pub fn mark_draw_on_dirty(
        &mut self,
        draw_on_index: usize,
        tiles: TileSet,
    ) -> Result<TileSet, SessionError> {
        let _command = self.lower_draw_on_command(draw_on_index)?;
        let command = self
            .draw_on
            .get(draw_on_index)
            .ok_or(SessionError::InvalidDrawOnIndex {
                index: draw_on_index,
            })?
            .clone();
        let target = resolve_destination_target(&self.local, &self.doc_access, command.dst)?;
        self.mark_dirty(target, tiles);
        self.drain_pending()?;
        Ok(self.root_repaint.clone())
    }

    pub fn mark_image_dirty(
        &mut self,
        id: ImageId,
        tiles: TileSet,
    ) -> Result<TileSet, SessionError> {
        let target = if self.local.contains(id) {
            ResolvedTarget::Local(id)
        } else if self.doc_start.contains(id) {
            ResolvedTarget::Document(id)
        } else {
            return Err(SessionError::MissingImage { id });
        };
        self.mark_dirty(target, tiles);
        self.drain_pending()?;
        Ok(self.root_repaint.clone())
    }

    pub fn root_repaint(&self) -> &TileSet {
        &self.root_repaint
    }

    pub fn commit(
        self,
        document: &mut SessionDocument,
        images: &mut ImagesSession<'_>,
        tiles: &mut TilesSession<'_>,
    ) -> Result<DrawCommit, SessionError> {
        let active = document.active();
        if active.graph != self.graph || active.bindings != self.bindings_before {
            return Err(SessionError::ActiveStateChanged);
        }

        let mut discarded_images = Vec::new();
        let mut discarded_tiles = Vec::new();
        let mut bindings_after = self.doc_current.clone();
        for id in &self.doc_write_closure {
            if self
                .document_dirty
                .get(id)
                .is_none_or(|tiles| tiles.is_empty())
            {
                let old_key = self
                    .doc_start
                    .get(*id)
                    .ok_or(SessionError::BindingMissing { id: *id })?;
                let new_key = self
                    .cow_images
                    .get(id)
                    .copied()
                    .ok_or(SessionError::BindingMissing { id: *id })?;
                bindings_after.insert(*id, old_key);
                images.discard(new_key);
                discarded_images.push(new_key);
            }
        }

        for local in self.local.values() {
            discarded_tiles.extend(images.discard_all_tiles(tiles, local.key)?);
            images.discard(local.key);
            discarded_images.push(local.key);
        }

        let doc_dirty = dirty_map_to_vec(&self.record_dirty);
        if doc_dirty.is_empty() {
            for (id, key) in self.cow_images {
                if !discarded_images.contains(&key) {
                    bindings_after.insert(
                        id,
                        self.doc_start
                            .get(id)
                            .ok_or(SessionError::BindingMissing { id })?,
                    );
                    images.discard(key);
                    discarded_images.push(key);
                }
            }
            return Ok(DrawCommit {
                record: None,
                root_repaint: self.root_repaint,
                discarded_images,
                discarded_tiles,
            });
        }

        {
            let graph = document.graph(self.graph)?;
            for id in &self.doc_write_closure {
                if self
                    .document_dirty
                    .get(id)
                    .is_none_or(|tiles| tiles.is_empty())
                {
                    continue;
                }
                if *id == self.root {
                    continue;
                }
                if !graph
                    .declaration(*id)
                    .is_some_and(ImageDeclaration::is_derived)
                {
                    continue;
                }
                let old_key = self
                    .doc_start
                    .get(*id)
                    .ok_or(SessionError::BindingMissing { id: *id })?;
                let new_key = bindings_after
                    .get(*id)
                    .ok_or(SessionError::BindingMissing { id: *id })?;
                discarded_tiles.extend(images.discard_replaced_tiles(tiles, old_key, new_key)?);
            }
            bindings_after.validate_against_graph(graph)?;
        }

        let record = document.commit_draw(
            self.graph,
            self.bindings_before,
            bindings_after,
            doc_dirty,
            self.root_cache_before,
        )?;

        Ok(DrawCommit {
            record: Some(record),
            root_repaint: self.root_repaint,
            discarded_images,
            discarded_tiles,
        })
    }

    fn resolve_target_key(&self, target: ResolvedTarget) -> Result<GlaImageKey, SessionError> {
        match target {
            ResolvedTarget::Local(id) => {
                self.local.key(id).ok_or(SessionError::MissingImage { id })
            }
            ResolvedTarget::Document(id) => self
                .doc_current
                .get(id)
                .ok_or(SessionError::MissingImage { id }),
        }
    }

    fn lower_session_command(
        &self,
        command: &SessionCommand,
        dst: ImageId,
    ) -> Result<ImageCommand, SessionError> {
        let target = resolve_destination_target(&self.local, &self.doc_access, dst)?;
        let reads = command
            .reads
            .iter()
            .map(|read| {
                self.resolve_image_for_local_command(read.image)
                    .map(|image| ImageCommandRead {
                        image,
                        mapping: read.mapping,
                        modifier: read.modifier,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ImageCommand {
            reads,
            dst: self.resolve_target_key(target)?,
            op: command.op,
            params: command.params.clone(),
        })
    }

    fn mark_dirty(&mut self, target: ResolvedTarget, tiles: TileSet) {
        if tiles.is_empty() {
            return;
        }

        match target {
            ResolvedTarget::Local(id) => union_dirty(&mut self.local_dirty, id, &tiles),
            ResolvedTarget::Document(id) => {
                union_dirty(&mut self.document_dirty, id, &tiles);
                if self.doc_access.get(&id) == Some(&DocumentImageAccess::ReadWrite) {
                    union_dirty(&mut self.record_dirty, id, &tiles);
                }
                if id == self.root {
                    self.root_repaint.union_assign(&tiles);
                }
            }
        }

        if let Some(readers) = self.reader_edges.get(&target) {
            for edge in readers {
                let dst_tiles =
                    upload_dirty_through_read(edge.mapping, edge.modifier, &tiles, edge.dst_layout);
                union_pending(&mut self.pending_by_command, edge.command, &dst_tiles);
            }
        }
    }

    fn drain_pending(&mut self) -> Result<(), SessionError> {
        while !self.pending_by_command.is_empty() {
            let mut made_progress = false;
            for command in self.execution_order.clone() {
                let Some(tiles) = self.pending_by_command.remove(&command) else {
                    continue;
                };
                made_progress = true;
                self.process_pending_command(command, tiles)?;
            }

            if !made_progress {
                let Some((command, tiles)) = pop_first_pending(&mut self.pending_by_command) else {
                    break;
                };
                self.process_pending_command(command, tiles)?;
            }
        }
        Ok(())
    }

    fn process_pending_command(
        &mut self,
        command: SessionCommandRef,
        tiles: TileSet,
    ) -> Result<(), SessionError> {
        if tiles.is_empty() {
            return Ok(());
        }

        let dst = match command {
            SessionCommandRef::LocalDerive(index) => {
                let _command = self.lower_derive_command(index)?;
                let derive = &self.derive_commands[index];
                if self.local.contains(derive.dst) {
                    ResolvedTarget::Local(derive.dst)
                } else {
                    ResolvedTarget::Document(derive.dst)
                }
            }
            SessionCommandRef::Registry(index) => {
                let _command = self.lower_registry_command(index)?;
                let Some(dst) = self.registry_command_dst.get(&index).copied() else {
                    return Ok(());
                };
                ResolvedTarget::Document(dst)
            }
        };
        self.mark_dirty(dst, tiles);
        Ok(())
    }
}

fn validate_doc_image_uses(
    graph: &RegistryGraph,
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
        let declaration = graph
            .declaration(image_use.id)
            .ok_or(SessionError::MissingImage { id: image_use.id })?;
        if image_use.access == DocumentImageAccess::ReadWrite && !declaration.is_primitive() {
            return Err(SessionError::ReadWriteRequiresPrimitive { id: image_use.id });
        }
    }
    Ok(access)
}

fn resolve_session_declarations(
    graph: &RegistryGraph,
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    session_images: &[SessionImageDecl],
) -> Result<HashMap<ImageId, LocalImageDeclaration>, SessionError> {
    let all_session_ids = session_images
        .iter()
        .map(SessionImageDecl::id)
        .collect::<HashSet<_>>();
    let mut resolved = HashMap::new();
    for declaration in session_images {
        let id = declaration.id();
        if resolved.contains_key(&id) {
            return Err(SessionError::DuplicateSessionImage { id });
        }
        if doc_access.get(&id) == Some(&DocumentImageAccess::ReadWrite) {
            return Err(SessionError::SessionImageConflictsWithReadWriteDoc { id });
        }

        let image_declaration = match declaration {
            SessionImageDecl::Primitive { format, layout, .. } => {
                let format = resolve_format_ref(format, graph, &resolved, &all_session_ids)?;
                let layout = resolve_layout_ref(layout, graph, &resolved, &all_session_ids)?;
                LocalImageDeclaration::primitive(format, layout)
            }
            SessionImageDecl::Derived {
                format,
                layout,
                command,
                ..
            } => {
                let format = resolve_format_ref(format, graph, &resolved, &all_session_ids)?;
                let layout = resolve_layout_ref(layout, graph, &resolved, &all_session_ids)?;
                LocalImageDeclaration::derived(format, layout, command.clone())
            }
        };
        resolved.insert(id, image_declaration);
    }
    Ok(resolved)
}

fn resolve_format_ref(
    format: &MetadataRef<gla_color::GlaFormat>,
    graph: &RegistryGraph,
    session_decls: &HashMap<ImageId, LocalImageDeclaration>,
    all_session_ids: &HashSet<ImageId>,
) -> Result<gla_color::GlaFormat, SessionError> {
    match format {
        MetadataRef::Concrete(format) => Ok(*format),
        MetadataRef::Like(id) => resolve_like_format(*id, graph, session_decls, all_session_ids),
    }
}

fn resolve_layout_ref(
    layout: &MetadataRef<GlaImageLayout>,
    graph: &RegistryGraph,
    session_decls: &HashMap<ImageId, LocalImageDeclaration>,
    all_session_ids: &HashSet<ImageId>,
) -> Result<GlaImageLayout, SessionError> {
    match layout {
        MetadataRef::Concrete(layout) => Ok(*layout),
        MetadataRef::Like(id) => resolve_like_layout(*id, graph, session_decls, all_session_ids),
    }
}

fn resolve_like_format(
    id: ImageId,
    graph: &RegistryGraph,
    session_decls: &HashMap<ImageId, LocalImageDeclaration>,
    all_session_ids: &HashSet<ImageId>,
) -> Result<gla_color::GlaFormat, SessionError> {
    if let Some(declaration) = session_decls.get(&id) {
        return Ok(declaration.format());
    }
    if let Some(declaration) = graph.declaration(id) {
        return Ok(declaration.format());
    }
    if all_session_ids.contains(&id) {
        return Err(SessionError::LikeReferenceNotDeclaredYet { id });
    }
    Err(SessionError::LikeReferenceUnknown { id })
}

fn resolve_like_layout(
    id: ImageId,
    graph: &RegistryGraph,
    session_decls: &HashMap<ImageId, LocalImageDeclaration>,
    all_session_ids: &HashSet<ImageId>,
) -> Result<GlaImageLayout, SessionError> {
    if let Some(declaration) = session_decls.get(&id) {
        return Ok(declaration.layout());
    }
    if let Some(declaration) = graph.declaration(id) {
        return Ok(declaration.layout());
    }
    if all_session_ids.contains(&id) {
        return Err(SessionError::LikeReferenceNotDeclaredYet { id });
    }
    Err(SessionError::LikeReferenceUnknown { id })
}

fn derived_session_image_commands(
    session_decls: &HashMap<ImageId, LocalImageDeclaration>,
) -> Vec<DeriveCommand> {
    let mut ids = session_decls.keys().copied().collect::<Vec<_>>();
    sort_image_ids(&mut ids);
    ids.into_iter()
        .filter_map(|id| match session_decls.get(&id)? {
            LocalImageDeclaration::Derived { command, .. } => Some(DeriveCommand {
                dst: id,
                command: command.clone(),
            }),
            LocalImageDeclaration::Primitive { .. } => None,
        })
        .collect()
}

#[derive(Clone, Debug)]
struct DrawCommandPlan {
    doc_write_starts: HashSet<ImageId>,
}

fn validate_draw_commands(
    graph: &RegistryGraph,
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    local_decls: &HashMap<ImageId, LocalImageDeclaration>,
    draw_on: &[DrawOnCommand],
    derive: &[DeriveCommand],
) -> Result<DrawCommandPlan, SessionError> {
    let mut writers = HashSet::new();
    let mut doc_write_starts = HashSet::new();

    for command in draw_on {
        let target = resolve_destination_declaration(graph, doc_access, local_decls, command.dst)?;
        if !writers.insert(target) {
            return Err(SessionError::DuplicateWriter { id: command.dst });
        }
        if let ResolvedTarget::Document(id) = target {
            doc_write_starts.insert(id);
        }
    }

    for command in derive {
        for read in &command.command.reads {
            validate_tool_read(graph, doc_access, local_decls, read)?;
        }
        let target = resolve_destination_declaration(graph, doc_access, local_decls, command.dst)?;
        for read in &command.command.reads {
            if let SessionReadImage::Current(_) = read.image {
                let read_target =
                    resolve_current_declaration(graph, doc_access, local_decls, read.image.id())?;
                if read_target == target {
                    return Err(SessionError::DeriveReadsDestinationCurrent { id: command.dst });
                }
            }
        }
        if !writers.insert(target) {
            return Err(SessionError::DuplicateWriter { id: command.dst });
        }
        if let ResolvedTarget::Document(id) = target {
            if graph
                .declaration(id)
                .is_some_and(ImageDeclaration::is_derived)
            {
                return Err(SessionError::CannotShadowDocDerived { id });
            }
            doc_write_starts.insert(id);
        }
    }

    Ok(DrawCommandPlan { doc_write_starts })
}

fn validate_tool_read(
    graph: &RegistryGraph,
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    local_decls: &HashMap<ImageId, LocalImageDeclaration>,
    read: &SessionRead,
) -> Result<(), SessionError> {
    match read.image {
        SessionReadImage::Current(id) => {
            resolve_current_declaration(graph, doc_access, local_decls, id)?;
        }
        SessionReadImage::Backup(id) => {
            if !doc_access.contains_key(&id) {
                return Err(SessionError::BackupReadNotDeclared { id });
            }
        }
    }
    Ok(())
}

fn resolve_current_declaration(
    graph: &RegistryGraph,
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    local_decls: &HashMap<ImageId, LocalImageDeclaration>,
    id: ImageId,
) -> Result<ResolvedTarget, SessionError> {
    if local_decls.contains_key(&id) {
        return Ok(ResolvedTarget::Local(id));
    }
    if doc_access.contains_key(&id) {
        if graph.contains(id) {
            return Ok(ResolvedTarget::Document(id));
        }
        return Err(SessionError::MissingImage { id });
    }
    Err(SessionError::CurrentReadNotDeclared { id })
}

fn resolve_destination_declaration(
    graph: &RegistryGraph,
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    local_decls: &HashMap<ImageId, LocalImageDeclaration>,
    id: ImageId,
) -> Result<ResolvedTarget, SessionError> {
    if local_decls.contains_key(&id) {
        return Ok(ResolvedTarget::Local(id));
    }
    if doc_access.get(&id) == Some(&DocumentImageAccess::ReadWrite) {
        if graph.contains(id) {
            return Ok(ResolvedTarget::Document(id));
        }
        return Err(SessionError::MissingImage { id });
    }
    Err(SessionError::DestinationNotWritable { id })
}

fn resolve_destination_target(
    local: &LocalImageTable,
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    id: ImageId,
) -> Result<ResolvedTarget, SessionError> {
    if local.contains(id) {
        return Ok(ResolvedTarget::Local(id));
    }
    if doc_access.get(&id) == Some(&DocumentImageAccess::ReadWrite) {
        return Ok(ResolvedTarget::Document(id));
    }
    Err(SessionError::DestinationNotWritable { id })
}

fn compute_doc_write_closure(graph: &RegistryGraph, starts: &HashSet<ImageId>) -> HashSet<ImageId> {
    let mut closure = HashSet::new();
    let mut stack: Vec<ImageId> = starts.iter().copied().collect();
    while let Some(id) = stack.pop() {
        if !closure.insert(id) {
            continue;
        }
        if let Some(readers) = graph.analysis().readers_by_image.get(&id) {
            for command in readers {
                if let Some(node) = graph.analysis().command(*command) {
                    stack.push(node.dst);
                }
            }
        }
    }
    closure
}

fn build_session_reader_edges(
    graph: &RegistryGraph,
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    local: &LocalImageTable,
    derive: &[DeriveCommand],
) -> Result<HashMap<ResolvedTarget, Vec<SessionReadEdge>>, SessionError> {
    let local_decls = local.declarations();
    let mut edges = HashMap::<ResolvedTarget, Vec<SessionReadEdge>>::new();

    for (command_index, command) in derive.iter().enumerate() {
        let dst_layout = target_layout(graph, &local_decls, command.dst)?;
        for read in &command.command.reads {
            if let SessionReadImage::Current(id) = read.image {
                let source = resolve_current_declaration(graph, doc_access, &local_decls, id)?;
                edges.entry(source).or_default().push(SessionReadEdge {
                    command: SessionCommandRef::LocalDerive(command_index),
                    mapping: read.mapping,
                    modifier: read.modifier,
                    dst_layout,
                });
            }
        }
    }

    for node_index in &graph.analysis().topo_order {
        let node = graph
            .analysis()
            .command(*node_index)
            .ok_or(SessionError::MissingImage { id: graph.root() })?;
        for read in &node.command.reads {
            let dst_layout = graph
                .declaration(node.dst)
                .ok_or(SessionError::MissingImage { id: node.dst })?
                .layout();
            edges
                .entry(ResolvedTarget::Document(read.image))
                .or_default()
                .push(SessionReadEdge {
                    command: SessionCommandRef::Registry(*node_index),
                    mapping: read.mapping,
                    modifier: read.modifier,
                    dst_layout,
                });
        }
    }

    Ok(edges)
}

fn target_layout(
    graph: &RegistryGraph,
    local_decls: &HashMap<ImageId, LocalImageDeclaration>,
    id: ImageId,
) -> Result<GlaImageLayout, SessionError> {
    if let Some(declaration) = local_decls.get(&id) {
        return Ok(declaration.layout());
    }
    graph
        .declaration(id)
        .map(ImageDeclaration::layout)
        .ok_or(SessionError::MissingImage { id })
}

fn build_session_execution_order(
    graph: &RegistryGraph,
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    local: &LocalImageTable,
    derive: &[DeriveCommand],
) -> Result<Vec<SessionCommandRef>, SessionError> {
    let local_decls = local.declarations();
    let mut local_writer = HashMap::<ResolvedTarget, usize>::new();
    for (index, command) in derive.iter().enumerate() {
        let target = resolve_destination_declaration(graph, doc_access, &local_decls, command.dst)?;
        local_writer.insert(target, index);
    }

    let mut visiting = HashSet::new();
    let mut done = HashSet::new();
    let mut order = Vec::new();
    for target in local_writer.keys().copied() {
        visit_draw_writer_for_order(
            target,
            graph,
            doc_access,
            &local_decls,
            derive,
            &local_writer,
            &mut visiting,
            &mut done,
            &mut order,
        )?;
    }
    for command_index in &graph.analysis().topo_order {
        if let Some(node) = graph.analysis().command(*command_index) {
            visit_draw_writer_for_order(
                ResolvedTarget::Document(node.dst),
                graph,
                doc_access,
                &local_decls,
                derive,
                &local_writer,
                &mut visiting,
                &mut done,
                &mut order,
            )?;
        }
    }
    Ok(order)
}

fn visit_draw_writer_for_order(
    target: ResolvedTarget,
    graph: &RegistryGraph,
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    local_decls: &HashMap<ImageId, LocalImageDeclaration>,
    derive: &[DeriveCommand],
    local_writer: &HashMap<ResolvedTarget, usize>,
    visiting: &mut HashSet<ResolvedTarget>,
    done: &mut HashSet<ResolvedTarget>,
    order: &mut Vec<SessionCommandRef>,
) -> Result<(), SessionError> {
    if done.contains(&target) {
        return Ok(());
    }
    let Some(writer) = derive_writer_for_target(target, graph, local_writer) else {
        return Ok(());
    };
    if !visiting.insert(target) {
        return Err(SessionError::DrawDeriveCycle { id: target.id() });
    }

    let reads = draw_writer_current_reads(target, graph, derive, local_writer);
    for read_id in reads {
        let source = match target {
            ResolvedTarget::Local(_) => {
                resolve_current_declaration(graph, doc_access, local_decls, read_id)?
            }
            ResolvedTarget::Document(_) => ResolvedTarget::Document(read_id),
        };
        if derive_writer_for_target(source, graph, local_writer).is_some() {
            visit_draw_writer_for_order(
                source,
                graph,
                doc_access,
                local_decls,
                derive,
                local_writer,
                visiting,
                done,
                order,
            )?;
        }
    }

    visiting.remove(&target);
    done.insert(target);
    order.push(writer);
    Ok(())
}

fn derive_writer_for_target(
    target: ResolvedTarget,
    graph: &RegistryGraph,
    local_writer: &HashMap<ResolvedTarget, usize>,
) -> Option<SessionCommandRef> {
    if let Some(index) = local_writer.get(&target) {
        return Some(SessionCommandRef::LocalDerive(*index));
    }
    match target {
        ResolvedTarget::Local(_) => None,
        ResolvedTarget::Document(id) => graph
            .analysis()
            .writer_of
            .get(&id)
            .copied()
            .filter(|_| {
                graph
                    .declaration(id)
                    .is_some_and(ImageDeclaration::is_derived)
            })
            .map(SessionCommandRef::Registry),
    }
}

fn draw_writer_current_reads(
    target: ResolvedTarget,
    graph: &RegistryGraph,
    derive: &[DeriveCommand],
    local_writer: &HashMap<ResolvedTarget, usize>,
) -> Vec<ImageId> {
    if let Some(index) = local_writer.get(&target) {
        return derive[*index]
            .command
            .reads
            .iter()
            .filter_map(|read| match read.image {
                SessionReadImage::Current(id) => Some(id),
                SessionReadImage::Backup(_) => None,
            })
            .collect();
    }

    match target {
        ResolvedTarget::Local(_) => Vec::new(),
        ResolvedTarget::Document(id) => graph
            .declaration(id)
            .and_then(ImageDeclaration::graph_command)
            .map(|command| command.reads.iter().map(|read| read.image).collect())
            .unwrap_or_default(),
    }
}

fn upload_dirty_through_read(
    mapping: Mapping,
    modifier: FootprintModifier,
    tiles: &TileSet,
    dst_layout: GlaImageLayout,
) -> TileSet {
    if tiles.is_empty() {
        return TileSet::default();
    }
    match (mapping, modifier) {
        (Mapping::Identity, FootprintModifier::None) => tiles.clone(),
        (Mapping::Identity, FootprintModifier::Expand(px)) => {
            expand_identity_dirty(tiles, dst_layout, px)
        }
        _ => TileSet::Full,
    }
}

fn expand_identity_dirty(tiles: &TileSet, layout: GlaImageLayout, px: f32) -> TileSet {
    if tiles.is_empty() || px <= 0.0 {
        return tiles.clone();
    }
    let TileSet::Tiles(source_tiles) = tiles else {
        return TileSet::Full;
    };

    let tile_count_x = layout.tile_count_x();
    let tile_count_y = layout.tile_count_y();
    if tile_count_x == 0 || tile_count_y == 0 {
        return TileSet::default();
    }

    let radius = (px / IMAGE_TILE_SIZE as f32).ceil() as i32;
    let mut expanded = Vec::new();
    for tile in source_tiles {
        if *tile >= layout.tile_count() {
            continue;
        }
        let x = (*tile % tile_count_x) as i32;
        let y = (*tile / tile_count_x) as i32;
        for yy in (y - radius)..=(y + radius) {
            if yy < 0 || yy >= tile_count_y as i32 {
                continue;
            }
            for xx in (x - radius)..=(x + radius) {
                if xx < 0 || xx >= tile_count_x as i32 {
                    continue;
                }
                expanded.push((yy as u32) * tile_count_x + xx as u32);
            }
        }
    }
    TileSet::tiles(expanded)
}

fn union_dirty(map: &mut HashMap<ImageId, TileSet>, id: ImageId, tiles: &TileSet) {
    map.entry(id)
        .and_modify(|existing| existing.union_assign(tiles))
        .or_insert_with(|| tiles.clone());
}

fn union_pending(
    map: &mut HashMap<SessionCommandRef, TileSet>,
    command: SessionCommandRef,
    tiles: &TileSet,
) {
    if tiles.is_empty() {
        return;
    }
    map.entry(command)
        .and_modify(|existing| existing.union_assign(tiles))
        .or_insert_with(|| tiles.clone());
}

fn pop_first_pending(
    map: &mut HashMap<SessionCommandRef, TileSet>,
) -> Option<(SessionCommandRef, TileSet)> {
    let command = map.keys().next().copied()?;
    let tiles = map.remove(&command)?;
    Some((command, tiles))
}

fn dirty_map_to_vec(map: &HashMap<ImageId, TileSet>) -> Vec<(ImageId, TileSet)> {
    let mut dirty = map
        .iter()
        .filter(|(_, tiles)| !tiles.is_empty())
        .map(|(id, tiles)| (*id, tiles.clone()))
        .collect::<Vec<_>>();
    dirty.sort_unstable_by_key(|(id, _)| id.value());
    dirty
}

fn sort_image_ids(ids: &mut [ImageId]) {
    ids.sort_unstable_by_key(|id| id.value());
}

#[cfg(test)]
mod tests {
    use super::*;
    use gla_color::{ChannelCount, ChannelType, GlaFormat};
    use gla_image::GlaImages;
    use tile_key::Tiles;

    fn format() -> GlaFormat {
        GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::U8,
        }
    }

    fn layout() -> GlaImageLayout {
        GlaImageLayout::new(1, 1)
    }

    fn op(id: u32) -> OpId {
        OpId(id)
    }

    fn id(value: u64) -> ImageId {
        ImageId::new(value)
    }

    fn tile(value: u32) -> TileKey {
        TileKey::new(value, 0)
    }

    fn primitive_doc(root: ImageId) -> SessionDocument {
        let mut declarations = HashMap::new();
        declarations.insert(root, ImageDeclaration::primitive(format(), layout()));
        let graph = RegistryGraph::new(root, declarations).unwrap();
        let mut bindings = HashMap::new();
        bindings.insert(root, GlaImageKey::new(1, 0));
        SessionDocument::new(graph, ImageBindingTable::new(bindings)).unwrap()
    }

    fn paint_to_root_doc() -> (
        SessionDocument,
        GlaImages,
        ImageId,
        ImageId,
        GlaImageKey,
        GlaImageKey,
    ) {
        let paint = id(1);
        let root = id(2);
        let mut declarations = HashMap::new();
        declarations.insert(paint, ImageDeclaration::primitive(format(), layout()));
        declarations.insert(
            root,
            ImageDeclaration::derived(
                format(),
                layout(),
                GraphCommand::new(vec![GraphRead::current(paint)], op(10)),
            ),
        );
        let graph = RegistryGraph::new(root, declarations).unwrap();

        let mut images = GlaImages::new();
        let paint_key = images
            .insert(format(), layout(), vec![tile(1)].into_boxed_slice())
            .unwrap();
        let root_key = images
            .insert(
                format(),
                layout(),
                vec![TileKey::INVALID].into_boxed_slice(),
            )
            .unwrap();

        let mut bindings = HashMap::new();
        bindings.insert(paint, paint_key);
        bindings.insert(root, root_key);
        let document = SessionDocument::new(graph, ImageBindingTable::new(bindings)).unwrap();
        (document, images, paint, root, paint_key, root_key)
    }

    fn two_target_doc() -> (SessionDocument, GlaImages, ImageId, ImageId, ImageId) {
        let color = id(1);
        let wetness = id(2);
        let root = id(3);
        let mut declarations = HashMap::new();
        declarations.insert(color, ImageDeclaration::primitive(format(), layout()));
        declarations.insert(wetness, ImageDeclaration::primitive(format(), layout()));
        declarations.insert(
            root,
            ImageDeclaration::derived(
                format(),
                layout(),
                GraphCommand::new(
                    vec![GraphRead::current(color), GraphRead::current(wetness)],
                    op(30),
                ),
            ),
        );
        let graph = RegistryGraph::new(root, declarations).unwrap();

        let mut images = GlaImages::new();
        let color_key = images
            .insert(format(), layout(), vec![tile(1)].into_boxed_slice())
            .unwrap();
        let wetness_key = images
            .insert(format(), layout(), vec![tile(2)].into_boxed_slice())
            .unwrap();
        let root_key = images
            .insert(
                format(),
                layout(),
                vec![TileKey::INVALID].into_boxed_slice(),
            )
            .unwrap();

        let mut bindings = HashMap::new();
        bindings.insert(color, color_key);
        bindings.insert(wetness, wetness_key);
        bindings.insert(root, root_key);
        let document = SessionDocument::new(graph, ImageBindingTable::new(bindings)).unwrap();
        (document, images, color, wetness, root)
    }

    #[test]
    fn registry_validation_rejects_unreachable_images() {
        let root = id(1);
        let extra = id(2);
        let mut declarations = HashMap::new();
        declarations.insert(root, ImageDeclaration::primitive(format(), layout()));
        declarations.insert(extra, ImageDeclaration::primitive(format(), layout()));

        let err = RegistryGraph::new(root, declarations).unwrap_err();
        assert!(matches!(err, SessionError::UnreachableImage { id } if id == extra));
    }

    #[test]
    fn registry_validation_rejects_cycles() {
        let a = id(1);
        let b = id(2);
        let mut declarations = HashMap::new();
        declarations.insert(
            a,
            ImageDeclaration::derived(
                format(),
                layout(),
                GraphCommand::new(vec![GraphRead::current(b)], op(1)),
            ),
        );
        declarations.insert(
            b,
            ImageDeclaration::derived(
                format(),
                layout(),
                GraphCommand::new(vec![GraphRead::current(a)], op(2)),
            ),
        );

        let err = RegistryGraph::new(a, declarations).unwrap_err();
        assert!(matches!(err, SessionError::RegistryCycle { .. }));
    }

    #[test]
    fn registry_patch_adds_derived_root_and_records_full_dirty_sources() {
        let root = id(1);
        let new_root = id(2);
        let mut document = primitive_doc(root);
        let mut images = GlaImages::new();
        let mut image_session = ImagesSession::new(&mut images);
        let mut tiles = Tiles::new();
        let mut tile_session = TilesSession::new(&mut tiles);

        let patch = RegistryPatch::new(vec![
            RegistryPatchOp::NewImage {
                id: new_root,
                format: format(),
                layout: layout(),
                role: NewImageRole::Derived(GraphCommand::new(
                    vec![GraphRead::current(root)],
                    op(3),
                )),
            },
            RegistryPatchOp::SetRoot(new_root),
        ]);

        let record = document
            .apply_registry_patch(
                &patch,
                &mut image_session,
                &mut tile_session,
                RegistryPatchOptions { atlas_id: 0 },
            )
            .unwrap()
            .unwrap();

        let SessionRecord::Registry(record) = record else {
            panic!("expected registry record");
        };
        assert_eq!(record.changed_before, vec![root]);
        assert_eq!(record.changed_after, vec![new_root]);
        assert_eq!(document.active_graph().unwrap().root(), new_root);
        assert!(document.active_bindings().unwrap().contains(new_root));
    }

    #[test]
    fn registry_patch_forbids_set_derived_on_primitive() {
        let root = id(1);
        let mut document = primitive_doc(root);
        let mut images = GlaImages::new();
        let mut image_session = ImagesSession::new(&mut images);
        let mut tiles = Tiles::new();
        let mut tile_session = TilesSession::new(&mut tiles);
        let patch = RegistryPatch::new(vec![RegistryPatchOp::SetDerived {
            id: root,
            command: GraphCommand::new(Vec::new(), op(4)),
        }]);

        let err = document
            .apply_registry_patch(
                &patch,
                &mut image_session,
                &mut tile_session,
                RegistryPatchOptions { atlas_id: 0 },
            )
            .unwrap_err();
        assert!(matches!(err, SessionError::SetDerivedOnPrimitive { id } if id == root));
    }

    #[test]
    fn draw_session_rejects_read_write_derived_doc_image() {
        let (document, mut images, _paint, root, _, _) = paint_to_root_doc();
        let mut image_session = ImagesSession::new(&mut images);
        let mut tiles = Tiles::new();
        let mut tile_session = TilesSession::new(&mut tiles);
        let ir = DrawSessionIR {
            expected_document_version: document.active().version,
            doc_images: vec![DocImageUse::read_write(root)],
            session_images: Vec::new(),
            draw_on: Vec::new(),
            derive: Vec::new(),
        };

        let err = document
            .begin_draw_session(ir, &mut image_session, &mut tile_session, 0)
            .unwrap_err();
        assert!(matches!(err, SessionError::ReadWriteRequiresPrimitive { id } if id == root));
    }

    #[test]
    fn draw_session_rejects_unknown_like_metadata() {
        let (document, mut images, paint, _, _, _) = paint_to_root_doc();
        let mut image_session = ImagesSession::new(&mut images);
        let mut tiles = Tiles::new();
        let mut tile_session = TilesSession::new(&mut tiles);
        let unknown = id(99);
        let ir = DrawSessionIR {
            expected_document_version: document.active().version,
            doc_images: vec![DocImageUse::read_write(paint)],
            session_images: vec![SessionImageDecl::Primitive {
                id: id(3),
                format: MetadataRef::Like(unknown),
                layout: MetadataRef::Like(paint),
            }],
            draw_on: Vec::new(),
            derive: Vec::new(),
        };

        let err = document
            .begin_draw_session(ir, &mut image_session, &mut tile_session, 0)
            .unwrap_err();
        assert!(matches!(err, SessionError::LikeReferenceUnknown { id } if id == unknown));
    }

    #[test]
    fn draw_session_reports_forward_like_reference() {
        let (document, mut images, paint, _, _, _) = paint_to_root_doc();
        let mut image_session = ImagesSession::new(&mut images);
        let mut tiles = Tiles::new();
        let mut tile_session = TilesSession::new(&mut tiles);
        let first = id(3);
        let later = id(4);
        let ir = DrawSessionIR {
            expected_document_version: document.active().version,
            doc_images: vec![DocImageUse::read_write(paint)],
            session_images: vec![
                SessionImageDecl::Primitive {
                    id: first,
                    format: MetadataRef::Like(later),
                    layout: MetadataRef::Like(paint),
                },
                SessionImageDecl::Primitive {
                    id: later,
                    format: MetadataRef::Concrete(format()),
                    layout: MetadataRef::Concrete(layout()),
                },
            ],
            draw_on: Vec::new(),
            derive: Vec::new(),
        };

        let err = document
            .begin_draw_session(ir, &mut image_session, &mut tile_session, 0)
            .unwrap_err();
        assert!(matches!(err, SessionError::LikeReferenceNotDeclaredYet { id } if id == later));
    }

    #[test]
    fn local_and_registry_resolution_do_not_confuse_shadowed_ids() {
        let root = id(1);
        let document = primitive_doc(root);
        let mut images = GlaImages::new();
        let doc_key = images
            .insert(format(), layout(), vec![tile(11)].into_boxed_slice())
            .unwrap();
        let mut bindings = HashMap::new();
        bindings.insert(root, doc_key);
        let graph = document.active_graph().unwrap().clone();
        let document = SessionDocument::new(graph, ImageBindingTable::new(bindings)).unwrap();
        let mut image_session = ImagesSession::new(&mut images);
        let mut tiles = Tiles::new();
        let mut tile_session = TilesSession::new(&mut tiles);

        let ir = DrawSessionIR {
            expected_document_version: document.active().version,
            doc_images: vec![DocImageUse::read(root)],
            session_images: vec![SessionImageDecl::Derived {
                id: root,
                format: MetadataRef::Concrete(format()),
                layout: MetadataRef::Concrete(layout()),
                command: SessionCommand::new(Vec::new(), op(40)),
            }],
            draw_on: Vec::new(),
            derive: Vec::new(),
        };
        let session = document
            .begin_draw_session(ir, &mut image_session, &mut tile_session, 0)
            .unwrap();

        let local_key = session.local_images().get(&root).unwrap().key;
        assert_eq!(
            session
                .resolve_image_for_local_command(SessionReadImage::Current(root))
                .unwrap(),
            local_key
        );
        assert_eq!(
            session.resolve_image_for_registry_command(root).unwrap(),
            doc_key
        );
    }

    #[test]
    fn invalid_draw_on_index_has_specific_error() {
        let (document, mut images, paint, _, _, _) = paint_to_root_doc();
        let mut image_session = ImagesSession::new(&mut images);
        let mut tiles = Tiles::new();
        let mut tile_session = TilesSession::new(&mut tiles);
        let ir = DrawSessionIR {
            expected_document_version: document.active().version,
            doc_images: vec![DocImageUse::read_write(paint)],
            session_images: Vec::new(),
            draw_on: Vec::new(),
            derive: Vec::new(),
        };
        let mut session = document
            .begin_draw_session(ir, &mut image_session, &mut tile_session, 0)
            .unwrap();

        let err = session
            .mark_draw_on_dirty(0, TileSet::single(0))
            .unwrap_err();
        assert!(matches!(err, SessionError::InvalidDrawOnIndex { index: 0 }));
    }

    #[test]
    fn explicit_execution_order_handles_local_derive_chain_out_of_ir_order() {
        let (document, mut images, paint, _, _, _) = paint_to_root_doc();
        let mut image_session = ImagesSession::new(&mut images);
        let mut tiles = Tiles::new();
        let mut tile_session = TilesSession::new(&mut tiles);
        let blurred = id(3);
        let coverage = id(4);
        let ir = DrawSessionIR {
            expected_document_version: document.active().version,
            doc_images: vec![DocImageUse::read_write(paint)],
            session_images: vec![
                SessionImageDecl::Derived {
                    id: blurred,
                    format: MetadataRef::Like(paint),
                    layout: MetadataRef::Like(paint),
                    command: SessionCommand::new(vec![SessionRead::current(coverage)], op(52)),
                },
                SessionImageDecl::Derived {
                    id: coverage,
                    format: MetadataRef::Like(paint),
                    layout: MetadataRef::Like(paint),
                    command: SessionCommand::new(vec![SessionRead::current(paint)], op(51)),
                },
            ],
            draw_on: vec![DrawOnCommand::new(paint, op(50))],
            derive: Vec::new(),
        };
        let mut session = document
            .begin_draw_session(ir, &mut image_session, &mut tile_session, 0)
            .unwrap();

        assert_eq!(
            session.execution_order,
            vec![
                SessionCommandRef::LocalDerive(1),
                SessionCommandRef::LocalDerive(0),
                SessionCommandRef::Registry(CommandIndex(0)),
            ]
        );
        session.mark_draw_on_dirty(0, TileSet::single(0)).unwrap();
        assert_eq!(session.local_dirty.get(&blurred), Some(&TileSet::single(0)));
    }

    #[test]
    fn identity_expand_dirty_uses_tile_neighborhood_instead_of_full() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE * 3, IMAGE_TILE_SIZE * 3);

        let expanded = upload_dirty_through_read(
            Mapping::Identity,
            FootprintModifier::Expand(1.0),
            &TileSet::single(4),
            layout,
        );
        assert_eq!(expanded, TileSet::tiles(0..9));
    }

    #[test]
    fn empty_draw_session_commit_produces_no_record() {
        let (mut document, mut images, paint, _, _, _) = paint_to_root_doc();
        let mut image_session = ImagesSession::new(&mut images);
        let mut tiles = Tiles::new();
        let mut tile_session = TilesSession::new(&mut tiles);
        let ir = DrawSessionIR {
            expected_document_version: document.active().version,
            doc_images: vec![DocImageUse::read_write(paint)],
            session_images: Vec::new(),
            draw_on: Vec::new(),
            derive: Vec::new(),
        };
        let session = document
            .begin_draw_session(ir, &mut image_session, &mut tile_session, 0)
            .unwrap();
        let commit = session
            .commit(&mut document, &mut image_session, &mut tile_session)
            .unwrap();
        assert!(commit.record.is_none());
    }

    #[test]
    fn multi_target_draw_records_dirty_per_document_image() {
        let (mut document, mut images, color, wetness, _) = two_target_doc();
        let mut image_session = ImagesSession::new(&mut images);
        let mut tiles = Tiles::new();
        let mut tile_session = TilesSession::new(&mut tiles);
        let ir = DrawSessionIR {
            expected_document_version: document.active().version,
            doc_images: vec![
                DocImageUse::read_write(color),
                DocImageUse::read_write(wetness),
            ],
            session_images: Vec::new(),
            draw_on: vec![
                DrawOnCommand::new(color, op(60)),
                DrawOnCommand::new(wetness, op(61)),
            ],
            derive: Vec::new(),
        };
        let mut session = document
            .begin_draw_session(ir, &mut image_session, &mut tile_session, 0)
            .unwrap();
        session.mark_draw_on_dirty(0, TileSet::single(0)).unwrap();
        session.mark_draw_on_dirty(1, TileSet::single(0)).unwrap();
        let commit = session
            .commit(&mut document, &mut image_session, &mut tile_session)
            .unwrap();
        let Some(SessionRecord::Draw(record)) = commit.record else {
            panic!("expected draw record");
        };
        assert_eq!(
            record.doc_dirty,
            vec![(color, TileSet::single(0)), (wetness, TileSet::single(0))]
        );
    }

    #[test]
    fn set_primitive_uses_materializer_for_invalid_derived_image() {
        struct FillMaterializer;

        impl DerivedMaterializer for FillMaterializer {
            fn materialize_derived_image(
                &mut self,
                _id: ImageId,
                key: GlaImageKey,
                images: &mut ImagesSession<'_>,
                _tiles: &mut TilesSession<'_>,
            ) -> Result<(), SessionError> {
                images.set_tile(key, 0, tile(99))?;
                Ok(())
            }
        }

        let (mut document, mut images, _paint, root, _, _) = paint_to_root_doc();
        let mut image_session = ImagesSession::new(&mut images);
        let mut tiles = Tiles::new();
        let mut tile_session = TilesSession::new(&mut tiles);
        let mut materializer = FillMaterializer;
        let patch = RegistryPatch::new(vec![RegistryPatchOp::SetPrimitive(root)]);

        let record = document
            .apply_registry_patch_with(
                &patch,
                &mut image_session,
                &mut tile_session,
                RegistryPatchOptions { atlas_id: 0 },
                &mut materializer,
            )
            .unwrap();
        assert!(record.is_some());
        assert!(
            document
                .active_graph()
                .unwrap()
                .declaration(root)
                .unwrap()
                .is_primitive()
        );
    }

    #[test]
    fn registry_patch_sweeps_unreachable_images_after_command_change() {
        let keep = id(1);
        let drop = id(2);
        let root = id(3);
        let mut declarations = HashMap::new();
        declarations.insert(keep, ImageDeclaration::primitive(format(), layout()));
        declarations.insert(drop, ImageDeclaration::primitive(format(), layout()));
        declarations.insert(
            root,
            ImageDeclaration::derived(
                format(),
                layout(),
                GraphCommand::new(
                    vec![GraphRead::current(keep), GraphRead::current(drop)],
                    op(70),
                ),
            ),
        );
        let graph = RegistryGraph::new(root, declarations).unwrap();
        let mut bindings = HashMap::new();
        bindings.insert(keep, GlaImageKey::new(10, 0));
        bindings.insert(drop, GlaImageKey::new(11, 0));
        bindings.insert(root, GlaImageKey::new(12, 0));
        let mut document = SessionDocument::new(graph, ImageBindingTable::new(bindings)).unwrap();
        let mut images = GlaImages::new();
        let mut image_session = ImagesSession::new(&mut images);
        let mut tiles = Tiles::new();
        let mut tile_session = TilesSession::new(&mut tiles);

        let patch = RegistryPatch::new(vec![RegistryPatchOp::SetDerived {
            id: root,
            command: GraphCommand::new(vec![GraphRead::current(keep)], op(71)),
        }]);
        document
            .apply_registry_patch(
                &patch,
                &mut image_session,
                &mut tile_session,
                RegistryPatchOptions { atlas_id: 0 },
            )
            .unwrap();

        assert!(!document.active_graph().unwrap().contains(drop));
        assert!(!document.active_bindings().unwrap().contains(drop));
    }

    #[test]
    fn draw_session_commits_doc_dirty_and_propagates_to_registry_root() {
        let (mut document, mut images, paint, root, paint_key, root_key) = paint_to_root_doc();
        let mut image_session = ImagesSession::new(&mut images);
        let mut tiles = Tiles::new();
        let mut tile_session = TilesSession::new(&mut tiles);
        let ir = DrawSessionIR {
            expected_document_version: document.active().version,
            doc_images: vec![DocImageUse::read_write(paint)],
            session_images: Vec::new(),
            draw_on: vec![DrawOnCommand::new(paint, op(5))],
            derive: Vec::new(),
        };

        let mut session = document
            .begin_draw_session(ir, &mut image_session, &mut tile_session, 0)
            .unwrap();
        let repaint = session.mark_draw_on_dirty(0, TileSet::single(0)).unwrap();
        assert_eq!(repaint, TileSet::single(0));

        let commit = session
            .commit(&mut document, &mut image_session, &mut tile_session)
            .unwrap();
        let Some(SessionRecord::Draw(record)) = commit.record else {
            panic!("expected draw record");
        };
        assert_eq!(record.doc_dirty, vec![(paint, TileSet::single(0))]);
        assert_ne!(
            document.active_bindings().unwrap().get(paint),
            Some(paint_key)
        );
        assert_ne!(
            document.active_bindings().unwrap().get(root),
            Some(root_key)
        );
        assert_eq!(record.root_cache_before, root_key);
        assert_eq!(record.root_cache_after, document.root_cache().unwrap());
    }

    #[test]
    fn undo_and_redo_restore_draw_binding_snapshots() {
        let (mut document, mut images, paint, _root, paint_key, _root_key) = paint_to_root_doc();
        let mut image_session = ImagesSession::new(&mut images);
        let mut tiles = Tiles::new();
        let mut tile_session = TilesSession::new(&mut tiles);
        let ir = DrawSessionIR {
            expected_document_version: document.active().version,
            doc_images: vec![DocImageUse::read_write(paint)],
            session_images: Vec::new(),
            draw_on: vec![DrawOnCommand::new(paint, op(5))],
            derive: Vec::new(),
        };
        let mut session = document
            .begin_draw_session(ir, &mut image_session, &mut tile_session, 0)
            .unwrap();
        session.mark_draw_on_dirty(0, TileSet::single(0)).unwrap();
        let record = session
            .commit(&mut document, &mut image_session, &mut tile_session)
            .unwrap()
            .record
            .unwrap();
        let after_key = document.active_bindings().unwrap().get(paint).unwrap();
        assert_ne!(after_key, paint_key);

        let undo = document.undo(&record).unwrap();
        assert_eq!(
            document.active_bindings().unwrap().get(paint),
            Some(paint_key)
        );
        assert_eq!(undo.sources, vec![(paint, TileSet::single(0))]);

        let redo = document.redo(&record).unwrap();
        assert_eq!(
            document.active_bindings().unwrap().get(paint),
            Some(after_key)
        );
        assert_eq!(redo.sources, vec![(paint, TileSet::single(0))]);
    }
}
