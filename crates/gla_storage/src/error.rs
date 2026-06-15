use crate::ImageEdit;
use gla_image::ImageError;
use gla_ir::ImageId;
use std::collections::HashMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use tile_key::TilesError;

#[derive(Debug)]
pub enum GlobalStorageError {
    DuplicateImage { id: ImageId },
    MissingImage { id: ImageId },
    CannotDeleteRoot { id: ImageId },
    ImageInUse { id: ImageId, dependent: ImageId },
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
    pub(crate) fn new(kind: GlobalEditError, edits: HashMap<ImageId, ImageEdit>) -> Self {
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
            Self::CannotDeleteRoot { id } => write!(f, "cannot delete root image {id:?}"),
            Self::ImageInUse { id, dependent } => {
                write!(f, "global image {id:?} is still read by {dependent:?}")
            }
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
