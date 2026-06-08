use gla_image::{GlaImageKey, GlaImageLayout};
use gla_ir::{ImageId, SessionCommand};
use std::collections::HashMap;

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
    pub fn primitive(format: gla_color::GlaFormat, layout: GlaImageLayout) -> Self {
        Self::Primitive { format, layout }
    }

    pub fn derived(
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

    pub fn format(&self) -> gla_color::GlaFormat {
        match self {
            Self::Primitive { format, .. } | Self::Derived { format, .. } => *format,
        }
    }

    pub fn layout(&self) -> GlaImageLayout {
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

#[derive(Clone, Debug, Default)]
pub struct LocalImageTable {
    images: HashMap<ImageId, LocalImage>,
}

impl LocalImageTable {
    pub fn new(images: HashMap<ImageId, LocalImage>) -> Self {
        Self { images }
    }

    pub fn empty() -> Self {
        Self {
            images: HashMap::new(),
        }
    }

    pub fn as_map(&self) -> &HashMap<ImageId, LocalImage> {
        &self.images
    }

    pub fn get(&self, id: ImageId) -> Option<&LocalImage> {
        self.images.get(&id)
    }

    pub fn key(&self, id: ImageId) -> Option<GlaImageKey> {
        self.get(id).map(|local| local.key)
    }

    pub fn insert(&mut self, id: ImageId, image: LocalImage) -> Option<LocalImage> {
        self.images.insert(id, image)
    }

    pub fn contains(&self, id: ImageId) -> bool {
        self.images.contains_key(&id)
    }

    pub fn contains_key(&self, id: &ImageId) -> bool {
        self.images.contains_key(id)
    }

    pub fn values(&self) -> impl Iterator<Item = &LocalImage> + '_ {
        self.images.values()
    }

    pub fn iter(&self) -> impl Iterator<Item = (ImageId, &LocalImage)> + '_ {
        self.images.iter().map(|(id, image)| (*id, image))
    }

    pub fn declarations(&self) -> HashMap<ImageId, LocalImageDeclaration> {
        self.iter()
            .map(|(id, local)| (id, local.declaration.clone()))
            .collect()
    }
}
