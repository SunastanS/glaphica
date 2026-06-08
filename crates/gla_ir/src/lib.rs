use gla_color::GlaFormat;
pub use gla_command_core::{Affine2D, FootprintModifier, Mapping, Tool, ToolParams};
use gla_image::GlaImageLayout;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ImageId(u64);

impl ImageId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct DocumentVersionId(u64);

impl DocumentVersionId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GraphRead {
    pub image: ImageId,
    pub mapping: Mapping,
    pub modifier: FootprintModifier,
}

impl GraphRead {
    pub fn current(image: ImageId) -> Self {
        Self {
            image,
            mapping: Mapping::Identity,
            modifier: FootprintModifier::None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GraphCommand {
    pub reads: Vec<GraphRead>,
}

impl GraphCommand {
    pub fn new(reads: Vec<GraphRead>) -> Self {
        Self { reads }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionReadImage {
    Current(ImageId),
    Backup(ImageId),
}

impl SessionReadImage {
    pub const fn id(self) -> ImageId {
        match self {
            Self::Current(id) | Self::Backup(id) => id,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionRead {
    pub image: SessionReadImage,
    pub mapping: Mapping,
    pub modifier: FootprintModifier,
}

impl SessionRead {
    pub fn current(image: ImageId) -> Self {
        Self {
            image: SessionReadImage::Current(image),
            mapping: Mapping::Identity,
            modifier: FootprintModifier::None,
        }
    }

    pub fn backup(image: ImageId) -> Self {
        Self {
            image: SessionReadImage::Backup(image),
            mapping: Mapping::Identity,
            modifier: FootprintModifier::None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionCommand {
    pub reads: Vec<SessionRead>,
}

impl SessionCommand {
    pub fn new(reads: Vec<SessionRead>) -> Self {
        Self { reads }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ImageRole {
    Primitive,
    Derived(GraphCommand),
}

impl ImageRole {
    pub fn is_primitive(&self) -> bool {
        matches!(self, Self::Primitive)
    }

    pub fn is_derived(&self) -> bool {
        matches!(self, Self::Derived(_))
    }

    pub fn graph_command(&self) -> Option<&GraphCommand> {
        match self {
            Self::Primitive => None,
            Self::Derived(command) => Some(command),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageDeclaration {
    pub role: ImageRole,
    pub format: GlaFormat,
    pub layout: GlaImageLayout,
}

impl ImageDeclaration {
    pub fn primitive(format: GlaFormat, layout: GlaImageLayout) -> Self {
        Self {
            role: ImageRole::Primitive,
            format,
            layout,
        }
    }

    pub fn derived(format: GlaFormat, layout: GlaImageLayout, command: GraphCommand) -> Self {
        Self {
            role: ImageRole::Derived(command),
            format,
            layout,
        }
    }

    pub fn role(&self) -> &ImageRole {
        &self.role
    }

    pub fn format(&self) -> GlaFormat {
        self.format
    }

    pub fn layout(&self) -> GlaImageLayout {
        self.layout
    }

    pub fn is_primitive(&self) -> bool {
        self.role.is_primitive()
    }

    pub fn is_derived(&self) -> bool {
        self.role.is_derived()
    }

    pub fn graph_command(&self) -> Option<&GraphCommand> {
        self.role.graph_command()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DrawOnCommand {
    pub dst: ImageId,
    pub input_mapping: Mapping,
    pub tool: Tool,
    pub tool_params: ToolParams,
}

impl DrawOnCommand {
    pub fn new(dst: ImageId) -> Self {
        Self {
            dst,
            input_mapping: Mapping::Identity,
            tool: Tool::default(),
            tool_params: ToolParams::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeriveCommand {
    pub dst: ImageId,
    pub command: SessionCommand,
}

impl DeriveCommand {
    pub fn new(reads: Vec<SessionRead>, dst: ImageId) -> Self {
        Self {
            dst,
            command: SessionCommand::new(reads),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentImageAccess {
    Read,
    ReadWrite,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocImageUse {
    pub id: ImageId,
    pub access: DocumentImageAccess,
}

impl DocImageUse {
    pub fn read(id: ImageId) -> Self {
        Self {
            id,
            access: DocumentImageAccess::Read,
        }
    }

    pub fn read_write(id: ImageId) -> Self {
        Self {
            id,
            access: DocumentImageAccess::ReadWrite,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetadataRef<T> {
    Concrete(T),
    Like(ImageId),
}

impl<T> From<T> for MetadataRef<T> {
    fn from(value: T) -> Self {
        Self::Concrete(value)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SessionImageDecl {
    Primitive {
        id: ImageId,
        format: MetadataRef<GlaFormat>,
        layout: MetadataRef<GlaImageLayout>,
    },
    Derived {
        id: ImageId,
        format: MetadataRef<GlaFormat>,
        layout: MetadataRef<GlaImageLayout>,
        command: SessionCommand,
    },
}

impl SessionImageDecl {
    pub fn id(&self) -> ImageId {
        match self {
            Self::Primitive { id, .. } | Self::Derived { id, .. } => *id,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DrawSessionIR {
    pub expected_document_version: DocumentVersionId,
    pub doc_images: Vec<DocImageUse>,
    pub session_images: Vec<SessionImageDecl>,
    pub draw_on: Vec<DrawOnCommand>,
    pub derive: Vec<DeriveCommand>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RegistryPatch {
    pub ops: Vec<RegistryPatchOp>,
}

impl RegistryPatch {
    pub fn new(ops: Vec<RegistryPatchOp>) -> Self {
        Self { ops }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RegistryPatchOp {
    NewImage {
        id: ImageId,
        format: GlaFormat,
        layout: GlaImageLayout,
        role: ImageRole,
    },
    InsertImage {
        id: ImageId,
        key: gla_image::GlaImageKey,
        role: ImageRole,
        format: GlaFormat,
        layout: GlaImageLayout,
    },
    SetPrimitive(ImageId),
    SetDerived {
        id: ImageId,
        command: GraphCommand,
    },
    SetRoot(ImageId),
}
