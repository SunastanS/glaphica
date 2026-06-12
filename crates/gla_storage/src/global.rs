use crate::edit::ImageEdit;
use crate::error::{GlobalEditApplyError, GlobalEditError, GlobalStorageError, GlobalTileError};
use crate::graph::{ImageSpec, validate_specs};
use atlas::TilePos;
use gla_color::GlaFormat;
use gla_image::{CacheImage, DenseImage, GlaImageLayout};
use gla_ir::{DocumentVersionId, GraphCommand, ImageId, ImageRole, RegistryPatch, RegistryPatchOp};
use std::collections::{HashMap, HashSet};
use tile_key::{Tile, TileReadRef, Tiles, TilesError};

#[derive(Debug)]
pub enum GlobalImage {
    Primitive(DenseImage),
    Derived {
        image: CacheImage,
        command: GraphCommand,
    },
}

impl GlobalImage {
    pub fn format(&self) -> GlaFormat {
        match self {
            Self::Primitive(image) => image.format(),
            Self::Derived { image, .. } => image.format(),
        }
    }

    pub fn layout(&self) -> GlaImageLayout {
        match self {
            Self::Primitive(image) => image.layout(),
            Self::Derived { image, .. } => image.layout(),
        }
    }

    pub fn role(&self) -> ImageRole {
        match self {
            Self::Primitive(_) => ImageRole::Primitive,
            Self::Derived { command, .. } => ImageRole::Derived(command.clone()),
        }
    }

    pub fn as_dense(&self) -> Option<&DenseImage> {
        match self {
            Self::Primitive(image) => Some(image),
            Self::Derived { .. } => None,
        }
    }

    pub fn as_cache(&self) -> Option<&CacheImage> {
        match self {
            Self::Primitive(_) => None,
            Self::Derived { image, .. } => Some(image),
        }
    }

    pub fn graph_command(&self) -> Option<&GraphCommand> {
        match self {
            Self::Primitive(_) => None,
            Self::Derived { command, .. } => Some(command),
        }
    }

    fn release_tiles(self, tiles: &mut Tiles) {
        match self {
            Self::Primitive(image) => image.release_tiles(tiles),
            Self::Derived { image, .. } => image.release_tiles(tiles),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlobalEditTarget {
    Primitive,
    Derived,
}

pub struct GlobalStorage {
    version: DocumentVersionId,
    root: Option<ImageId>,
    images: HashMap<ImageId, GlobalImage>,
    tiles: Tiles,
}

impl GlobalStorage {
    pub fn new(tiles: Tiles) -> Self {
        Self {
            version: DocumentVersionId::default(),
            root: None,
            images: HashMap::new(),
            tiles,
        }
    }

    pub fn version(&self) -> DocumentVersionId {
        self.version
    }

    pub fn bump_version(&mut self) -> DocumentVersionId {
        self.version = self.version.next();
        self.version
    }

    pub fn root(&self) -> Option<ImageId> {
        self.root
    }

    pub fn image(&self, id: ImageId) -> Option<&GlobalImage> {
        self.images.get(&id)
    }

    pub fn images(&self) -> &HashMap<ImageId, GlobalImage> {
        &self.images
    }

    pub fn tiles(&self) -> &Tiles {
        &self.tiles
    }

    pub fn tiles_mut(&mut self) -> &mut Tiles {
        &mut self.tiles
    }

    pub fn read_tile_ref(&self, tile: &Tile) -> Result<TileReadRef, TilesError> {
        self.tiles.read_ref(tile)
    }

    pub fn write_tile_pos(&mut self, tile: &mut Tile) -> Result<TilePos, TilesError> {
        self.tiles.write_pos(tile)
    }

    pub fn write_tile_pos_with_zero_init(
        &mut self,
        tile: &mut Tile,
        init_zero: impl FnOnce(TilePos),
    ) -> Result<TilePos, TilesError> {
        self.tiles.write_pos_with_zero_init(tile, init_zero)
    }

    pub fn reserve_tile_for_format(&mut self, format: GlaFormat) -> Result<Tile, TilesError> {
        self.tiles.reserve_for_format(format)
    }

    fn release_image_edit(&mut self, edit: ImageEdit) {
        edit.release_tiles(&mut self.tiles);
    }

    pub fn read_global_ref(
        &self,
        id: ImageId,
        tile_index: u32,
    ) -> Result<TileReadRef, GlobalTileError> {
        let image = self.image(id).ok_or(GlobalTileError::MissingImage { id })?;
        match image {
            GlobalImage::Primitive(image) => {
                let tile = image
                    .tile(tile_index)
                    .map_err(|source| GlobalTileError::Image { id, source })?;
                self.tiles
                    .read_ref(tile)
                    .map_err(|source| GlobalTileError::Tile { id, source })
            }
            GlobalImage::Derived { image, .. } => {
                let tile = image
                    .tile(tile_index)
                    .map_err(|source| GlobalTileError::Image { id, source })?
                    .ok_or(GlobalTileError::MissingMaterializedTile { id })?;
                self.tiles
                    .read_ref(tile)
                    .map_err(|source| GlobalTileError::Tile { id, source })
            }
        }
    }

    pub fn write_global_cache_pos(
        &mut self,
        id: ImageId,
        tile_index: u32,
    ) -> Result<TilePos, GlobalTileError> {
        self.write_global_cache_pos_with_zero_init(id, tile_index, |_| {})
    }

    pub fn write_global_cache_pos_with_zero_init(
        &mut self,
        id: ImageId,
        tile_index: u32,
        init_zero: impl FnOnce(TilePos),
    ) -> Result<TilePos, GlobalTileError> {
        let image = self
            .images
            .get_mut(&id)
            .ok_or(GlobalTileError::MissingImage { id })?;
        match image {
            GlobalImage::Primitive(_) => Err(GlobalTileError::GlobalPrimitiveWrite { id }),
            GlobalImage::Derived { image, .. } => {
                if image
                    .tile(tile_index)
                    .map_err(|source| GlobalTileError::Image { id, source })?
                    .is_none()
                {
                    let tile = self
                        .tiles
                        .reserve_for_format(image.format())
                        .map_err(|source| GlobalTileError::Tile { id, source })?;
                    if let Err(error) = image.replace_tile(tile_index, tile) {
                        let (source, tile) = error.into_parts();
                        self.tiles.release(tile);
                        return Err(GlobalTileError::Image { id, source });
                    }
                }

                let tile = image
                    .tile_mut(tile_index)
                    .map_err(|source| GlobalTileError::Image { id, source })?
                    .expect("global cache tile was materialized before write");
                self.tiles
                    .write_pos_with_zero_init(tile, init_zero)
                    .map_err(|source| GlobalTileError::Tile { id, source })
            }
        }
    }

    fn validate_edit(
        &self,
        id: ImageId,
        edit: &ImageEdit,
    ) -> Result<GlobalEditTarget, GlobalEditError> {
        match self.image(id).ok_or(GlobalEditError::MissingImage { id })? {
            GlobalImage::Primitive(image) => {
                validate_edit_bounds(id, image.tile_count(), edit)?;
                Ok(GlobalEditTarget::Primitive)
            }
            GlobalImage::Derived { image, .. } => {
                validate_edit_bounds(id, image.tile_count(), edit)?;
                Ok(GlobalEditTarget::Derived)
            }
        }
    }

    pub fn validate_primitive_edits(
        &self,
        edits: &HashMap<ImageId, ImageEdit>,
    ) -> Result<(), GlobalEditError> {
        for (id, edit) in edits {
            let target = self.validate_edit(*id, edit)?;
            if target != GlobalEditTarget::Primitive {
                return Err(GlobalEditError::DestinationNotWritable { id: *id });
            }
        }
        Ok(())
    }

    pub fn apply_session_edits(
        &mut self,
        edits: HashMap<ImageId, ImageEdit>,
    ) -> Result<HashMap<ImageId, ImageEdit>, GlobalEditApplyError> {
        let targets = match self.validate_session_edit_targets(&edits) {
            Ok(targets) => targets,
            Err(kind) => return Err(GlobalEditApplyError::new(kind, edits)),
        };

        let mut inverse = HashMap::new();
        for (id, edit) in edits {
            match targets[&id] {
                GlobalEditTarget::Primitive => {
                    let image = self
                        .images
                        .get_mut(&id)
                        .expect("primitive edit target was validated");
                    let GlobalImage::Primitive(image) = image else {
                        panic!("primitive edit target changed role after validation");
                    };
                    let old = apply_dense_edit(image, edit);
                    if !old.is_empty() {
                        inverse.insert(id, old);
                    }
                }
                GlobalEditTarget::Derived => {
                    let image = self
                        .images
                        .get_mut(&id)
                        .expect("derived edit target was validated");
                    let GlobalImage::Derived { image, .. } = image else {
                        panic!("derived edit target changed role after validation");
                    };
                    let old = apply_cache_edit(image, edit);
                    self.release_image_edit(old);
                }
            }
        }

        Ok(inverse)
    }

    fn validate_session_edit_targets(
        &self,
        edits: &HashMap<ImageId, ImageEdit>,
    ) -> Result<HashMap<ImageId, GlobalEditTarget>, GlobalEditError> {
        let mut targets = HashMap::new();
        for (id, edit) in edits {
            targets.insert(*id, self.validate_edit(*id, edit)?);
        }
        Ok(targets)
    }

    pub fn into_parts(
        self,
    ) -> (
        DocumentVersionId,
        Option<ImageId>,
        HashMap<ImageId, GlobalImage>,
        Tiles,
    ) {
        (self.version, self.root, self.images, self.tiles)
    }

    pub fn apply_registry_patch(&mut self, patch: RegistryPatch) -> Result<(), GlobalStorageError> {
        let mut specs = self.image_specs();
        let mut root = self.root;

        for op in patch.ops {
            match op {
                RegistryPatchOp::NewImage {
                    id,
                    format,
                    layout,
                    role,
                } => {
                    if specs.contains_key(&id) {
                        return Err(GlobalStorageError::DuplicateImage { id });
                    }
                    specs.insert(
                        id,
                        ImageSpec {
                            format,
                            layout,
                            role,
                        },
                    );
                }
                RegistryPatchOp::SetPrimitive(id) => {
                    let spec = specs
                        .get_mut(&id)
                        .ok_or(GlobalStorageError::MissingImage { id })?;
                    spec.role = ImageRole::Primitive;
                }
                RegistryPatchOp::SetDerived { id, command } => {
                    let spec = specs
                        .get_mut(&id)
                        .ok_or(GlobalStorageError::MissingImage { id })?;
                    spec.role = ImageRole::Derived(command);
                }
                RegistryPatchOp::SetRoot(id) => {
                    if !specs.contains_key(&id) {
                        return Err(GlobalStorageError::MissingImage { id });
                    }
                    root = Some(id);
                }
            }
        }

        validate_specs(&specs)?;
        let replacements = self.stage_replacements(&specs)?;
        let changed_images = replacements
            .iter()
            .map(|(id, _)| *id)
            .collect::<HashSet<_>>();
        let changed_root = root != self.root;

        for (id, image) in replacements {
            if let Some(old) = self.images.insert(id, image) {
                old.release_tiles(&mut self.tiles);
            }
        }
        self.invalidate_downstream_caches(&changed_images);
        self.root = root;
        if changed_root || !changed_images.is_empty() {
            self.bump_version();
        }
        Ok(())
    }

    fn image_specs(&self) -> HashMap<ImageId, ImageSpec> {
        self.images
            .iter()
            .map(|(id, image)| {
                (
                    *id,
                    ImageSpec {
                        format: image.format(),
                        layout: image.layout(),
                        role: image.role(),
                    },
                )
            })
            .collect()
    }

    fn stage_replacements(
        &mut self,
        specs: &HashMap<ImageId, ImageSpec>,
    ) -> Result<Vec<(ImageId, GlobalImage)>, GlobalStorageError> {
        let mut replacements = Vec::new();

        for (id, spec) in specs {
            let current = self.images.get(id).map(|image| image.role());
            if current.as_ref() == Some(&spec.role) {
                continue;
            }

            match allocate_image(&mut self.tiles, *id, spec) {
                Ok(image) => replacements.push((*id, image)),
                Err(error) => {
                    release_replacements(&mut self.tiles, replacements);
                    return Err(error);
                }
            }
        }

        Ok(replacements)
    }

    fn invalidate_downstream_caches(&mut self, changed_images: &HashSet<ImageId>) {
        let mut frontier = changed_images.iter().copied().collect::<Vec<_>>();
        let mut invalidated = HashSet::new();

        while let Some(src) = frontier.pop() {
            let dependents = self
                .images
                .iter()
                .filter_map(|(id, image)| {
                    let GlobalImage::Derived { command, .. } = image else {
                        return None;
                    };
                    command
                        .reads
                        .iter()
                        .any(|read| read.image == src)
                        .then_some(*id)
                })
                .collect::<Vec<_>>();

            for id in dependents {
                if invalidated.insert(id) {
                    self.invalidate_cache(id);
                    frontier.push(id);
                }
            }
        }
    }

    fn invalidate_cache(&mut self, id: ImageId) {
        let Some(GlobalImage::Derived { image, .. }) = self.images.get_mut(&id) else {
            return;
        };
        for tile_index in 0..image.tile_count() {
            let tile = image
                .take_tile(tile_index)
                .expect("cache tile index must be in bounds during full invalidation");
            self.tiles.release_optional(tile);
        }
    }
}

fn allocate_image(
    tiles: &mut Tiles,
    id: ImageId,
    spec: &ImageSpec,
) -> Result<GlobalImage, GlobalStorageError> {
    match &spec.role {
        ImageRole::Primitive => DenseImage::allocate(spec.format, spec.layout, tiles)
            .map(GlobalImage::Primitive)
            .map_err(|source| GlobalStorageError::ImageCreate { id, source }),
        ImageRole::Derived(command) => CacheImage::new_invalid(spec.format, spec.layout)
            .map(|image| GlobalImage::Derived {
                image,
                command: command.clone(),
            })
            .map_err(|source| GlobalStorageError::ImageCreate { id, source }),
    }
}

fn release_replacements(tiles: &mut Tiles, replacements: Vec<(ImageId, GlobalImage)>) {
    for (_, image) in replacements {
        image.release_tiles(tiles);
    }
}

fn validate_edit_bounds(
    id: ImageId,
    tile_count: u32,
    edit: &ImageEdit,
) -> Result<(), GlobalEditError> {
    for (tile_index, _) in edit.edits() {
        if *tile_index >= tile_count {
            return Err(GlobalEditError::InvalidEditTile {
                id,
                tile_index: *tile_index,
            });
        }
    }
    Ok(())
}

fn apply_dense_edit(image: &mut DenseImage, edit: ImageEdit) -> ImageEdit {
    let mut inverse = Vec::with_capacity(edit.edits().len());
    for (tile_index, new_tile) in edit.into_edits() {
        let old_tile = image
            .replace_tile(tile_index, new_tile)
            .expect("dense edit tile index was validated before apply");
        inverse.push((tile_index, old_tile));
    }
    ImageEdit::from_sorted_unique(inverse).expect("dense inverse edit indices preserve order")
}

fn apply_cache_edit(image: &mut CacheImage, edit: ImageEdit) -> ImageEdit {
    let mut replaced = Vec::new();
    for (tile_index, new_tile) in edit.into_edits() {
        if let Some(old_tile) = image
            .replace_tile(tile_index, new_tile)
            .expect("cache edit tile index was validated before apply")
        {
            replaced.push((tile_index, old_tile));
        }
    }
    ImageEdit::from_sorted_unique(replaced).expect("cache inverse edit indices preserve order")
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas::{AtlasLayout, NoAtlasTextures};
    use gla_color::{ChannelCount, ChannelType, GlaFormat};
    use gla_image::ImageError;
    use gla_ir::{GraphRead, RegistryPatch, RegistryPatchOp};
    use std::collections::HashMap;
    use tile_key::{TileReadRef, TilesError};

    fn format() -> GlaFormat {
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

    fn new_storage_with_format(format: GlaFormat) -> GlobalStorage {
        let mut tiles = Tiles::new();
        let mut textures = NoAtlasTextures;
        tiles
            .new_atlas(AtlasLayout::TINY8, format, &mut textures)
            .unwrap();
        GlobalStorage::new(tiles)
    }

    #[test]
    fn new_primitive_image_allocates_dense_zero_tiles_by_format() {
        let id = ImageId::new(1);
        let mut storage = new_storage_with_format(format());

        storage
            .apply_registry_patch(RegistryPatch::new(vec![RegistryPatchOp::NewImage {
                id,
                format: format(),
                layout: layout(),
                role: ImageRole::Primitive,
            }]))
            .unwrap();

        let image = storage.image(id).unwrap().as_dense().unwrap();
        assert_eq!(
            storage.tiles().read_ref(image.tile(0).unwrap()).unwrap(),
            TileReadRef::Zero
        );
    }

    #[test]
    fn registry_patch_bumps_version_only_for_effective_changes() {
        let id = ImageId::new(1);
        let mut storage = new_storage_with_format(format());

        storage
            .apply_registry_patch(RegistryPatch::new(vec![RegistryPatchOp::NewImage {
                id,
                format: format(),
                layout: layout(),
                role: ImageRole::Primitive,
            }]))
            .unwrap();
        assert_eq!(storage.version(), DocumentVersionId::new(1));

        storage
            .apply_registry_patch(RegistryPatch::new(vec![RegistryPatchOp::SetRoot(id)]))
            .unwrap();
        assert_eq!(storage.version(), DocumentVersionId::new(2));

        storage
            .apply_registry_patch(RegistryPatch::new(vec![RegistryPatchOp::SetRoot(id)]))
            .unwrap();
        assert_eq!(storage.version(), DocumentVersionId::new(2));
    }

    #[test]
    fn registry_patch_invalidates_downstream_derived_caches() {
        let base = ImageId::new(1);
        let group = ImageId::new(2);
        let root = ImageId::new(3);
        let mut storage = new_storage_with_format(format());

        storage
            .apply_registry_patch(RegistryPatch::new(vec![
                RegistryPatchOp::NewImage {
                    id: base,
                    format: format(),
                    layout: layout(),
                    role: ImageRole::Primitive,
                },
                RegistryPatchOp::NewImage {
                    id: group,
                    format: format(),
                    layout: layout(),
                    role: ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(base)])),
                },
                RegistryPatchOp::NewImage {
                    id: root,
                    format: format(),
                    layout: layout(),
                    role: ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(group)])),
                },
            ]))
            .unwrap();

        let atlas_id = storage.tiles().atlas_for_format(format()).unwrap();
        storage.write_global_cache_pos(group, 0).unwrap();
        storage.write_global_cache_pos(root, 0).unwrap();
        assert_eq!(storage.tiles().atlas(atlas_id).unwrap().remaining(), 254);

        storage
            .apply_registry_patch(RegistryPatch::new(vec![RegistryPatchOp::SetDerived {
                id: group,
                command: GraphCommand::new(Vec::new()),
            }]))
            .unwrap();

        assert_eq!(storage.version(), DocumentVersionId::new(2));
        assert_eq!(storage.tiles().atlas(atlas_id).unwrap().remaining(), 256);
        assert!(matches!(
            storage.read_global_ref(group, 0).unwrap_err(),
            GlobalTileError::MissingMaterializedTile { id } if id == group
        ));
        assert!(matches!(
            storage.read_global_ref(root, 0).unwrap_err(),
            GlobalTileError::MissingMaterializedTile { id } if id == root
        ));
    }

    #[test]
    fn missing_atlas_keeps_patch_atomic() {
        let first = ImageId::new(1);
        let second = ImageId::new(2);
        let mut storage = new_storage_with_format(format());

        let err = storage
            .apply_registry_patch(RegistryPatch::new(vec![
                RegistryPatchOp::NewImage {
                    id: first,
                    format: format(),
                    layout: layout(),
                    role: ImageRole::Primitive,
                },
                RegistryPatchOp::NewImage {
                    id: second,
                    format: value_format(),
                    layout: layout(),
                    role: ImageRole::Primitive,
                },
            ]))
            .unwrap_err();

        assert!(matches!(
            err,
            GlobalStorageError::ImageCreate {
                id,
                source: ImageError::TileAllocFailed {
                    source: TilesError::MissingAtlasForFormat { .. }
                }
            } if id == second
        ));
        assert!(storage.image(first).is_none());
        assert!(storage.image(second).is_none());
    }

    #[test]
    fn derived_graph_rejects_missing_reads() {
        let derived = ImageId::new(1);
        let missing = ImageId::new(2);
        let mut storage = new_storage_with_format(format());

        let err = storage
            .apply_registry_patch(RegistryPatch::new(vec![RegistryPatchOp::NewImage {
                id: derived,
                format: format(),
                layout: layout(),
                role: ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(missing)])),
            }]))
            .unwrap_err();

        assert!(matches!(err, GlobalStorageError::MissingImage { id } if id == missing));
        assert!(storage.image(derived).is_none());
    }

    #[test]
    fn derived_graph_rejects_cycles() {
        let a = ImageId::new(1);
        let b = ImageId::new(2);
        let mut storage = new_storage_with_format(format());

        let err = storage
            .apply_registry_patch(RegistryPatch::new(vec![
                RegistryPatchOp::NewImage {
                    id: a,
                    format: format(),
                    layout: layout(),
                    role: ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(b)])),
                },
                RegistryPatchOp::NewImage {
                    id: b,
                    format: format(),
                    layout: layout(),
                    role: ImageRole::Derived(GraphCommand::new(vec![GraphRead::current(a)])),
                },
            ]))
            .unwrap_err();

        assert!(matches!(err, GlobalStorageError::RegistryCycle { .. }));
        assert!(storage.images().is_empty());
    }

    #[test]
    fn set_root_is_validated_but_not_required_for_storage() {
        let id = ImageId::new(1);
        let mut storage = new_storage_with_format(format());

        storage
            .apply_registry_patch(RegistryPatch::new(vec![
                RegistryPatchOp::NewImage {
                    id,
                    format: format(),
                    layout: layout(),
                    role: ImageRole::Primitive,
                },
                RegistryPatchOp::SetRoot(id),
            ]))
            .unwrap();

        assert_eq!(storage.root(), Some(id));
    }

    #[test]
    fn session_edit_validation_error_returns_owned_tiles() {
        let id = ImageId::new(1);
        let mut storage = new_storage_with_format(format());
        storage
            .apply_registry_patch(RegistryPatch::new(vec![RegistryPatchOp::NewImage {
                id,
                format: format(),
                layout: layout(),
                role: ImageRole::Primitive,
            }]))
            .unwrap();

        let atlas_id = storage.tiles().atlas_for_format(format()).unwrap();
        let mut tile = storage.reserve_tile_for_format(format()).unwrap();
        storage.write_tile_pos(&mut tile).unwrap();
        assert_eq!(storage.tiles().atlas(atlas_id).unwrap().remaining(), 255);

        let edit = ImageEdit::from_sorted_unique(vec![(99, tile)]).unwrap();
        let mut edits = HashMap::new();
        edits.insert(id, edit);

        let err = storage.apply_session_edits(edits).unwrap_err();
        match err.kind() {
            GlobalEditError::InvalidEditTile {
                id: err_id,
                tile_index,
            } => {
                assert_eq!(*err_id, id);
                assert_eq!(*tile_index, 99);
            }
            other => panic!("unexpected edit error: {other:?}"),
        }

        let (_, edits) = err.into_parts();
        for (_, edit) in edits {
            edit.release_tiles(storage.tiles_mut());
        }
        assert_eq!(storage.tiles().atlas(atlas_id).unwrap().remaining(), 256);
    }
}
