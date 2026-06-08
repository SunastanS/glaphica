use gla_image::{GlaImageKey, ImagesSession, TileSet};
use gla_ir::*;
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use tile_key::TilesSession;

/// Re-exported from gla_ir.
pub use gla_ir::ImageRole;

/// Opaque handle to a stored patch. Janet passes this to apply undo/redo.
pub type SessionId = u64;

#[derive(Clone, Debug, PartialEq)]
pub struct DrawPatch {
    pub version: DocumentVersionId,
    pub bindings: HashMap<ImageId, GlaImageKey>,
    pub dirty: TileSet,
}

impl DrawPatch {
    pub fn new(bindings: HashMap<ImageId, GlaImageKey>, dirty: TileSet) -> Self {
        Self {
            version: DocumentVersionId::default(),
            bindings,
            dirty,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PatchKind {
    Draw(DrawPatch),
    Registry(RegistryPatch),
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoredPatch {
    pub version: DocumentVersionId,
    pub kind: PatchKind,
}

#[derive(Debug)]
pub enum DocError {
    EmptyRegistry,
    MissingRoot { root: ImageId },
    MissingImage { id: ImageId },
    UnreachableImage { id: ImageId },
    RegistryCommandReadsDestination { dst: ImageId },
    RegistryCycle { id: ImageId },
    BindingMissing { id: ImageId },
    BindingExtra { id: ImageId },
    InvalidSessionId { id: SessionId },
    VersionMismatch { expected: DocumentVersionId, actual: DocumentVersionId },
}

impl Display for DocError {
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
            Self::InvalidSessionId { id } => write!(f, "invalid session id {id}"),
            Self::VersionMismatch { expected, actual } => {
                write!(f, "version mismatch: expected {expected:?}, actual {actual:?}")
            }
        }
    }
}

#[derive(Debug)]
pub struct Document {
    root: ImageId,
    roles: HashMap<ImageId, ImageRole>,
    bindings: HashMap<ImageId, GlaImageKey>,
    version: DocumentVersionId,
    patches: HashMap<SessionId, StoredPatch>,
    next_id: SessionId,
}

impl Document {
    pub fn new(
        root: ImageId,
        roles: HashMap<ImageId, ImageRole>,
        bindings: HashMap<ImageId, GlaImageKey>,
    ) -> Result<Self, DocError> {
        validate_document(root, &roles)?;
        for id in roles.keys() {
            if !bindings.contains_key(id) {
                return Err(DocError::BindingMissing { id: *id });
            }
        }
        for id in bindings.keys() {
            if !roles.contains_key(id) {
                return Err(DocError::BindingExtra { id: *id });
            }
        }
        Ok(Self {
            root,
            roles,
            bindings,
            version: DocumentVersionId::default(),
            patches: HashMap::new(),
            next_id: 0,
        })
    }

    pub fn root(&self) -> ImageId {
        self.root
    }

    pub fn roles(&self) -> &HashMap<ImageId, ImageRole> {
        &self.roles
    }

    pub fn role(&self, id: ImageId) -> Option<&ImageRole> {
        self.roles.get(&id)
    }

    pub fn bindings(&self) -> &HashMap<ImageId, GlaImageKey> {
        &self.bindings
    }

    pub fn binding(&self, id: ImageId) -> Option<GlaImageKey> {
        self.bindings.get(&id).copied()
    }

    pub fn version(&self) -> DocumentVersionId {
        self.version
    }

    pub fn root_binding(&self) -> Option<GlaImageKey> {
        self.bindings.get(&self.root).copied()
    }

    pub fn stored_patch_version(&self, id: SessionId) -> Option<DocumentVersionId> {
        self.patches.get(&id).map(|p| p.version)
    }

    pub fn commit_draw(&mut self, mut patch: DrawPatch) -> Result<SessionId, DocError> {
        let mut old_bindings = HashMap::new();
        for (id, new_key) in &patch.bindings {
            let old_key = self
                .bindings
                .insert(*id, *new_key)
                .ok_or(DocError::BindingMissing { id: *id })?;
            old_bindings.insert(*id, old_key);
        }
        self.version = self.version.next();
        let inverse = DrawPatch {
            version: self.version,
            bindings: old_bindings,
            dirty: patch.dirty.clone(),
        };
        patch.version = self.version;
        let id = self.store_patch(PatchKind::Draw(inverse));
        Ok(id)
    }

    pub fn apply_registry_patch(
        &mut self,
        patch: &RegistryPatch,
        images: &mut ImagesSession<'_>,
        tiles: &mut TilesSession<'_>,
        atlas_id: u8,
    ) -> Result<SessionId, DocError> {
        let old_root = self.root;
        let mut inverse_ops = Vec::new();
        let mut pending_discard = Vec::new();

        for op in &patch.ops {
            match op {
                RegistryPatchOp::NewImage {
                    id,
                    format,
                    layout,
                    role,
                } => {
                    if self.roles.contains_key(id) {
                        continue;
                    }
                    let image = images.alloc(*format, *layout, tiles, atlas_id)
                        .map_err(|_| DocError::MissingImage { id: *id })?;
                    self.roles.insert(*id, role.clone());
                    self.bindings.insert(*id, image);
                }
                RegistryPatchOp::InsertImage {
                    id,
                    key,
                    role,
                    format: _,
                    layout: _,
                } => {
                    self.roles.insert(*id, role.clone());
                    self.bindings.insert(*id, *key);
                }
                RegistryPatchOp::SetPrimitive(id) => {
                    if let Some(old_role_mut) = self.roles.get_mut(id) {
                        let old_role = old_role_mut.clone();
                        *old_role_mut = gla_ir::ImageRole::Primitive;
                        match old_role {
                            ImageRole::Derived(command) => {
                                inverse_ops.push(RegistryPatchOp::SetDerived {
                                    id: *id,
                                    command,
                                });
                            }
                            ImageRole::Primitive => {}
                        }
                    }
                }
                RegistryPatchOp::SetDerived { id, command } => {
                    let old_role = self.roles.get(id).cloned();
                    let old_key = self.binding(*id).ok_or(DocError::BindingMissing { id: *id })?;
                    let image = images
                        .get(old_key)
                        .map_err(|_| DocError::MissingImage { id: *id })?;
                    let new_key = images
                        .insert_invalid(image.format, image.layout)
                        .map_err(|_| DocError::MissingImage { id: *id })?;
                    self.bindings.insert(*id, new_key);
                    pending_discard.push(old_key);
                    match old_role {
                        Some(ImageRole::Derived(old_command)) => {
                            inverse_ops.push(RegistryPatchOp::SetDerived {
                                id: *id,
                                command: old_command,
                            });
                        }
                        _ => {
                            inverse_ops.push(RegistryPatchOp::SetPrimitive(*id));
                        }
                    }
                    self.roles.insert(*id, ImageRole::Derived(command.clone()));
                }
                RegistryPatchOp::SetRoot(id) => {
                    self.root = *id;
                }
            }
        }

        let swept = sweep_unreachable(&self.root, &mut self.roles, &mut self.bindings)?;
        for (id, key) in &swept {
            pending_discard.push(*key);
            if let Ok(image) = images.get(*key) {
                inverse_ops.push(RegistryPatchOp::InsertImage {
                    id: *id,
                    key: *key,
                    role: ImageRole::Primitive,
                    format: image.format,
                    layout: image.layout,
                });
            }
        }

        for key in pending_discard {
            images.discard(key);
        }

        inverse_ops.push(RegistryPatchOp::SetRoot(old_root));
        self.version = self.version.next();
        let inverse = RegistryPatch::new(inverse_ops);
        Ok(self.store_patch(PatchKind::Registry(inverse)))
    }

    pub fn apply_stored_patch(
        &mut self,
        id: SessionId,
        images: &mut ImagesSession<'_>,
        tiles: &mut TilesSession<'_>,
        atlas_id: u8,
    ) -> Result<SessionId, DocError> {
        let stored = self
            .patches
            .get(&id)
            .ok_or(DocError::InvalidSessionId { id })?;
        let stored_version = stored.version;
        if stored_version != self.version {
            return Err(DocError::VersionMismatch {
                expected: stored_version,
                actual: self.version,
            });
        }

        match stored.kind.clone() {
            PatchKind::Draw(mut patch) => {
                patch.version = stored_version; // ensure version stamp
                self.commit_draw(patch)
            }
            PatchKind::Registry(patch) => {
                self.apply_registry_patch(&patch, images, tiles, atlas_id)
            }
        }
    }

    fn store_patch(&mut self, kind: PatchKind) -> SessionId {
        let id = self.next_id;
        self.next_id += 1;
        let version = self.version;
        self.patches.insert(id, StoredPatch { version, kind });
        id
    }
}

fn sweep_unreachable(
    root: &ImageId,
    roles: &mut HashMap<ImageId, ImageRole>,
    bindings: &mut HashMap<ImageId, GlaImageKey>,
) -> Result<HashMap<ImageId, GlaImageKey>, DocError> {
    let reachable = collect_reachable(*root, roles)?;
    let mut swept = HashMap::new();
    let unreachable: Vec<ImageId> = roles.keys().copied().filter(|id| !reachable.contains(id)).collect();
    for id in unreachable {
        roles.remove(&id);
        if let Some(key) = bindings.remove(&id) {
            swept.insert(id, key);
        }
    }
    Ok(swept)
}

fn validate_document(
    root: ImageId,
    roles: &HashMap<ImageId, ImageRole>,
) -> Result<(), DocError> {
    if roles.is_empty() {
        return Err(DocError::EmptyRegistry);
    }
    if !roles.contains_key(&root) {
        return Err(DocError::MissingRoot { root });
    }

    let reachable = collect_reachable(root, roles)?;
    for id in roles.keys().copied() {
        if !reachable.contains(&id) {
            return Err(DocError::UnreachableImage { id });
        }
    }

    validate_no_cycles_or_self_reads(roles)?;
    Ok(())
}

fn collect_reachable(
    root: ImageId,
    roles: &HashMap<ImageId, ImageRole>,
) -> Result<HashSet<ImageId>, DocError> {
    let mut scanned = HashSet::new();
    let mut frontier = vec![root];
    while let Some(id) = frontier.pop() {
        if !scanned.insert(id) {
            continue;
        }
        if let Some(ImageRole::Derived(command)) = roles.get(&id) {
            for read in &command.reads {
                if !roles.contains_key(&read.image) {
                    return Err(DocError::MissingImage { id: read.image });
                }
                frontier.push(read.image);
            }
        }
    }
    Ok(scanned)
}

fn validate_no_cycles_or_self_reads(
    roles: &HashMap<ImageId, ImageRole>,
) -> Result<(), DocError> {
    for (id, role) in roles {
        if let ImageRole::Derived(command) = role {
            for read in &command.reads {
                if read.image == *id {
                    return Err(DocError::RegistryCommandReadsDestination { dst: *id });
                }
            }
        }
    }

    let mut out_edges = HashMap::<ImageId, Vec<ImageId>>::new();
    let mut in_degree = HashMap::<ImageId, usize>::new();
    for id in roles.keys() {
        in_degree.entry(*id).or_insert(0);
        out_edges.entry(*id).or_default();
    }
    for (id, role) in roles {
        if let ImageRole::Derived(command) = role {
            for read in &command.reads {
                out_edges.entry(*id).or_default().push(read.image);
                *in_degree.entry(read.image).or_insert(0) += 1;
            }
        }
    }

    let mut queue: Vec<ImageId> = in_degree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut visited = 0usize;
    while let Some(id) = queue.pop() {
        visited += 1;
        if let Some(outs) = out_edges.get(&id) {
            for out in outs {
                let deg = in_degree.get_mut(out).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push(*out);
                }
            }
        }
    }

    if visited < roles.len() {
        let cycle_image = roles
            .keys()
            .find(|id| in_degree.get(id).copied().unwrap_or(0) > 0)
            .copied()
            .unwrap_or(ImageId::new(0));
        return Err(DocError::RegistryCycle { id: cycle_image });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gla_color::{ChannelCount, ChannelType, GlaFormat};
    use gla_image::GlaImageLayout;
    use tile_key::Tiles;

    fn format() -> GlaFormat {
        GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::U8,
        }
    }

    fn layout() -> GlaImageLayout {
        GlaImageLayout::new(1024, 1024)
    }

    fn primitive_role() -> ImageRole {
        ImageRole::Primitive
    }

    fn key(value: u32) -> GlaImageKey {
        GlaImageKey::new(value, 0)
    }

    fn make_tile_session(tiles: &mut Tiles) -> TilesSession<'_> {
        tiles.new_atlas(atlas::AtlasLayout::LARGE17, format());
        TilesSession::new(tiles)
    }

    fn simple_doc(root: ImageId) -> Document {
        Document::new(
            root,
            HashMap::from([(root, primitive_role())]),
            HashMap::from([(root, key(10))]),
        )
        .unwrap()
    }

    #[test]
    fn document_rejects_unreachable_images() {
        let root = ImageId::new(1);
        let extra = ImageId::new(2);
        let roles = HashMap::from([(root, primitive_role()), (extra, primitive_role())]);

        let err = Document::new(root, roles, HashMap::new()).unwrap_err();
        assert!(matches!(err, DocError::UnreachableImage { id } if id == extra));
    }

    #[test]
    fn document_rejects_cycles() {
        let a = ImageId::new(1);
        let b = ImageId::new(2);
        let mut roles = HashMap::new();
        roles.insert(a, ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(b)])));
        roles.insert(b, ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(a)])));

        let err = Document::new(a, roles, HashMap::new()).unwrap_err();
        assert!(matches!(err, DocError::RegistryCycle { .. }));
    }

    #[test]
    fn document_rejects_self_read() {
        let root = ImageId::new(1);
        let roles = HashMap::from([(
            root,
            ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(root)])),
        )]);

        let err = Document::new(root, roles, HashMap::new()).unwrap_err();
        assert!(matches!(err, DocError::RegistryCommandReadsDestination { .. }));
    }

    #[test]
    fn commit_draw_returns_session_id_and_stores_inverse() {
        let root = ImageId::new(1);
        let after_key = key(11);
        let mut doc = simple_doc(root);
        assert_eq!(doc.version(), DocumentVersionId::new(0));

        let patch = DrawPatch::new(HashMap::from([(root, after_key)]), TileSet::single(3));
        let id = doc.commit_draw(patch).unwrap();

        assert_eq!(doc.binding(root), Some(after_key));
        assert_eq!(doc.version(), DocumentVersionId::new(1));
        assert_eq!(doc.stored_patch_version(id), Some(DocumentVersionId::new(1)));
    }

    #[test]
    fn apply_stored_draw_patch_undoes_and_stores_redo() {
        let root = ImageId::new(1);
        let before_key = key(10);
        let after_key = key(11);
        let mut doc = simple_doc(root);

        let forward = DrawPatch::new(HashMap::from([(root, after_key)]), TileSet::single(3));
        let undo_id = doc.commit_draw(forward).unwrap();
        assert_eq!(doc.binding(root), Some(after_key));

        let redo_id = doc.apply_stored_patch(undo_id, &mut ImagesSession::new(&mut gla_image::GlaImages::new()), &mut TilesSession::new(&mut Tiles::new()), 0).unwrap();
        assert_eq!(doc.binding(root), Some(before_key));
        assert_eq!(doc.version(), DocumentVersionId::new(2));

        let undo_id2 = doc.apply_stored_patch(redo_id, &mut ImagesSession::new(&mut gla_image::GlaImages::new()), &mut TilesSession::new(&mut Tiles::new()), 0).unwrap();
        assert_eq!(doc.binding(root), Some(after_key));
        assert_ne!(undo_id2, undo_id);
    }

    #[test]
    fn apply_stored_patch_fails_on_version_mismatch() {
        let root = ImageId::new(1);
        let after_key = key(11);
        let mut doc = simple_doc(root);

        let patch = DrawPatch::new(HashMap::from([(root, after_key)]), TileSet::single(3));
        let id = doc.commit_draw(patch).unwrap();
        // doc is now at version 1. commit again to move further.
        let _id2 = doc.commit_draw(DrawPatch::new(HashMap::new(), TileSet::default())).unwrap();
        // doc is at version 2. stored patch expects version 1.

        let err = doc.apply_stored_patch(id, &mut ImagesSession::new(&mut gla_image::GlaImages::new()), &mut TilesSession::new(&mut Tiles::new()), 0).unwrap_err();
        assert!(matches!(err, DocError::VersionMismatch { expected, actual } if expected == DocumentVersionId::new(1) && actual == DocumentVersionId::new(2)));
    }

    #[test]
    fn commit_draw_errors_on_missing_binding() {
        let root = ImageId::new(1);
        let mut doc = simple_doc(root);
        let missing = ImageId::new(99);
        let patch = DrawPatch::new(HashMap::from([(missing, key(99))]), TileSet::default());

        let err = doc.commit_draw(patch).unwrap_err();
        assert!(matches!(err, DocError::BindingMissing { id } if id == missing));
    }

    #[test]
    fn empty_draw_patch_is_valid() {
        let root = ImageId::new(1);
        let mut doc = simple_doc(root);
        let patch = DrawPatch::new(HashMap::new(), TileSet::default());
        let id = doc.commit_draw(patch).unwrap();
        let stored = doc.stored_patch_version(id);
        assert!(stored.is_some());
    }

    #[test]
    fn apply_registry_patch_adds_derived_root_and_stores_inverse() {
        let root = ImageId::new(1);
        let new_root = ImageId::new(2);
        let mut doc = simple_doc(root);
        let mut images = gla_image::GlaImages::new();
        let mut image_session = ImagesSession::new(&mut images);
        let mut tiles = Tiles::new();
        let mut tile_session = make_tile_session(&mut tiles);

        let patch = RegistryPatch::new(vec![
            RegistryPatchOp::NewImage {
                id: new_root,
                format: format(),
                layout: layout(),
                role: ImageRole::Derived(GraphCommand::new(
                    vec![GraphRead::current(root)],
                )),
            },
            RegistryPatchOp::SetRoot(new_root),
        ]);

        let id = doc.apply_registry_patch(&patch, &mut image_session, &mut tile_session, 0).unwrap();
        assert_eq!(doc.root(), new_root);
        assert!(doc.binding(new_root).is_some());
        assert_eq!(doc.stored_patch_version(id), Some(DocumentVersionId::new(1)));
    }

    #[test]
    fn registry_patch_undo_redo_via_stored_patches() {
        let root = ImageId::new(1);
        let new_root = ImageId::new(2);
        let mut doc = simple_doc(root);
        let mut images = gla_image::GlaImages::new();
        let mut image_session = ImagesSession::new(&mut images);
        let mut tiles = Tiles::new();
        let mut tile_session = make_tile_session(&mut tiles);
        let orig_root = doc.root();

        let patch = RegistryPatch::new(vec![
            RegistryPatchOp::NewImage {
                id: new_root,
                format: format(),
                layout: layout(),
                role: ImageRole::Derived(GraphCommand::new(
                    vec![GraphRead::current(root)],
                )),
            },
            RegistryPatchOp::SetRoot(new_root),
        ]);
        let undo_id = doc.apply_registry_patch(&patch, &mut image_session, &mut tile_session, 0).unwrap();
        assert_eq!(doc.root(), new_root);

        let redo_id = doc.apply_stored_patch(undo_id, &mut image_session, &mut tile_session, 0).unwrap();
        assert_eq!(doc.root(), orig_root);
        assert!(!doc.roles.contains_key(&new_root));

        let _ = doc.apply_stored_patch(redo_id, &mut image_session, &mut tile_session, 0).unwrap();
        assert_eq!(doc.root(), new_root);
    }

    #[test]
    fn registry_patch_sweeps_unreachable_after_set_derived_change() {
        let keep = ImageId::new(1);
        let drop = ImageId::new(2);
        let root = ImageId::new(3);
        let mut roles = HashMap::new();
        roles.insert(keep, primitive_role());
        roles.insert(drop, primitive_role());
        roles.insert(root, ImageRole::Derived(GraphCommand::new(
            vec![GraphRead::current(keep), GraphRead::current(drop)],
        )));

        let mut images = gla_image::GlaImages::new();
        let mut image_session = ImagesSession::new(&mut images);
        let mut tiles = Tiles::new();
        let mut tile_session = make_tile_session(&mut tiles);
        let key_keep = image_session.insert_invalid(format(), GlaImageLayout::new(64, 64)).unwrap();
        let key_drop = image_session.insert_invalid(format(), GlaImageLayout::new(64, 64)).unwrap();
        let key_root = image_session.insert_invalid(format(), GlaImageLayout::new(64, 64)).unwrap();
        let bindings = HashMap::from([
            (keep, key_keep),
            (drop, key_drop),
            (root, key_root),
        ]);
        let mut doc = Document::new(root, roles, bindings).unwrap();

        let patch = RegistryPatch::new(vec![RegistryPatchOp::SetDerived {
            id: root,
            command: GraphCommand::new(vec![GraphRead::current(keep)]),
        }]);
        doc.apply_registry_patch(&patch, &mut image_session, &mut tile_session, 0).unwrap();

        assert!(!doc.roles.contains_key(&drop));
        assert!(!doc.bindings.contains_key(&drop));
    }
}
