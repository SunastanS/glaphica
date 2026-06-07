use gla_core::IMAGE_TILE_SIZE;
use gla_image::{GlaImageKey, GlaImageLayout, GlaImagesError, ImagesSession};
use gla_image_command::{DrawCommand, ImageCommand, ImageCommandRead};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::marker::PhantomData;
use tile_key::{TileKey, TilesSession};

pub use gla_image_command::TileSet;
pub use gla_ir::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct RegistryGraphKey(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ImageBindingTableKey(u64);

trait SnapshotKey: Copy {
    fn new(index: u32, generation: u32) -> Self;
    fn index(self) -> u32;
    fn generation(self) -> u32;
}

macro_rules! impl_snapshot_key {
    ($key:ty) => {
        impl SnapshotKey for $key {
            fn new(index: u32, generation: u32) -> Self {
                Self(((generation as u64) << 32) | index as u64)
            }

            fn index(self) -> u32 {
                self.0 as u32
            }

            fn generation(self) -> u32 {
                (self.0 >> 32) as u32
            }
        }

        impl $key {
            pub fn index(self) -> u32 {
                <Self as SnapshotKey>::index(self)
            }

            pub fn generation(self) -> u32 {
                <Self as SnapshotKey>::generation(self)
            }
        }
    };
}

impl_snapshot_key!(RegistryGraphKey);
impl_snapshot_key!(ImageBindingTableKey);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct CommandIndex(pub usize);

#[derive(Clone, Debug, PartialEq)]
pub struct RegistryCommandNode {
    pub dst: ImageId,
    pub command: GraphCommand,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RegistryReadEdge {
    pub command: CommandIndex,
    pub read_index: usize,
    pub read: GraphRead,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RegistryAnalysis {
    pub commands: Vec<RegistryCommandNode>,
    pub writer_of: HashMap<ImageId, CommandIndex>,
    pub readers_by_image: HashMap<ImageId, Vec<CommandIndex>>,
    pub read_edges_by_image: HashMap<ImageId, Vec<RegistryReadEdge>>,
    pub topo_order: Vec<CommandIndex>,
}

impl RegistryAnalysis {
    pub fn command(&self, index: CommandIndex) -> Option<&RegistryCommandNode> {
        self.commands.get(index.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RegistryGraph {
    root: ImageId,
    declarations: HashMap<ImageId, ImageDeclaration>,
    analysis: RegistryAnalysis,
}

impl RegistryGraph {
    pub fn new(
        root: ImageId,
        declarations: HashMap<ImageId, ImageDeclaration>,
    ) -> Result<Self, SessionError> {
        let analysis = validate_registry_graph(root, &declarations)?;
        Ok(Self {
            root,
            declarations,
            analysis,
        })
    }

    pub fn root(&self) -> ImageId {
        self.root
    }

    pub fn declarations(&self) -> &HashMap<ImageId, ImageDeclaration> {
        &self.declarations
    }

    pub fn declaration(&self, id: ImageId) -> Option<&ImageDeclaration> {
        self.declarations.get(&id)
    }

    pub fn analysis(&self) -> &RegistryAnalysis {
        &self.analysis
    }

    pub fn contains(&self, id: ImageId) -> bool {
        self.declarations.contains_key(&id)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageBindingTable {
    bindings: HashMap<ImageId, GlaImageKey>,
}

impl ImageBindingTable {
    pub fn new(bindings: HashMap<ImageId, GlaImageKey>) -> Self {
        Self { bindings }
    }

    pub fn empty() -> Self {
        Self {
            bindings: HashMap::new(),
        }
    }

    pub fn get(&self, id: ImageId) -> Option<GlaImageKey> {
        self.bindings.get(&id).copied()
    }

    pub fn insert(&mut self, id: ImageId, key: GlaImageKey) -> Option<GlaImageKey> {
        self.bindings.insert(id, key)
    }

    pub fn remove(&mut self, id: ImageId) -> Option<GlaImageKey> {
        self.bindings.remove(&id)
    }

    pub fn contains(&self, id: ImageId) -> bool {
        self.bindings.contains_key(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (ImageId, GlaImageKey)> + '_ {
        self.bindings.iter().map(|(id, key)| (*id, *key))
    }

    pub fn validate_against_graph(&self, graph: &RegistryGraph) -> Result<(), SessionError> {
        for id in graph.declarations.keys().copied() {
            if !self.bindings.contains_key(&id) {
                return Err(SessionError::BindingMissing { id });
            }
        }

        for id in self.bindings.keys().copied() {
            if !graph.declarations.contains_key(&id) {
                return Err(SessionError::BindingExtra { id });
            }
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveDocumentState {
    pub graph: RegistryGraphKey,
    pub bindings: ImageBindingTableKey,
    pub version: DocumentVersionId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DrawRecord {
    pub graph: RegistryGraphKey,
    pub bindings_before: ImageBindingTableKey,
    pub bindings_after: ImageBindingTableKey,
    pub doc_dirty: Vec<(ImageId, TileSet)>,
    pub root_cache_before: GlaImageKey,
    pub root_cache_after: GlaImageKey,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RegistryRecord {
    pub graph_before: RegistryGraphKey,
    pub graph_after: RegistryGraphKey,
    pub bindings_before: ImageBindingTableKey,
    pub bindings_after: ImageBindingTableKey,
    pub changed_before: Vec<ImageId>,
    pub changed_after: Vec<ImageId>,
    pub root_cache_before: GlaImageKey,
    pub root_cache_after: GlaImageKey,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SessionRecord {
    Draw(DrawRecord),
    Registry(RegistryRecord),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RepaintDemand {
    pub graph: RegistryGraphKey,
    pub sources: Vec<(ImageId, TileSet)>,
    pub root_cache: GlaImageKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegistryPatchOptions {
    pub atlas_id: u8,
}

pub trait DerivedMaterializer {
    fn materialize_derived_image(
        &mut self,
        id: ImageId,
        key: GlaImageKey,
        images: &mut ImagesSession<'_>,
        tiles: &mut TilesSession<'_>,
    ) -> Result<(), SessionError>;
}

#[derive(Default)]
pub struct AlreadyValidMaterializer;

impl DerivedMaterializer for AlreadyValidMaterializer {
    fn materialize_derived_image(
        &mut self,
        id: ImageId,
        key: GlaImageKey,
        images: &mut ImagesSession<'_>,
        _tiles: &mut TilesSession<'_>,
    ) -> Result<(), SessionError> {
        let image = images.get(key)?;
        if let Some((tile_index, _)) = image
            .tiles
            .iter()
            .copied()
            .enumerate()
            .find(|(_, tile)| tile.is_invalid())
        {
            return Err(SessionError::DerivedImageNotMaterialized { id, tile_index });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DrawCommit {
    pub record: Option<SessionRecord>,
    pub root_repaint: TileSet,
    pub discarded_images: Vec<GlaImageKey>,
    pub discarded_tiles: Vec<TileKey>,
}

#[derive(Debug)]
pub enum SessionError {
    EmptyRegistry,
    MissingRoot {
        root: ImageId,
    },
    MissingImage {
        id: ImageId,
    },
    UnreachableImage {
        id: ImageId,
    },
    RegistryCommandReadsDestination {
        dst: ImageId,
    },
    RegistryCycle {
        id: ImageId,
    },
    BindingMissing {
        id: ImageId,
    },
    BindingExtra {
        id: ImageId,
    },
    InvalidRegistryGraphKey {
        key: RegistryGraphKey,
    },
    InvalidBindingTableKey {
        key: ImageBindingTableKey,
    },
    ExpectedDocumentVersion {
        expected: DocumentVersionId,
        actual: DocumentVersionId,
    },
    ActiveStateChanged,
    InvalidDrawOnIndex {
        index: usize,
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
    ReadWriteRequiresPrimitive {
        id: ImageId,
    },
    BoundImageMetadataMismatch {
        id: ImageId,
    },
    PrimitiveImageHasInvalidTile {
        id: ImageId,
        tile_index: usize,
    },
    BackupReadNotDeclared {
        id: ImageId,
    },
    CurrentReadNotDeclared {
        id: ImageId,
    },
    DestinationNotWritable {
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
    DrawDeriveCycle {
        id: ImageId,
    },
    LikeReferenceUnknown {
        id: ImageId,
    },
    LikeReferenceNotDeclaredYet {
        id: ImageId,
    },
    NewImageAlreadyExists {
        id: ImageId,
    },
    SetDerivedOnPrimitive {
        id: ImageId,
    },
    SetPrimitiveMissing {
        id: ImageId,
    },
    DerivedImageNotMaterialized {
        id: ImageId,
        tile_index: usize,
    },
    Images {
        source: GlaImagesError,
    },
}

impl Display for SessionError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyRegistry => f.write_str("registry graph is empty"),
            Self::MissingRoot { root } => write!(f, "registry root {root:?} is not declared"),
            Self::MissingImage { id } => write!(f, "image {id:?} is not declared"),
            Self::UnreachableImage { id } => {
                write!(f, "image {id:?} is not reachable from the registry root")
            }
            Self::RegistryCommandReadsDestination { dst } => {
                write!(f, "registry command for {dst:?} reads its destination")
            }
            Self::RegistryCycle { id } => {
                write!(f, "registry graph has a dependency cycle at {id:?}")
            }
            Self::BindingMissing { id } => write!(f, "binding table is missing {id:?}"),
            Self::BindingExtra { id } => write!(f, "binding table has extra image {id:?}"),
            Self::InvalidRegistryGraphKey { key } => {
                write!(f, "invalid registry graph key {key:?}")
            }
            Self::InvalidBindingTableKey { key } => {
                write!(f, "invalid image binding table key {key:?}")
            }
            Self::ExpectedDocumentVersion { expected, actual } => write!(
                f,
                "expected document version {expected:?}, active version is {actual:?}"
            ),
            Self::ActiveStateChanged => f.write_str("active document state changed"),
            Self::InvalidDrawOnIndex { index } => {
                write!(f, "draw_on command index {index} is out of bounds")
            }
            Self::DuplicateDocImageUse { id } => {
                write!(f, "document image {id:?} is declared more than once")
            }
            Self::DuplicateSessionImage { id } => {
                write!(f, "session image {id:?} is declared more than once")
            }
            Self::SessionImageConflictsWithReadWriteDoc { id } => write!(
                f,
                "session image {id:?} conflicts with a read-write document image"
            ),
            Self::ReadWriteRequiresPrimitive { id } => {
                write!(f, "read-write document image {id:?} is not primitive")
            }
            Self::BoundImageMetadataMismatch { id } => {
                write!(
                    f,
                    "bound image metadata does not match declaration for {id:?}"
                )
            }
            Self::PrimitiveImageHasInvalidTile { id, tile_index } => write!(
                f,
                "primitive image {id:?} contains invalid tile at index {tile_index}"
            ),
            Self::BackupReadNotDeclared { id } => {
                write!(f, "backup read of {id:?} was not declared in doc_images")
            }
            Self::CurrentReadNotDeclared { id } => {
                write!(f, "current read of {id:?} was not declared in doc_images")
            }
            Self::DestinationNotWritable { id } => {
                write!(f, "destination {id:?} is not a writable session/doc image")
            }
            Self::DuplicateWriter { id } => write!(f, "image {id:?} has more than one writer"),
            Self::DeriveReadsDestinationCurrent { id } => {
                write!(f, "derive command for {id:?} reads destination current")
            }
            Self::CannotShadowDocDerived { id } => {
                write!(f, "local derive command cannot shadow doc-derived {id:?}")
            }
            Self::DrawDeriveCycle { id } => {
                write!(f, "draw-session derive graph has a cycle at {id:?}")
            }
            Self::LikeReferenceUnknown { id } => {
                write!(f, "metadata Like({id:?}) does not reference a known image")
            }
            Self::LikeReferenceNotDeclaredYet { id } => write!(
                f,
                "metadata Like({id:?}) references a session image declared later"
            ),
            Self::NewImageAlreadyExists { id } => {
                write!(f, "patch tried to create existing image {id:?}")
            }
            Self::SetDerivedOnPrimitive { id } => {
                write!(f, "patch tried to set primitive image {id:?} as derived")
            }
            Self::SetPrimitiveMissing { id } => {
                write!(f, "patch tried to materialize missing image {id:?}")
            }
            Self::DerivedImageNotMaterialized { id, tile_index } => write!(
                f,
                "derived image {id:?} still has invalid tile at index {tile_index}"
            ),
            Self::Images { source } => write!(f, "{source}"),
        }
    }
}

impl From<GlaImagesError> for SessionError {
    fn from(source: GlaImagesError) -> Self {
        Self::Images { source }
    }
}

#[derive(Debug)]
pub struct SessionDocument {
    graphs: SnapshotStore<RegistryGraph, RegistryGraphKey>,
    binding_tables: SnapshotStore<ImageBindingTable, ImageBindingTableKey>,
    active: ActiveDocumentState,
}

impl SessionDocument {
    pub fn new(graph: RegistryGraph, bindings: ImageBindingTable) -> Result<Self, SessionError> {
        bindings.validate_against_graph(&graph)?;

        let mut graphs = SnapshotStore::new();
        let graph_key = graphs.insert(graph);
        let mut binding_tables = SnapshotStore::new();
        let binding_key = binding_tables.insert(bindings);

        Ok(Self {
            graphs,
            binding_tables,
            active: ActiveDocumentState {
                graph: graph_key,
                bindings: binding_key,
                version: DocumentVersionId::default(),
            },
        })
    }

    pub fn active(&self) -> ActiveDocumentState {
        self.active
    }

    pub fn active_graph(&self) -> Result<&RegistryGraph, SessionError> {
        self.graph(self.active.graph)
    }

    pub fn active_bindings(&self) -> Result<&ImageBindingTable, SessionError> {
        self.bindings(self.active.bindings)
    }

    pub fn graph(&self, key: RegistryGraphKey) -> Result<&RegistryGraph, SessionError> {
        self.graphs
            .get(key)
            .ok_or(SessionError::InvalidRegistryGraphKey { key })
    }

    pub fn bindings(&self, key: ImageBindingTableKey) -> Result<&ImageBindingTable, SessionError> {
        self.binding_tables
            .get(key)
            .ok_or(SessionError::InvalidBindingTableKey { key })
    }

    pub fn root_cache(&self) -> Result<GlaImageKey, SessionError> {
        let graph = self.active_graph()?;
        self.active_bindings()?
            .get(graph.root())
            .ok_or(SessionError::BindingMissing { id: graph.root() })
    }

    pub fn apply_registry_patch(
        &mut self,
        patch: &RegistryPatch,
        images: &mut ImagesSession<'_>,
        tiles: &mut TilesSession<'_>,
        options: RegistryPatchOptions,
    ) -> Result<Option<SessionRecord>, SessionError> {
        let mut materializer = AlreadyValidMaterializer;
        self.apply_registry_patch_with(patch, images, tiles, options, &mut materializer)
    }

    pub fn apply_registry_patch_with(
        &mut self,
        patch: &RegistryPatch,
        images: &mut ImagesSession<'_>,
        tiles: &mut TilesSession<'_>,
        options: RegistryPatchOptions,
        materializer: &mut impl DerivedMaterializer,
    ) -> Result<Option<SessionRecord>, SessionError> {
        let graph_before_key = self.active.graph;
        let bindings_before_key = self.active.bindings;
        let graph_before = self.active_graph()?.clone();
        let bindings_before = self.active_bindings()?.clone();
        let root_cache_before =
            bindings_before
                .get(graph_before.root())
                .ok_or(SessionError::BindingMissing {
                    id: graph_before.root(),
                })?;

        let mut root = graph_before.root();
        let mut declarations = graph_before.declarations().clone();
        let mut bindings = bindings_before.clone();
        let mut changed_before = HashSet::new();
        let mut changed_after = HashSet::new();
        let mut replaced_derived = Vec::new();
        let mut swept_old_derived = Vec::new();

        for op in &patch.ops {
            match op {
                RegistryPatchOp::NewImage {
                    id,
                    format,
                    layout,
                    role,
                } => {
                    if declarations.contains_key(id) {
                        return Err(SessionError::NewImageAlreadyExists { id: *id });
                    }

                    let (declaration, key) = match role {
                        NewImageRole::Primitive => {
                            let key = images.alloc(*format, *layout, tiles, options.atlas_id)?;
                            (ImageDeclaration::primitive(*format, *layout), key)
                        }
                        NewImageRole::Derived(command) => {
                            let key = images.insert_invalid(*format, *layout)?;
                            changed_after.insert(*id);
                            (
                                ImageDeclaration::derived(*format, *layout, command.clone()),
                                key,
                            )
                        }
                    };
                    declarations.insert(*id, declaration);
                    bindings.insert(*id, key);
                }
                RegistryPatchOp::SetPrimitive(id) => {
                    let old = declarations
                        .get(id)
                        .cloned()
                        .ok_or(SessionError::SetPrimitiveMissing { id: *id })?;
                    match old {
                        ImageDeclaration::Primitive { .. } => {}
                        ImageDeclaration::Derived { format, layout, .. } => {
                            let key = bindings
                                .get(*id)
                                .ok_or(SessionError::BindingMissing { id: *id })?;
                            materializer.materialize_derived_image(*id, key, images, tiles)?;
                            declarations.insert(*id, ImageDeclaration::primitive(format, layout));
                            changed_before.insert(*id);
                            changed_after.insert(*id);
                        }
                    }
                }
                RegistryPatchOp::SetDerived { id, command } => {
                    let old = declarations
                        .get(id)
                        .cloned()
                        .ok_or(SessionError::MissingImage { id: *id })?;
                    match old {
                        ImageDeclaration::Primitive { .. } => {
                            return Err(SessionError::SetDerivedOnPrimitive { id: *id });
                        }
                        ImageDeclaration::Derived { format, layout, .. } => {
                            let new_decl =
                                ImageDeclaration::derived(format, layout, command.clone());
                            if declarations.get(id) == Some(&new_decl) {
                                continue;
                            }
                            let old_key = bindings
                                .get(*id)
                                .ok_or(SessionError::BindingMissing { id: *id })?;
                            let key = images.insert_invalid(format, layout)?;
                            declarations.insert(*id, new_decl);
                            bindings.insert(*id, key);
                            replaced_derived.push((*id, old_key, key));
                            changed_before.insert(*id);
                            changed_after.insert(*id);
                        }
                    }
                }
                RegistryPatchOp::SetRoot(id) => {
                    if root == *id {
                        continue;
                    }
                    if !declarations.contains_key(id) {
                        return Err(SessionError::MissingImage { id: *id });
                    }
                    changed_before.insert(root);
                    changed_after.insert(*id);
                    root = *id;
                }
            }
        }

        let reachable = collect_registry_reachable(root, &declarations)?;
        let removed: Vec<ImageId> = declarations
            .keys()
            .copied()
            .filter(|id| !reachable.contains(id))
            .collect();
        for id in removed {
            declarations.remove(&id);
            if let Some(key) = bindings.remove(id) {
                if !bindings_before.contains(id) {
                    images.discard_all_tiles(tiles, key)?;
                    images.discard(key);
                } else if graph_before
                    .declaration(id)
                    .is_some_and(ImageDeclaration::is_derived)
                    && id != graph_before.root()
                {
                    swept_old_derived.push(key);
                }
            }
        }

        let graph_after = RegistryGraph::new(root, declarations)?;
        bindings.validate_against_graph(&graph_after)?;

        changed_before.retain(|id| graph_before.contains(*id));
        changed_after.retain(|id| graph_after.contains(*id));

        if graph_before == graph_after && bindings_before == bindings {
            return Ok(None);
        }

        for (id, old_key, new_key) in replaced_derived {
            if id != graph_before.root() && graph_after.contains(id) {
                images.discard_replaced_tiles(tiles, old_key, new_key)?;
            }
        }
        for old_key in swept_old_derived {
            images.discard_all_tiles(tiles, old_key)?;
        }

        let root_cache_after =
            bindings
                .get(graph_after.root())
                .ok_or(SessionError::BindingMissing {
                    id: graph_after.root(),
                })?;

        let graph_after_key = self.graphs.insert(graph_after);
        let bindings_after_key = self.binding_tables.insert(bindings);
        self.active = ActiveDocumentState {
            graph: graph_after_key,
            bindings: bindings_after_key,
            version: self.active.version.next(),
        };

        let mut changed_before = changed_before.into_iter().collect::<Vec<_>>();
        let mut changed_after = changed_after.into_iter().collect::<Vec<_>>();
        sort_image_ids(&mut changed_before);
        sort_image_ids(&mut changed_after);

        Ok(Some(SessionRecord::Registry(RegistryRecord {
            graph_before: graph_before_key,
            graph_after: graph_after_key,
            bindings_before: bindings_before_key,
            bindings_after: bindings_after_key,
            changed_before,
            changed_after,
            root_cache_before,
            root_cache_after,
        })))
    }

    pub fn begin_draw_session(
        &self,
        ir: DrawSessionIR,
        images: &mut ImagesSession<'_>,
        tiles: &mut TilesSession<'_>,
        atlas_id: u8,
    ) -> Result<DrawSession, SessionError> {
        if ir.expected_document_version != self.active.version {
            return Err(SessionError::ExpectedDocumentVersion {
                expected: ir.expected_document_version,
                actual: self.active.version,
            });
        }

        let graph = self.active_graph()?.clone();
        let bindings_before_key = self.active.bindings;
        let bindings_before = self.active_bindings()?.clone();
        bindings_before.validate_against_graph(&graph)?;
        validate_bound_images(&graph, &bindings_before, images)?;

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

        let mut local = HashMap::new();
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
            graph: self.active.graph,
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
        match record {
            SessionRecord::Draw(record) => {
                self.graph(record.graph)?;
                self.bindings(record.bindings_before)?;
                self.active = ActiveDocumentState {
                    graph: record.graph,
                    bindings: record.bindings_before,
                    version: self.active.version.next(),
                };
                Ok(RepaintDemand {
                    graph: record.graph,
                    sources: record.doc_dirty.clone(),
                    root_cache: record.root_cache_before,
                })
            }
            SessionRecord::Registry(record) => {
                self.graph(record.graph_before)?;
                self.bindings(record.bindings_before)?;
                self.active = ActiveDocumentState {
                    graph: record.graph_before,
                    bindings: record.bindings_before,
                    version: self.active.version.next(),
                };
                Ok(RepaintDemand {
                    graph: record.graph_before,
                    sources: record
                        .changed_before
                        .iter()
                        .copied()
                        .map(|id| (id, TileSet::Full))
                        .collect(),
                    root_cache: record.root_cache_before,
                })
            }
        }
    }

    pub fn redo(&mut self, record: &SessionRecord) -> Result<RepaintDemand, SessionError> {
        match record {
            SessionRecord::Draw(record) => {
                self.graph(record.graph)?;
                self.bindings(record.bindings_after)?;
                self.active = ActiveDocumentState {
                    graph: record.graph,
                    bindings: record.bindings_after,
                    version: self.active.version.next(),
                };
                Ok(RepaintDemand {
                    graph: record.graph,
                    sources: record.doc_dirty.clone(),
                    root_cache: record.root_cache_after,
                })
            }
            SessionRecord::Registry(record) => {
                self.graph(record.graph_after)?;
                self.bindings(record.bindings_after)?;
                self.active = ActiveDocumentState {
                    graph: record.graph_after,
                    bindings: record.bindings_after,
                    version: self.active.version.next(),
                };
                Ok(RepaintDemand {
                    graph: record.graph_after,
                    sources: record
                        .changed_after
                        .iter()
                        .copied()
                        .map(|id| (id, TileSet::Full))
                        .collect(),
                    root_cache: record.root_cache_after,
                })
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LocalImageDeclaration {
    Primitive {
        format: gla_color::GlaFormat,
        layout: GlaImageLayout,
    },
    Derived {
        format: gla_color::GlaFormat,
        layout: GlaImageLayout,
        command: SessionCommand,
    },
}

impl LocalImageDeclaration {
    fn primitive(format: gla_color::GlaFormat, layout: GlaImageLayout) -> Self {
        Self::Primitive { format, layout }
    }

    fn derived(
        format: gla_color::GlaFormat,
        layout: GlaImageLayout,
        command: SessionCommand,
    ) -> Self {
        Self::Derived {
            format,
            layout,
            command,
        }
    }

    fn format(&self) -> gla_color::GlaFormat {
        match self {
            Self::Primitive { format, .. } | Self::Derived { format, .. } => *format,
        }
    }

    fn layout(&self) -> GlaImageLayout {
        match self {
            Self::Primitive { layout, .. } | Self::Derived { layout, .. } => *layout,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LocalImage {
    pub key: GlaImageKey,
    pub declaration: LocalImageDeclaration,
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
    local: HashMap<ImageId, LocalImage>,
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
                .get(&id)
                .map(|local| local.key)
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
        let target = if self.local.contains_key(&id) {
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
        if document.active.graph != self.graph || document.active.bindings != self.bindings_before {
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

        let root_cache_after = bindings_after
            .get(self.root)
            .ok_or(SessionError::BindingMissing { id: self.root })?;
        let bindings_after_key = document.binding_tables.insert(bindings_after);
        document.active = ActiveDocumentState {
            graph: self.graph,
            bindings: bindings_after_key,
            version: document.active.version.next(),
        };

        let record = SessionRecord::Draw(DrawRecord {
            graph: self.graph,
            bindings_before: self.bindings_before,
            bindings_after: bindings_after_key,
            doc_dirty,
            root_cache_before: self.root_cache_before,
            root_cache_after,
        });

        Ok(DrawCommit {
            record: Some(record),
            root_repaint: self.root_repaint,
            discarded_images,
            discarded_tiles,
        })
    }

    fn resolve_target_key(&self, target: ResolvedTarget) -> Result<GlaImageKey, SessionError> {
        match target {
            ResolvedTarget::Local(id) => self
                .local
                .get(&id)
                .map(|local| local.key)
                .ok_or(SessionError::MissingImage { id }),
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
                if self.local.contains_key(&derive.dst) {
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

#[derive(Debug)]
struct SnapshotStore<T, K> {
    entries: Vec<Option<T>>,
    generations: Vec<u32>,
    free: Vec<u32>,
    _marker: PhantomData<K>,
}

impl<T, K> SnapshotStore<T, K>
where
    K: SnapshotKey,
{
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            generations: Vec::new(),
            free: Vec::new(),
            _marker: PhantomData,
        }
    }

    fn insert(&mut self, value: T) -> K {
        let (index, generation) = if let Some(index) = self.free.pop() {
            (index, self.generations[index as usize])
        } else {
            let index = self.entries.len() as u32;
            self.entries.push(None);
            self.generations.push(0);
            (index, 0)
        };

        self.entries[index as usize] = Some(value);
        K::new(index, generation)
    }

    fn get(&self, key: K) -> Option<&T> {
        let index = key.index() as usize;
        if self.generations.get(index).copied()? != key.generation() {
            return None;
        }
        self.entries.get(index)?.as_ref()
    }
}

fn validate_registry_graph(
    root: ImageId,
    declarations: &HashMap<ImageId, ImageDeclaration>,
) -> Result<RegistryAnalysis, SessionError> {
    if declarations.is_empty() {
        return Err(SessionError::EmptyRegistry);
    }
    if !declarations.contains_key(&root) {
        return Err(SessionError::MissingRoot { root });
    }

    let mut commands = Vec::new();
    let mut writer_of = HashMap::new();
    let mut derived_ids = declarations
        .iter()
        .filter_map(|(id, declaration)| declaration.is_derived().then_some(*id))
        .collect::<Vec<_>>();
    sort_image_ids(&mut derived_ids);
    for id in derived_ids {
        if let Some(ImageDeclaration::Derived { command, .. }) = declarations.get(&id) {
            let index = CommandIndex(commands.len());
            writer_of.insert(id, index);
            commands.push(RegistryCommandNode {
                dst: id,
                command: command.clone(),
            });
        }
    }

    let mut readers_by_image = HashMap::<ImageId, Vec<CommandIndex>>::new();
    let mut read_edges_by_image = HashMap::<ImageId, Vec<RegistryReadEdge>>::new();
    for (command_index, node) in commands.iter().enumerate() {
        let index = CommandIndex(command_index);
        let mut unique_sources = HashSet::new();
        for (read_index, read) in node.command.reads.iter().enumerate() {
            let source = read.image;

            if !declarations.contains_key(&source) {
                return Err(SessionError::MissingImage { id: source });
            }
            if source == node.dst {
                return Err(SessionError::RegistryCommandReadsDestination { dst: node.dst });
            }

            if unique_sources.insert(source) {
                readers_by_image.entry(source).or_default().push(index);
            }
            read_edges_by_image
                .entry(source)
                .or_default()
                .push(RegistryReadEdge {
                    command: index,
                    read_index,
                    read: read.clone(),
                });
        }
    }

    let reachable = collect_registry_reachable(root, declarations)?;
    for id in declarations.keys().copied() {
        if !reachable.contains(&id) {
            return Err(SessionError::UnreachableImage { id });
        }
    }

    let mut topo_order = Vec::new();
    let mut visiting = HashSet::new();
    let mut done = HashSet::new();
    topo_visit_registry_image(
        root,
        declarations,
        &writer_of,
        &commands,
        &mut visiting,
        &mut done,
        &mut topo_order,
    )?;

    Ok(RegistryAnalysis {
        commands,
        writer_of,
        readers_by_image,
        read_edges_by_image,
        topo_order,
    })
}

fn topo_visit_registry_image(
    id: ImageId,
    declarations: &HashMap<ImageId, ImageDeclaration>,
    writer_of: &HashMap<ImageId, CommandIndex>,
    commands: &[RegistryCommandNode],
    visiting: &mut HashSet<ImageId>,
    done: &mut HashSet<ImageId>,
    topo_order: &mut Vec<CommandIndex>,
) -> Result<(), SessionError> {
    if !declarations.contains_key(&id) {
        return Err(SessionError::MissingImage { id });
    }
    let Some(command_index) = writer_of.get(&id).copied() else {
        return Ok(());
    };
    if done.contains(&id) {
        return Ok(());
    }
    if !visiting.insert(id) {
        return Err(SessionError::RegistryCycle { id });
    }

    for read in &commands[command_index.0].command.reads {
        topo_visit_registry_image(
            read.image,
            declarations,
            writer_of,
            commands,
            visiting,
            done,
            topo_order,
        )?;
    }

    visiting.remove(&id);
    done.insert(id);
    topo_order.push(command_index);
    Ok(())
}

fn collect_registry_reachable(
    root: ImageId,
    declarations: &HashMap<ImageId, ImageDeclaration>,
) -> Result<HashSet<ImageId>, SessionError> {
    let mut reachable = HashSet::new();
    collect_registry_reachable_inner(root, declarations, &mut reachable)?;
    Ok(reachable)
}

fn collect_registry_reachable_inner(
    id: ImageId,
    declarations: &HashMap<ImageId, ImageDeclaration>,
    reachable: &mut HashSet<ImageId>,
) -> Result<(), SessionError> {
    let declaration = declarations
        .get(&id)
        .ok_or(SessionError::MissingImage { id })?;
    if !reachable.insert(id) {
        return Ok(());
    }
    if let ImageDeclaration::Derived { command, .. } = declaration {
        for read in &command.reads {
            collect_registry_reachable_inner(read.image, declarations, reachable)?;
        }
    }
    Ok(())
}

fn validate_bound_images(
    graph: &RegistryGraph,
    bindings: &ImageBindingTable,
    images: &mut ImagesSession<'_>,
) -> Result<(), SessionError> {
    for (id, declaration) in graph.declarations() {
        let key = bindings
            .get(*id)
            .ok_or(SessionError::BindingMissing { id: *id })?;
        let image = images.get(key)?;
        if image.format != declaration.format() || image.layout != declaration.layout() {
            return Err(SessionError::BoundImageMetadataMismatch { id: *id });
        }
        if declaration.is_primitive() {
            if let Some((tile_index, _)) = image
                .tiles
                .iter()
                .copied()
                .enumerate()
                .find(|(_, tile)| tile.is_invalid())
            {
                return Err(SessionError::PrimitiveImageHasInvalidTile {
                    id: *id,
                    tile_index,
                });
            }
        }
    }
    Ok(())
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
    local: &HashMap<ImageId, LocalImage>,
    doc_access: &HashMap<ImageId, DocumentImageAccess>,
    id: ImageId,
) -> Result<ResolvedTarget, SessionError> {
    if local.contains_key(&id) {
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
    local: &HashMap<ImageId, LocalImage>,
    derive: &[DeriveCommand],
) -> Result<HashMap<ResolvedTarget, Vec<SessionReadEdge>>, SessionError> {
    let local_decls = local
        .iter()
        .map(|(id, local)| (*id, local.declaration.clone()))
        .collect::<HashMap<_, _>>();
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
    local: &HashMap<ImageId, LocalImage>,
    derive: &[DeriveCommand],
) -> Result<Vec<SessionCommandRef>, SessionError> {
    let local_decls = local
        .iter()
        .map(|(id, local)| (*id, local.declaration.clone()))
        .collect::<HashMap<_, _>>();
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
