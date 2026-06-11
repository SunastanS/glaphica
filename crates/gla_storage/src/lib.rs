mod local;

pub use local::{
    DrawOnWriter, ImageEdit, ImageEditCreateError, LocalStorage, LocalStorageError, SessionImage,
    SessionImageContent, SessionImageId, SessionImageWriter,
};

use gla_color::GlaFormat;
use gla_image::{CacheImage, DenseImage, GlaImageLayout, ImageError};
use gla_ir::{GraphCommand, ImageId, ImageRole, RegistryPatch, RegistryPatchOp};
use gla_renderer::Renderer;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use tile_key::Tiles;

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
    root: Option<ImageId>,
    images: HashMap<ImageId, GlobalImage>,
    tiles: Tiles,
    renderer: Renderer,
}

impl GlobalStorage {
    pub fn new(tiles: Tiles, renderer: Renderer) -> Self {
        Self {
            root: None,
            images: HashMap::new(),
            tiles,
            renderer,
        }
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

    pub fn into_parts(
        self,
    ) -> (
        Option<ImageId>,
        HashMap<ImageId, GlobalImage>,
        Tiles,
        Renderer,
    ) {
        (self.root, self.images, self.tiles, self.renderer)
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

#[cfg(test)]
mod tests {
    use super::*;
    use atlas::{AtlasLayout, NoAtlasTextures};
    use gla_color::{ChannelCount, ChannelType, GlaFormat};
    use gla_ir::GraphRead;
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
}
