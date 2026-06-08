use gla_image::{GlaImageKey, GlaImages, TileSet};
use gla_ir::*;
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use tile_key::{TileKey, Tiles};

/// Re-exported from gla_ir.
pub use gla_ir::ImageRole;

/// Opaque handle to a stored patch. Janet passes this to apply undo/redo.
pub type SessionId = u64;

#[derive(Clone, Debug, PartialEq)]
pub struct DrawPatch {
    pub version: DocumentVersionId,
    pub bindings: HashMap<ImageId, GlaImageKey>,
    pub dirty: TileSet,
    pub tile_keys: Vec<TileKey>,
}

impl DrawPatch {
    pub fn new(bindings: HashMap<ImageId, GlaImageKey>, dirty: TileSet) -> Self {
        Self {
            version: DocumentVersionId::default(),
            bindings,
            dirty,
            tile_keys: Vec::new(),
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
    DerivedImageNotMaterialized {
        id: ImageId,
    },
    InvalidSessionId {
        id: SessionId,
    },
    VersionMismatch {
        expected: DocumentVersionId,
        actual: DocumentVersionId,
    },
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
            Self::DerivedImageNotMaterialized { id } => {
                write!(f, "derived image {id:?} is not fully materialized")
            }
            Self::InvalidSessionId { id } => write!(f, "invalid session id {id}"),
            Self::VersionMismatch { expected, actual } => {
                write!(
                    f,
                    "version mismatch: expected {expected:?}, actual {actual:?}"
                )
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
            tile_keys: Vec::new(),
        };
        patch.version = self.version;
        let id = self.store_patch(PatchKind::Draw(inverse));
        Ok(id)
    }

    pub fn apply_registry_patch(
        &mut self,
        patch: &RegistryPatch,
        images: &mut GlaImages,
        tiles: &mut Tiles,
        atlas_id: u8,
    ) -> Result<SessionId, DocError> {
        let old_root = self.root;
        let mut overlay = RegistryOverlay::new(self.root, &self.roles, &self.bindings);
        let mut inverse_ops = Vec::new();
        let mut allocated = Vec::new();
        let mut derived_discard = Vec::new();

        for op in &patch.ops {
            match op {
                RegistryPatchOp::NewImage {
                    id,
                    format,
                    layout,
                    role,
                } => {
                    if overlay.role(*id).is_some() {
                        continue;
                    }
                    let image = match images.alloc(*format, *layout, tiles, atlas_id) {
                        Ok(image) => image,
                        Err(_) => {
                            free_allocated_images(images, tiles, &allocated);
                            return Err(DocError::MissingImage { id: *id });
                        }
                    };
                    allocated.push(image);
                    overlay.set_image(*id, role.clone(), image);
                }
                RegistryPatchOp::InsertImage {
                    id,
                    key,
                    role,
                    format: _,
                    layout: _,
                } => {
                    if let Some(replaced) = overlay.image(*id) {
                        match replaced.role {
                            ImageRole::Primitive => {
                                let image = match images.get(replaced.key) {
                                    Ok(image) => image,
                                    Err(_) => {
                                        free_allocated_images(images, tiles, &allocated);
                                        return Err(DocError::MissingImage { id: *id });
                                    }
                                };
                                inverse_ops.push(RegistryPatchOp::InsertImage {
                                    id: *id,
                                    key: replaced.key,
                                    role: ImageRole::Primitive,
                                    format: image.format,
                                    layout: image.layout,
                                });
                            }
                            ImageRole::Derived(command) => {
                                if let Err(err) = push_replaced_derived_inverse(
                                    &mut inverse_ops,
                                    &mut derived_discard,
                                    images,
                                    old_root,
                                    *id,
                                    replaced.key,
                                    command,
                                ) {
                                    free_allocated_images(images, tiles, &allocated);
                                    return Err(err);
                                }
                            }
                        }
                    }
                    overlay.set_image(*id, role.clone(), *key);
                }
                RegistryPatchOp::SetPrimitive(id) => {
                    if let Some(old_image) = overlay.image(*id) {
                        match old_image.role {
                            ImageRole::Derived(command) => {
                                if let Err(err) =
                                    ensure_image_materialized(images, old_image.key, *id)
                                {
                                    free_allocated_images(images, tiles, &allocated);
                                    return Err(err);
                                }
                                overlay.set_role(*id, gla_ir::ImageRole::Primitive);
                                if *id == old_root {
                                    if let Err(err) = push_insert_image_inverse(
                                        &mut inverse_ops,
                                        images,
                                        *id,
                                        old_image.key,
                                        ImageRole::Derived(command),
                                    ) {
                                        free_allocated_images(images, tiles, &allocated);
                                        return Err(err);
                                    }
                                } else {
                                    inverse_ops
                                        .push(RegistryPatchOp::SetDerived { id: *id, command });
                                }
                            }
                            ImageRole::Primitive => {}
                        }
                    }
                }
                RegistryPatchOp::SetDerived { id, command } => {
                    let old_image = match overlay.image(*id) {
                        Some(image) => image,
                        None => {
                            free_allocated_images(images, tiles, &allocated);
                            return Err(DocError::BindingMissing { id: *id });
                        }
                    };
                    let image = images
                        .get(old_image.key)
                        .map_err(|_| DocError::MissingImage { id: *id });
                    let image = match image {
                        Ok(image) => image,
                        Err(err) => {
                            free_allocated_images(images, tiles, &allocated);
                            return Err(err);
                        }
                    };
                    let old_format = image.format;
                    let old_layout = image.layout;
                    let new_key = match images.insert_invalid(old_format, old_layout) {
                        Ok(key) => key,
                        Err(_) => {
                            free_allocated_images(images, tiles, &allocated);
                            return Err(DocError::MissingImage { id: *id });
                        }
                    };
                    allocated.push(new_key);
                    match old_image.role {
                        ImageRole::Primitive => {
                            inverse_ops.push(RegistryPatchOp::InsertImage {
                                id: *id,
                                key: old_image.key,
                                role: ImageRole::Primitive,
                                format: old_format,
                                layout: old_layout,
                            });
                        }
                        ImageRole::Derived(old_command) => {
                            if let Err(err) = push_replaced_derived_inverse(
                                &mut inverse_ops,
                                &mut derived_discard,
                                images,
                                old_root,
                                *id,
                                old_image.key,
                                old_command,
                            ) {
                                free_allocated_images(images, tiles, &allocated);
                                return Err(err);
                            }
                        }
                    }
                    overlay.set_image(*id, ImageRole::Derived(command.clone()), new_key);
                }
                RegistryPatchOp::SetRoot(id) => {
                    overlay.set_root(*id);
                }
            }
        }

        let swept = match sweep_unreachable_overlay(&mut overlay) {
            Ok(swept) => swept,
            Err(err) => {
                free_allocated_images(images, tiles, &allocated);
                return Err(err);
            }
        };
        if let Err(err) = validate_registry_view(&overlay) {
            free_allocated_images(images, tiles, &allocated);
            return Err(err);
        }
        for (id, key) in &swept {
            let image = match images.get(key.key) {
                Ok(image) => image,
                Err(_) => {
                    free_allocated_images(images, tiles, &allocated);
                    return Err(DocError::MissingImage { id: *id });
                }
            };
            match &key.role {
                ImageRole::Primitive => {
                    inverse_ops.push(RegistryPatchOp::InsertImage {
                        id: *id,
                        key: key.key,
                        role: ImageRole::Primitive,
                        format: image.format,
                        layout: image.layout,
                    });
                }
                ImageRole::Derived(command) => {
                    if *id == old_root {
                        inverse_ops.push(RegistryPatchOp::InsertImage {
                            id: *id,
                            key: key.key,
                            role: ImageRole::Derived(command.clone()),
                            format: image.format,
                            layout: image.layout,
                        });
                    } else {
                        inverse_ops.push(RegistryPatchOp::NewImage {
                            id: *id,
                            format: image.format,
                            layout: image.layout,
                            role: ImageRole::Derived(command.clone()),
                        });
                        derived_discard.push(key.key);
                    }
                }
            }
        }

        let (new_root, role_changes, binding_changes) = overlay.into_changes();
        publish_registry_changes(
            new_root,
            role_changes,
            binding_changes,
            &mut self.root,
            &mut self.roles,
            &mut self.bindings,
        );
        free_images_and_tiles(images, tiles, &derived_discard);
        inverse_ops.push(RegistryPatchOp::SetRoot(old_root));
        self.version = self.version.next();
        let inverse = RegistryPatch::new(inverse_ops);
        Ok(self.store_patch(PatchKind::Registry(inverse)))
    }

    pub fn apply_stored_patch(
        &mut self,
        id: SessionId,
        images: &mut GlaImages,
        tiles: &mut Tiles,
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

trait RegistryView {
    fn root(&self) -> ImageId;
    fn role(&self, id: ImageId) -> Option<&ImageRole>;
    fn role_ids(&self) -> Vec<ImageId>;
}

struct RegistryMapView<'a> {
    root: ImageId,
    roles: &'a HashMap<ImageId, ImageRole>,
}

impl RegistryView for RegistryMapView<'_> {
    fn root(&self) -> ImageId {
        self.root
    }

    fn role(&self, id: ImageId) -> Option<&ImageRole> {
        self.roles.get(&id)
    }

    fn role_ids(&self) -> Vec<ImageId> {
        self.roles.keys().copied().collect()
    }
}

struct RegistryOverlay<'a> {
    root: ImageId,
    base_roles: &'a HashMap<ImageId, ImageRole>,
    base_bindings: &'a HashMap<ImageId, GlaImageKey>,
    roles: HashMap<ImageId, Option<ImageRole>>,
    bindings: HashMap<ImageId, Option<GlaImageKey>>,
}

#[derive(Clone, Debug)]
struct RemovedImage {
    role: ImageRole,
    key: GlaImageKey,
}

impl<'a> RegistryOverlay<'a> {
    fn new(
        root: ImageId,
        base_roles: &'a HashMap<ImageId, ImageRole>,
        base_bindings: &'a HashMap<ImageId, GlaImageKey>,
    ) -> Self {
        Self {
            root,
            base_roles,
            base_bindings,
            roles: HashMap::new(),
            bindings: HashMap::new(),
        }
    }

    fn binding(&self, id: ImageId) -> Option<GlaImageKey> {
        match self.bindings.get(&id) {
            Some(Some(key)) => Some(*key),
            Some(None) => None,
            None => self.base_bindings.get(&id).copied(),
        }
    }

    fn image(&self, id: ImageId) -> Option<RemovedImage> {
        Some(RemovedImage {
            role: self.role(id)?.clone(),
            key: self.binding(id)?,
        })
    }

    fn set_root(&mut self, id: ImageId) {
        self.root = id;
    }

    fn set_role(&mut self, id: ImageId, role: ImageRole) {
        self.roles.insert(id, Some(role));
    }

    fn set_binding(&mut self, id: ImageId, key: GlaImageKey) {
        self.bindings.insert(id, Some(key));
    }

    fn set_image(&mut self, id: ImageId, role: ImageRole, key: GlaImageKey) {
        self.set_role(id, role);
        self.set_binding(id, key);
    }

    fn remove_image(&mut self, id: ImageId) -> Option<RemovedImage> {
        let image = self.image(id);
        self.roles.insert(id, None);
        self.bindings.insert(id, None);
        image
    }

    fn into_changes(
        self,
    ) -> (
        ImageId,
        HashMap<ImageId, Option<ImageRole>>,
        HashMap<ImageId, Option<GlaImageKey>>,
    ) {
        (self.root, self.roles, self.bindings)
    }
}

fn publish_registry_changes(
    new_root: ImageId,
    role_changes: HashMap<ImageId, Option<ImageRole>>,
    binding_changes: HashMap<ImageId, Option<GlaImageKey>>,
    root: &mut ImageId,
    roles: &mut HashMap<ImageId, ImageRole>,
    bindings: &mut HashMap<ImageId, GlaImageKey>,
) {
    *root = new_root;
    for (id, role) in role_changes {
        match role {
            Some(role) => {
                roles.insert(id, role);
            }
            None => {
                roles.remove(&id);
            }
        }
    }
    for (id, key) in binding_changes {
        match key {
            Some(key) => {
                bindings.insert(id, key);
            }
            None => {
                bindings.remove(&id);
            }
        }
    }
}

impl RegistryView for RegistryOverlay<'_> {
    fn root(&self) -> ImageId {
        self.root
    }

    fn role(&self, id: ImageId) -> Option<&ImageRole> {
        match self.roles.get(&id) {
            Some(Some(role)) => Some(role),
            Some(None) => None,
            None => self.base_roles.get(&id),
        }
    }

    fn role_ids(&self) -> Vec<ImageId> {
        let mut ids: HashSet<ImageId> = self.base_roles.keys().copied().collect();
        for (id, role) in &self.roles {
            if role.is_some() {
                ids.insert(*id);
            } else {
                ids.remove(id);
            }
        }
        ids.into_iter().collect()
    }
}

fn ensure_image_materialized(
    images: &GlaImages,
    key: GlaImageKey,
    id: ImageId,
) -> Result<(), DocError> {
    let image = images.get(key).map_err(|_| DocError::MissingImage { id })?;
    if image.tiles.iter().any(|tile| tile.is_invalid()) {
        return Err(DocError::DerivedImageNotMaterialized { id });
    }
    Ok(())
}

fn push_insert_image_inverse(
    inverse_ops: &mut Vec<RegistryPatchOp>,
    images: &GlaImages,
    id: ImageId,
    key: GlaImageKey,
    role: ImageRole,
) -> Result<(), DocError> {
    let image = images.get(key).map_err(|_| DocError::MissingImage { id })?;
    inverse_ops.push(RegistryPatchOp::InsertImage {
        id,
        key,
        role,
        format: image.format,
        layout: image.layout,
    });
    Ok(())
}

fn push_replaced_derived_inverse(
    inverse_ops: &mut Vec<RegistryPatchOp>,
    derived_discard: &mut Vec<GlaImageKey>,
    images: &GlaImages,
    old_root: ImageId,
    id: ImageId,
    key: GlaImageKey,
    command: GraphCommand,
) -> Result<(), DocError> {
    if id == old_root {
        push_insert_image_inverse(inverse_ops, images, id, key, ImageRole::Derived(command))
    } else {
        inverse_ops.push(RegistryPatchOp::SetDerived { id, command });
        derived_discard.push(key);
        Ok(())
    }
}

fn free_allocated_images(images: &mut GlaImages, tiles: &mut Tiles, keys: &[GlaImageKey]) {
    free_images_and_tiles(images, tiles, keys);
}

fn free_images_and_tiles(images: &mut GlaImages, tiles: &mut Tiles, keys: &[GlaImageKey]) {
    for key in keys {
        free_image_and_tiles(images, tiles, *key);
    }
}

fn free_image_and_tiles(images: &mut GlaImages, tiles: &mut Tiles, key: GlaImageKey) {
    if let Ok(image) = images.get(key) {
        let tile_keys: Vec<TileKey> = image
            .tiles
            .iter()
            .copied()
            .filter(|tile| !tile.is_invalid())
            .collect();
        tiles.discard_batch(&tile_keys);
    }
    let _ = images.free(key);
}

fn sweep_unreachable_overlay(
    overlay: &mut RegistryOverlay<'_>,
) -> Result<HashMap<ImageId, RemovedImage>, DocError> {
    if overlay.role(overlay.root()).is_none() {
        return Err(DocError::MissingRoot {
            root: overlay.root(),
        });
    }
    let reachable = collect_reachable_view(overlay)?;
    let mut swept = HashMap::new();
    let unreachable: Vec<ImageId> = overlay
        .role_ids()
        .into_iter()
        .filter(|id| !reachable.contains(id))
        .collect();
    for id in unreachable {
        if let Some(key) = overlay.remove_image(id) {
            swept.insert(id, key);
        }
    }
    Ok(swept)
}

fn sweep_unreachable(
    root: &ImageId,
    roles: &mut HashMap<ImageId, ImageRole>,
    bindings: &mut HashMap<ImageId, GlaImageKey>,
) -> Result<HashMap<ImageId, GlaImageKey>, DocError> {
    let reachable = collect_reachable(*root, roles)?;
    let mut swept = HashMap::new();
    let unreachable: Vec<ImageId> = roles
        .keys()
        .copied()
        .filter(|id| !reachable.contains(id))
        .collect();
    for id in unreachable {
        roles.remove(&id);
        if let Some(key) = bindings.remove(&id) {
            swept.insert(id, key);
        }
    }
    Ok(swept)
}

fn validate_document(root: ImageId, roles: &HashMap<ImageId, ImageRole>) -> Result<(), DocError> {
    validate_registry_view(&RegistryMapView { root, roles })
}

fn validate_registry_view(view: &impl RegistryView) -> Result<(), DocError> {
    let role_ids = view.role_ids();
    if role_ids.is_empty() {
        return Err(DocError::EmptyRegistry);
    }
    let root = view.root();
    if view.role(root).is_none() {
        return Err(DocError::MissingRoot { root });
    }

    let reachable = collect_reachable_view(view)?;
    for id in role_ids {
        if !reachable.contains(&id) {
            return Err(DocError::UnreachableImage { id });
        }
    }

    validate_no_cycles_or_self_reads_view(view)?;
    Ok(())
}

fn collect_reachable(
    root: ImageId,
    roles: &HashMap<ImageId, ImageRole>,
) -> Result<HashSet<ImageId>, DocError> {
    collect_reachable_view(&RegistryMapView { root, roles })
}

fn collect_reachable_view(view: &impl RegistryView) -> Result<HashSet<ImageId>, DocError> {
    let mut scanned = HashSet::new();
    let mut frontier = vec![view.root()];
    while let Some(id) = frontier.pop() {
        if !scanned.insert(id) {
            continue;
        }
        if let Some(ImageRole::Derived(command)) = view.role(id) {
            for read in &command.reads {
                if view.role(read.image).is_none() {
                    return Err(DocError::MissingImage { id: read.image });
                }
                frontier.push(read.image);
            }
        }
    }
    Ok(scanned)
}

fn validate_no_cycles_or_self_reads(roles: &HashMap<ImageId, ImageRole>) -> Result<(), DocError> {
    validate_no_cycles_or_self_reads_view(&RegistryMapView {
        root: ImageId::new(0),
        roles,
    })
}

fn validate_no_cycles_or_self_reads_view(view: &impl RegistryView) -> Result<(), DocError> {
    let role_ids = view.role_ids();
    for id in &role_ids {
        if let Some(ImageRole::Derived(command)) = view.role(*id) {
            for read in &command.reads {
                if read.image == *id {
                    return Err(DocError::RegistryCommandReadsDestination { dst: *id });
                }
            }
        }
    }

    let mut out_edges = HashMap::<ImageId, Vec<ImageId>>::new();
    let mut in_degree = HashMap::<ImageId, usize>::new();
    for id in &role_ids {
        in_degree.entry(*id).or_insert(0);
        out_edges.entry(*id).or_default();
    }
    for id in &role_ids {
        if let Some(ImageRole::Derived(command)) = view.role(*id) {
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

    if visited < role_ids.len() {
        let cycle_image = role_ids
            .iter()
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

    fn make_tile_atlas(tiles: &mut Tiles) {
        tiles.new_atlas(atlas::AtlasLayout::LARGE17, format());
    }

    fn fresh_resources() -> (GlaImages, Tiles) {
        (gla_image::GlaImages::new(), Tiles::new())
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
        roles.insert(
            a,
            ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(b)])),
        );
        roles.insert(
            b,
            ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(a)])),
        );

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
        assert!(matches!(
            err,
            DocError::RegistryCommandReadsDestination { .. }
        ));
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
        assert_eq!(
            doc.stored_patch_version(id),
            Some(DocumentVersionId::new(1))
        );
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

        let redo_id = doc
            .apply_stored_patch(
                undo_id,
                &mut gla_image::GlaImages::new(),
                &mut Tiles::new(),
                0,
            )
            .unwrap();
        assert_eq!(doc.binding(root), Some(before_key));
        assert_eq!(doc.version(), DocumentVersionId::new(2));

        let undo_id2 = doc
            .apply_stored_patch(
                redo_id,
                &mut gla_image::GlaImages::new(),
                &mut Tiles::new(),
                0,
            )
            .unwrap();
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
        let _id2 = doc
            .commit_draw(DrawPatch::new(HashMap::new(), TileSet::default()))
            .unwrap();
        // doc is at version 2. stored patch expects version 1.

        let err = doc
            .apply_stored_patch(id, &mut gla_image::GlaImages::new(), &mut Tiles::new(), 0)
            .unwrap_err();
        assert!(
            matches!(err, DocError::VersionMismatch { expected, actual } if expected == DocumentVersionId::new(1) && actual == DocumentVersionId::new(2))
        );
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
        let (mut images, mut tiles) = fresh_resources();
        make_tile_atlas(&mut tiles);

        let patch = RegistryPatch::new(vec![
            RegistryPatchOp::NewImage {
                id: new_root,
                format: format(),
                layout: layout(),
                role: ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(root)])),
            },
            RegistryPatchOp::SetRoot(new_root),
        ]);

        let id = doc
            .apply_registry_patch(&patch, &mut images, &mut tiles, 0)
            .unwrap();
        assert_eq!(doc.root(), new_root);
        assert!(doc.binding(new_root).is_some());
        assert_eq!(
            doc.stored_patch_version(id),
            Some(DocumentVersionId::new(1))
        );
    }

    #[test]
    fn registry_patch_undo_redo_via_stored_patches() {
        let root = ImageId::new(1);
        let new_root = ImageId::new(2);
        let mut doc = simple_doc(root);
        let (mut images, mut tiles) = fresh_resources();
        make_tile_atlas(&mut tiles);
        let orig_root = doc.root();

        let patch = RegistryPatch::new(vec![
            RegistryPatchOp::NewImage {
                id: new_root,
                format: format(),
                layout: layout(),
                role: ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(root)])),
            },
            RegistryPatchOp::SetRoot(new_root),
        ]);
        let undo_id = doc
            .apply_registry_patch(&patch, &mut images, &mut tiles, 0)
            .unwrap();
        assert_eq!(doc.root(), new_root);
        let new_root_key = doc.binding(new_root).unwrap();
        assert!(images.get(new_root_key).is_ok());

        let redo_id = doc
            .apply_stored_patch(undo_id, &mut images, &mut tiles, 0)
            .unwrap();
        assert_eq!(doc.root(), orig_root);
        assert!(!doc.roles.contains_key(&new_root));
        assert!(images.get(new_root_key).is_ok());

        let _ = doc
            .apply_stored_patch(redo_id, &mut images, &mut tiles, 0)
            .unwrap();
        assert_eq!(doc.root(), new_root);
        let redone_key = doc.binding(new_root).unwrap();
        assert_eq!(redone_key, new_root_key);
        assert!(images.get(redone_key).is_ok());
    }

    #[test]
    fn registry_patch_sweeps_unreachable_after_set_derived_change() {
        let keep = ImageId::new(1);
        let drop = ImageId::new(2);
        let root = ImageId::new(3);
        let mut roles = HashMap::new();
        roles.insert(keep, primitive_role());
        roles.insert(drop, primitive_role());
        roles.insert(
            root,
            ImageRole::Derived(GraphCommand::new(vec![
                GraphRead::current(keep),
                GraphRead::current(drop),
            ])),
        );

        let mut images = gla_image::GlaImages::new();
        let mut tiles = Tiles::new();
        make_tile_atlas(&mut tiles);
        let key_keep = images
            .insert_invalid(format(), GlaImageLayout::new(64, 64))
            .unwrap();
        let key_drop = images
            .insert_invalid(format(), GlaImageLayout::new(64, 64))
            .unwrap();
        let key_root = images
            .insert_invalid(format(), GlaImageLayout::new(64, 64))
            .unwrap();
        let bindings = HashMap::from([(keep, key_keep), (drop, key_drop), (root, key_root)]);
        let mut doc = Document::new(root, roles, bindings).unwrap();

        let patch = RegistryPatch::new(vec![RegistryPatchOp::SetDerived {
            id: root,
            command: GraphCommand::new(vec![GraphRead::current(keep)]),
        }]);
        doc.apply_registry_patch(&patch, &mut images, &mut tiles, 0)
            .unwrap();

        assert!(!doc.roles.contains_key(&drop));
        assert!(!doc.bindings.contains_key(&drop));
        assert!(images.get(key_drop).is_ok());
    }

    #[test]
    fn registry_patch_discards_swept_non_root_derived_cache() {
        let leaf = ImageId::new(1);
        let cache = ImageId::new(2);
        let root = ImageId::new(3);
        let mut roles = HashMap::new();
        roles.insert(leaf, primitive_role());
        roles.insert(
            cache,
            ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(leaf)])),
        );
        roles.insert(
            root,
            ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(cache)])),
        );

        let mut images = gla_image::GlaImages::new();
        let mut tiles = Tiles::new();
        make_tile_atlas(&mut tiles);
        let leaf_key = images
            .alloc(format(), GlaImageLayout::new(64, 64), &mut tiles, 0)
            .unwrap();
        let cache_key = images
            .alloc(format(), GlaImageLayout::new(64, 64), &mut tiles, 0)
            .unwrap();
        let root_key = images
            .alloc(format(), GlaImageLayout::new(64, 64), &mut tiles, 0)
            .unwrap();
        let bindings = HashMap::from([(leaf, leaf_key), (cache, cache_key), (root, root_key)]);
        let mut doc = Document::new(root, roles, bindings).unwrap();

        let patch = RegistryPatch::new(vec![RegistryPatchOp::SetDerived {
            id: root,
            command: GraphCommand::new(Vec::new()),
        }]);
        doc.apply_registry_patch(&patch, &mut images, &mut tiles, 0)
            .unwrap();

        assert!(!doc.roles.contains_key(&leaf));
        assert!(!doc.roles.contains_key(&cache));
        assert!(images.get(leaf_key).is_ok());
        assert!(images.get(cache_key).is_err());
        assert!(images.get(root_key).is_ok());
    }

    #[test]
    fn registry_patch_undo_restores_set_derived_old_image_key() {
        let root = ImageId::new(1);
        let mut images = gla_image::GlaImages::new();
        let mut tiles = Tiles::new();
        make_tile_atlas(&mut tiles);
        let old_key = images
            .alloc(format(), GlaImageLayout::new(64, 64), &mut tiles, 0)
            .unwrap();
        let mut doc = Document::new(
            root,
            HashMap::from([(root, primitive_role())]),
            HashMap::from([(root, old_key)]),
        )
        .unwrap();

        let patch = RegistryPatch::new(vec![RegistryPatchOp::SetDerived {
            id: root,
            command: GraphCommand::new(Vec::new()),
        }]);
        let undo_id = doc
            .apply_registry_patch(&patch, &mut images, &mut tiles, 0)
            .unwrap();
        let derived_key = doc.binding(root).unwrap();
        assert_ne!(derived_key, old_key);
        assert!(images.get(old_key).is_ok());
        assert!(images.get(derived_key).is_ok());

        let redo_id = doc
            .apply_stored_patch(undo_id, &mut images, &mut tiles, 0)
            .unwrap();
        assert_eq!(doc.binding(root), Some(old_key));
        assert_eq!(doc.role(root), Some(&ImageRole::Primitive));
        assert!(images.get(derived_key).is_ok());

        let _ = doc
            .apply_stored_patch(redo_id, &mut images, &mut tiles, 0)
            .unwrap();
        let redone_derived_key = doc.binding(root).unwrap();
        assert_eq!(redone_derived_key, derived_key);
        assert!(matches!(doc.role(root), Some(ImageRole::Derived(_))));
        assert!(images.get(old_key).is_ok());
        assert!(images.get(redone_derived_key).is_ok());
    }

    #[test]
    fn registry_patch_set_derived_preserves_replaced_root_cache() {
        let child = ImageId::new(1);
        let root = ImageId::new(2);
        let old_command = GraphCommand::new(vec![GraphRead::current(child)]);
        let new_command = GraphCommand::new(Vec::new());
        let mut roles = HashMap::new();
        roles.insert(child, primitive_role());
        roles.insert(root, ImageRole::Derived(old_command.clone()));

        let mut images = gla_image::GlaImages::new();
        let mut tiles = Tiles::new();
        make_tile_atlas(&mut tiles);
        let child_key = images
            .alloc(format(), GlaImageLayout::new(64, 64), &mut tiles, 0)
            .unwrap();
        let root_cache_key = images
            .alloc(format(), GlaImageLayout::new(64, 64), &mut tiles, 0)
            .unwrap();
        let bindings = HashMap::from([(child, child_key), (root, root_cache_key)]);
        let mut doc = Document::new(root, roles, bindings).unwrap();

        let patch = RegistryPatch::new(vec![RegistryPatchOp::SetDerived {
            id: root,
            command: new_command.clone(),
        }]);
        let undo_id = doc
            .apply_registry_patch(&patch, &mut images, &mut tiles, 0)
            .unwrap();
        let new_root_cache_key = doc.binding(root).unwrap();
        assert_ne!(new_root_cache_key, root_cache_key);
        assert!(images.get(root_cache_key).is_ok());
        assert!(!doc.roles.contains_key(&child));

        let redo_id = doc
            .apply_stored_patch(undo_id, &mut images, &mut tiles, 0)
            .unwrap();
        assert_eq!(doc.binding(root), Some(root_cache_key));
        assert_eq!(doc.role(root), Some(&ImageRole::Derived(old_command)));
        assert_eq!(doc.binding(child), Some(child_key));
        assert!(images.get(new_root_cache_key).is_ok());

        let _ = doc
            .apply_stored_patch(redo_id, &mut images, &mut tiles, 0)
            .unwrap();
        assert_eq!(doc.binding(root), Some(new_root_cache_key));
        assert_eq!(doc.role(root), Some(&ImageRole::Derived(new_command)));
        assert!(!doc.roles.contains_key(&child));
        assert!(images.get(root_cache_key).is_ok());
    }

    #[test]
    fn registry_patch_set_primitive_rejects_unmaterialized_derived() {
        let root = ImageId::new(1);
        let mut images = gla_image::GlaImages::new();
        let mut tiles = Tiles::new();
        make_tile_atlas(&mut tiles);
        let derived_key = images
            .insert_invalid(format(), GlaImageLayout::new(64, 64))
            .unwrap();
        let command = GraphCommand::new(Vec::new());
        let mut doc = Document::new(
            root,
            HashMap::from([(root, ImageRole::Derived(command.clone()))]),
            HashMap::from([(root, derived_key)]),
        )
        .unwrap();
        let version = doc.version();

        let patch = RegistryPatch::new(vec![RegistryPatchOp::SetPrimitive(root)]);
        let err = doc
            .apply_registry_patch(&patch, &mut images, &mut tiles, 0)
            .unwrap_err();

        assert!(matches!(
            err,
            DocError::DerivedImageNotMaterialized { id } if id == root
        ));
        assert_eq!(doc.version(), version);
        assert_eq!(doc.binding(root), Some(derived_key));
        assert_eq!(doc.role(root), Some(&ImageRole::Derived(command)));
    }

    #[test]
    fn registry_patch_set_primitive_reuses_materialized_key_and_redo_keeps_it() {
        let root = ImageId::new(1);
        let mut images = gla_image::GlaImages::new();
        let mut tiles = Tiles::new();
        make_tile_atlas(&mut tiles);
        let materialized_key = images
            .alloc(format(), GlaImageLayout::new(64, 64), &mut tiles, 0)
            .unwrap();
        let command = GraphCommand::new(Vec::new());
        let mut doc = Document::new(
            root,
            HashMap::from([(root, ImageRole::Derived(command))]),
            HashMap::from([(root, materialized_key)]),
        )
        .unwrap();

        let patch = RegistryPatch::new(vec![RegistryPatchOp::SetPrimitive(root)]);
        let undo_id = doc
            .apply_registry_patch(&patch, &mut images, &mut tiles, 0)
            .unwrap();
        assert_eq!(doc.binding(root), Some(materialized_key));
        assert_eq!(doc.role(root), Some(&ImageRole::Primitive));

        let redo_id = doc
            .apply_stored_patch(undo_id, &mut images, &mut tiles, 0)
            .unwrap();
        let undo_derived_key = doc.binding(root).unwrap();
        assert_eq!(undo_derived_key, materialized_key);
        assert!(matches!(doc.role(root), Some(ImageRole::Derived(_))));
        assert!(images.get(materialized_key).is_ok());

        let _ = doc
            .apply_stored_patch(redo_id, &mut images, &mut tiles, 0)
            .unwrap();
        assert_eq!(doc.binding(root), Some(materialized_key));
        assert_eq!(doc.role(root), Some(&ImageRole::Primitive));
        assert!(images.get(undo_derived_key).is_ok());
    }

    #[test]
    fn registry_patch_rejects_cycle_without_publishing() {
        let child = ImageId::new(1);
        let root = ImageId::new(2);
        let mut roles = HashMap::new();
        roles.insert(child, primitive_role());
        roles.insert(
            root,
            ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(child)])),
        );

        let mut images = gla_image::GlaImages::new();
        let mut tiles = Tiles::new();
        make_tile_atlas(&mut tiles);
        let child_key = images
            .insert_invalid(format(), GlaImageLayout::new(64, 64))
            .unwrap();
        let root_key = images
            .insert_invalid(format(), GlaImageLayout::new(64, 64))
            .unwrap();
        let bindings = HashMap::from([(child, child_key), (root, root_key)]);
        let mut doc = Document::new(root, roles, bindings).unwrap();
        let version = doc.version();

        let patch = RegistryPatch::new(vec![RegistryPatchOp::SetDerived {
            id: child,
            command: GraphCommand::new(vec![GraphRead::current(root)]),
        }]);

        let err = doc
            .apply_registry_patch(&patch, &mut images, &mut tiles, 0)
            .unwrap_err();
        assert!(matches!(err, DocError::RegistryCycle { .. }));
        assert_eq!(doc.version(), version);
        assert_eq!(doc.root(), root);
        assert_eq!(doc.role(child), Some(&ImageRole::Primitive));
        assert_eq!(doc.binding(child), Some(child_key));
    }

    #[test]
    fn registry_patch_rejects_missing_root_without_publishing() {
        let root = ImageId::new(1);
        let missing = ImageId::new(2);
        let mut doc = simple_doc(root);
        let version = doc.version();
        let (mut images, mut tiles) = fresh_resources();

        let patch = RegistryPatch::new(vec![RegistryPatchOp::SetRoot(missing)]);

        let err = doc
            .apply_registry_patch(&patch, &mut images, &mut tiles, 0)
            .unwrap_err();
        assert!(matches!(err, DocError::MissingRoot { root } if root == missing));
        assert_eq!(doc.version(), version);
        assert_eq!(doc.root(), root);
    }
}
