use gla_color::GlaFormat;
pub use gla_command_core::{Affine2D, FootprintModifier, Mapping};
use serde::{Deserialize, Serialize};

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub enum DrawOnToolKind {
    #[default]
    RadialKernel1D,
    ReplaceCircle4D,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphCommand {
    pub reads: Vec<GraphRead>,
}

impl GraphCommand {
    pub fn new(reads: Vec<GraphRead>) -> Self {
        Self { reads }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionCommand {
    pub reads: Vec<SessionRead>,
}

impl SessionCommand {
    pub fn new(reads: Vec<SessionRead>) -> Self {
        Self { reads }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DrawOnCommand {
    pub dst: ImageId,
    pub tool: DrawOnToolKind,
}

impl DrawOnCommand {
    pub fn new(dst: ImageId) -> Self {
        Self {
            dst,
            tool: DrawOnToolKind::default(),
        }
    }

    pub fn with_tool(dst: ImageId, tool: DrawOnToolKind) -> Self {
        Self { dst, tool }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocumentImageAccess {
    Read,
    ReadWrite,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MetadataRef<T> {
    Concrete(T),
    Like(ImageId),
}

impl<T> From<T> for MetadataRef<T> {
    fn from(value: T) -> Self {
        Self::Concrete(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ImageLayoutSpec {
    pub width_px: u32,
    pub height_px: u32,
}

impl ImageLayoutSpec {
    pub const fn new(width_px: u32, height_px: u32) -> Self {
        Self {
            width_px,
            height_px,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SessionImageDecl {
    Primitive {
        id: ImageId,
        format: MetadataRef<GlaFormat>,
        layout: MetadataRef<ImageLayoutSpec>,
    },
    Derived {
        id: ImageId,
        format: MetadataRef<GlaFormat>,
        layout: MetadataRef<ImageLayoutSpec>,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DrawSessionIR {
    pub expected_document_version: DocumentVersionId,
    pub doc_images: Vec<DocImageUse>,
    pub session_images: Vec<SessionImageDecl>,
    pub draw_on: Vec<DrawOnCommand>,
    pub derive: Vec<DeriveCommand>,
}

impl DrawSessionIR {
    pub fn required_draw_on_tools(&self) -> std::collections::BTreeSet<DrawOnToolKind> {
        self.draw_on.iter().map(|command| command.tool).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegistryPatch {
    pub ops: Vec<RegistryPatchOp>,
}

impl RegistryPatch {
    pub fn new(ops: Vec<RegistryPatchOp>) -> Self {
        Self { ops }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RegistryPatchOp {
    NewImage {
        id: ImageId,
        format: GlaFormat,
        layout: ImageLayoutSpec,
        role: ImageRole,
    },
    SetPrimitive(ImageId),
    SetDerived {
        id: ImageId,
        command: GraphCommand,
    },
    SetRoot(ImageId),
}

pub fn draw_session_ir_from_json_str(source: &str) -> Result<DrawSessionIR, serde_json::Error> {
    serde_json::from_str(source)
}

pub fn draw_session_ir_to_json_string_pretty(
    ir: &DrawSessionIR,
) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(ir)
}

pub fn registry_patch_from_json_str(source: &str) -> Result<RegistryPatch, serde_json::Error> {
    serde_json::from_str(source)
}

pub fn registry_patch_to_json_string_pretty(
    patch: &RegistryPatch,
) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(patch)
}
