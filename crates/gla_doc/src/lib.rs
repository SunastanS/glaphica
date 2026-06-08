use gla_image::{GlaImageKey, GlaImagesError, ImagesSession};
use gla_image_command::TileSet;
use gla_ir::*;
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::marker::PhantomData;
use tile_key::TilesSession;

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

    pub fn commit_draw(
        &mut self,
        graph_key: RegistryGraphKey,
        bindings_before_key: ImageBindingTableKey,
        bindings_after: ImageBindingTable,
        doc_dirty: Vec<(ImageId, TileSet)>,
        root_cache_before: GlaImageKey,
    ) -> Result<SessionRecord, SessionError> {
        if self.active.graph != graph_key || self.active.bindings != bindings_before_key {
            return Err(SessionError::ActiveStateChanged);
        }
        let graph = self.graph(graph_key)?;
        bindings_after.validate_against_graph(graph)?;
        let root = graph.root();
        let root_cache_after = bindings_after
            .get(root)
            .ok_or(SessionError::BindingMissing { id: root })?;
        let bindings_after_key = self.binding_tables.insert(bindings_after);
        self.active = ActiveDocumentState {
            graph: graph_key,
            bindings: bindings_after_key,
            version: self.active.version.next(),
        };
        Ok(SessionRecord::Draw(DrawRecord {
            graph: graph_key,
            bindings_before: bindings_before_key,
            bindings_after: bindings_after_key,
            doc_dirty,
            root_cache_before,
            root_cache_after,
        }))
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

pub fn validate_bound_images(
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

fn sort_image_ids(ids: &mut [ImageId]) {
    ids.sort_unstable_by_key(|id| id.value());
}
