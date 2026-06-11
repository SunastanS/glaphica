mod local;

pub use local::{ImageEdit, ImageEditCreateError};

use atlas::TilePos;
use gla_color::GlaFormat;
pub use gla_core::CanvasInput;
use gla_image::{CacheImage, DenseImage, GlaImageLayout, ImageError};
use gla_ir::{DocumentVersionId, GraphCommand, ImageId, ImageRole, RegistryPatch, RegistryPatchOp};
use gla_renderer::Renderer;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
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

#[derive(Debug)]
pub enum GlobalStorageError {
    DuplicateImage { id: ImageId },
    MissingImage { id: ImageId },
    RegistryCommandReadsDestination { dst: ImageId },
    RegistryCycle { id: ImageId },
    ImageCreate { id: ImageId, source: ImageError },
}

#[derive(Debug)]
pub enum GlobalTileError {
    MissingImage { id: ImageId },
    MissingMaterializedTile { id: ImageId },
    GlobalPrimitiveWrite { id: ImageId },
    Image { id: ImageId, source: ImageError },
    Tile { id: ImageId, source: TilesError },
}

impl Display for GlobalTileError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingImage { id } => write!(f, "global image {id:?} is not declared"),
            Self::MissingMaterializedTile { id } => {
                write!(f, "global image {id:?} has no materialized cache tile")
            }
            Self::GlobalPrimitiveWrite { id } => {
                write!(
                    f,
                    "global primitive image {id:?} cannot be written by render"
                )
            }
            Self::Image { id, source } => write!(f, "global image {id:?} access failed: {source}"),
            Self::Tile { id, source } => {
                write!(f, "global tile access for {id:?} failed: {source}")
            }
        }
    }
}

impl Error for GlobalTileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Image { source, .. } => Some(source),
            Self::Tile { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlobalEditTarget {
    Primitive,
    Derived,
}

#[derive(Debug)]
pub enum GlobalEditError {
    MissingImage { id: ImageId },
    DestinationNotWritable { id: ImageId },
    InvalidEditTile { id: ImageId, tile_index: u32 },
}

impl Display for GlobalEditError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingImage { id } => write!(f, "global image {id:?} is not declared"),
            Self::DestinationNotWritable { id } => {
                write!(f, "global image {id:?} is not a writable edit target")
            }
            Self::InvalidEditTile { id, tile_index } => {
                write!(
                    f,
                    "edit tile {tile_index} is invalid for global image {id:?}"
                )
            }
        }
    }
}

impl Error for GlobalEditError {}

#[derive(Debug)]
pub struct GlobalEditApplyError {
    kind: GlobalEditError,
    edits: HashMap<ImageId, ImageEdit>,
}

impl GlobalEditApplyError {
    fn new(kind: GlobalEditError, edits: HashMap<ImageId, ImageEdit>) -> Self {
        Self { kind, edits }
    }

    pub fn kind(&self) -> &GlobalEditError {
        &self.kind
    }

    pub fn into_parts(self) -> (GlobalEditError, HashMap<ImageId, ImageEdit>) {
        (self.kind, self.edits)
    }
}

impl Display for GlobalEditApplyError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.kind.fmt(f)
    }
}

impl Error for GlobalEditApplyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.kind)
    }
}

impl Display for GlobalStorageError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateImage { id } => write!(f, "global image {id:?} already exists"),
            Self::MissingImage { id } => write!(f, "global image {id:?} is not declared"),
            Self::RegistryCommandReadsDestination { dst } => {
                write!(f, "registry command for {dst:?} reads its destination")
            }
            Self::RegistryCycle { id } => {
                write!(f, "global image graph has a dependency cycle at {id:?}")
            }
            Self::ImageCreate { id, source } => {
                write!(f, "failed to create global image {id:?}: {source}")
            }
        }
    }
}

impl Error for GlobalStorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ImageCreate { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub struct GlobalStorage {
    version: DocumentVersionId,
    root: Option<ImageId>,
    images: HashMap<ImageId, GlobalImage>,
    tiles: Tiles,
    renderer: Renderer,
}

impl GlobalStorage {
    pub fn new(tiles: Tiles, renderer: Renderer) -> Self {
        Self {
            version: DocumentVersionId::default(),
            root: None,
            images: HashMap::new(),
            tiles,
            renderer,
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

    pub fn renderer(&self) -> &Renderer {
        &self.renderer
    }

    pub fn renderer_mut(&mut self) -> &mut Renderer {
        &mut self.renderer
    }

    pub fn read_tile_ref(&self, tile: &Tile) -> Result<TileReadRef, TilesError> {
        self.tiles.read_ref(tile)
    }

    pub fn write_tile_pos(&mut self, tile: &mut Tile) -> Result<TilePos, TilesError> {
        self.tiles.write_pos(tile)
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
                    .write_pos(tile)
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

    pub fn apply_primitive_edits(
        &mut self,
        edits: HashMap<ImageId, ImageEdit>,
    ) -> HashMap<ImageId, ImageEdit> {
        let mut inverse = HashMap::new();
        for (id, edit) in edits {
            let image = self
                .images
                .get_mut(&id)
                .expect("primitive edit patch was validated against global storage");
            let GlobalImage::Primitive(image) = image else {
                panic!("primitive edit patch changed role after validation");
            };
            let old = apply_dense_edit(image, edit);
            if !old.is_empty() {
                inverse.insert(id, old);
            }
        }
        inverse
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

    pub(crate) fn resources_mut(
        &mut self,
    ) -> (
        &mut HashMap<ImageId, GlobalImage>,
        &mut Tiles,
        &mut Renderer,
    ) {
        (&mut self.images, &mut self.tiles, &mut self.renderer)
    }

    pub fn into_parts(
        self,
    ) -> (
        DocumentVersionId,
        Option<ImageId>,
        HashMap<ImageId, GlobalImage>,
        Tiles,
        Renderer,
    ) {
        (
            self.version,
            self.root,
            self.images,
            self.tiles,
            self.renderer,
        )
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

        for (id, image) in replacements {
            if let Some(old) = self.images.insert(id, image) {
                old.release_tiles(&mut self.tiles);
            }
        }
        self.root = root;
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
}

#[derive(Clone, Debug, PartialEq)]
struct ImageSpec {
    format: GlaFormat,
    layout: GlaImageLayout,
    role: ImageRole,
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

fn validate_specs(specs: &HashMap<ImageId, ImageSpec>) -> Result<(), GlobalStorageError> {
    for (id, spec) in specs {
        let ImageRole::Derived(command) = &spec.role else {
            continue;
        };
        for read in &command.reads {
            if read.image == *id {
                return Err(GlobalStorageError::RegistryCommandReadsDestination { dst: *id });
            }
            if !specs.contains_key(&read.image) {
                return Err(GlobalStorageError::MissingImage { id: read.image });
            }
        }
    }

    let mut visited = HashSet::new();
    let mut visiting = HashSet::new();
    for id in specs.keys().copied() {
        visit_spec(id, specs, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn visit_spec(
    id: ImageId,
    specs: &HashMap<ImageId, ImageSpec>,
    visiting: &mut HashSet<ImageId>,
    visited: &mut HashSet<ImageId>,
) -> Result<(), GlobalStorageError> {
    if visited.contains(&id) {
        return Ok(());
    }
    if !visiting.insert(id) {
        return Err(GlobalStorageError::RegistryCycle { id });
    }

    if let Some(ImageSpec {
        role: ImageRole::Derived(command),
        ..
    }) = specs.get(&id)
    {
        for read in &command.reads {
            visit_spec(read.image, specs, visiting, visited)?;
        }
    }

    visiting.remove(&id);
    visited.insert(id);
    Ok(())
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
    use gla_ir::GraphRead;
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
        GlobalStorage::new(tiles, Renderer::new())
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
