use crate::{GlobalImage, GlobalStorage};
use gla_color::GlaFormat;
use gla_image::{DenseImage, GlaImageLayout, ImageError};
use gla_image_command::{Copy, Derive, DeriveCommand as ImageDeriveCommand, ImageRef};
use gla_ir::{
    DocumentImageAccess, DocumentVersionId, DrawOnCommand, DrawSessionIR, GraphCommand, ImageId,
    MetadataRef, SessionCommand, SessionImageDecl, SessionReadImage,
};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use tile_key::{Tile, Tiles};

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
    DuplicateDocImage { id: ImageId },
    MissingGlobalImage { id: ImageId },
    ReadWriteRequiresPrimitive { id: ImageId },
    DuplicateSessionImage { id: ImageId },
    SessionImageConflictsWithReadWriteDoc { id: ImageId },
    MissingMetadataRef { id: ImageId },
    DuplicateWriter { id: ImageId },
    MissingWriter { id: ImageId },
    DestinationNotWritable { id: ImageId },
    BackupReadRequiresDocImage { id: ImageId },
    CurrentReadRequiresDeclaredImage { id: ImageId },
    WriterCycle { id: ImageId },
    ImageCreate { id: ImageId, source: ImageError },
}

impl Display for LocalStorageError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
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
    images: HashMap<ImageId, SessionImage>,
}

impl LocalStorage {
    pub fn build(
        ir: &DrawSessionIR,
        global: &mut GlobalStorage,
    ) -> Result<Self, LocalStorageError> {
        let doc_access = collect_doc_access(ir, global)?;
        let session_specs = resolve_session_specs(ir, global, &doc_access)?;
        let writers = collect_writers(ir)?;
        let mut plans = build_plans(&doc_access, &session_specs, writers, global)?;
        activate_global_derived_chain(&mut plans, &session_specs, global)?;
        validate_writer_cycles(&plans)?;
        let images = allocate_plans(plans, global)?;
        Ok(Self {
            expected_document_version: ir.expected_document_version,
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

    pub fn into_images(self) -> HashMap<ImageId, SessionImage> {
        self.images
    }
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
    match op {
        Derive::Copy(op) => Some(op.src.key),
        Derive::RenderTo(op) => Some(op.src.key),
        Derive::Clear(_) => None,
    }
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
    use gla_ir::{DocImageUse, GraphRead, ImageRole, RegistryPatch, RegistryPatchOp, SessionRead};
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
        storage
            .apply_registry_patch(RegistryPatch::new(vec![RegistryPatchOp::NewImage {
                id,
                format,
                layout: layout(),
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
