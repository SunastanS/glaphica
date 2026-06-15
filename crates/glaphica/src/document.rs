use std::convert::Infallible;
use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasLayout, AtlasTextureStore, NoAtlasTextures};
use gla_color::{ChannelCount, ChannelType, GlaFormat};
use gla_ir::{
    DocImageUse, DocumentVersionId, DrawOnCommand, DrawOnToolKind, DrawSessionIR, ImageId,
    ImageLayoutSpec, ImageRole, RegistryPatch, RegistryPatchOp,
};
use gla_session::{DrawSession, SessionError};
use gla_storage::{GlobalStorage, GlobalStorageError};
use tile_key::{NewAtlasError, Tiles};

pub const DEFAULT_CANVAS_WIDTH_PX: u32 = 1024;
pub const DEFAULT_CANVAS_HEIGHT_PX: u32 = 768;

pub struct DocumentWorkspace {
    storage: GlobalStorage,
    root: ImageId,
    format: GlaFormat,
    layout: ImageLayoutSpec,
}

impl DocumentWorkspace {
    pub fn default_blank() -> Result<Self, DocumentWorkspaceBuildError<Infallible>> {
        Self::blank(DEFAULT_CANVAS_WIDTH_PX, DEFAULT_CANVAS_HEIGHT_PX)
    }

    pub fn blank(
        width_px: u32,
        height_px: u32,
    ) -> Result<Self, DocumentWorkspaceBuildError<Infallible>> {
        let mut textures = NoAtlasTextures;
        Self::blank_with_textures(width_px, height_px, &mut textures)
    }

    pub fn blank_with_textures<S>(
        width_px: u32,
        height_px: u32,
        textures: &mut S,
    ) -> Result<Self, DocumentWorkspaceBuildError<S::Error>>
    where
        S: AtlasTextureStore,
    {
        let format = default_canvas_format();
        let layout = ImageLayoutSpec::new(width_px, height_px);
        let mut tiles = Tiles::new();
        tiles
            .new_atlas(AtlasLayout::LARGE17, format, textures)
            .map_err(DocumentWorkspaceBuildError::Atlas)?;

        let root = ImageId::new(1);
        let mut storage = GlobalStorage::new(tiles);
        storage
            .apply_registry_patch(RegistryPatch::new(vec![
                RegistryPatchOp::NewImage {
                    id: root,
                    format,
                    layout,
                    role: ImageRole::Primitive,
                },
                RegistryPatchOp::SetRoot(root),
            ]))
            .map_err(DocumentWorkspaceBuildError::Registry)?;

        Ok(Self {
            storage,
            root,
            format,
            layout,
        })
    }

    pub fn storage(&self) -> &GlobalStorage {
        &self.storage
    }

    pub fn storage_mut(&mut self) -> &mut GlobalStorage {
        &mut self.storage
    }

    pub fn root(&self) -> ImageId {
        self.root
    }

    pub fn version(&self) -> DocumentVersionId {
        self.storage.version()
    }

    pub fn format(&self) -> GlaFormat {
        self.format
    }

    pub fn layout(&self) -> ImageLayoutSpec {
        self.layout
    }

    pub fn canvas_size_px(&self) -> (u32, u32) {
        (self.layout.width_px, self.layout.height_px)
    }

    pub fn root_reader_ir(&self) -> DrawSessionIR {
        DrawSessionIR {
            expected_document_version: self.version(),
            doc_images: vec![DocImageUse::read(self.root)],
            session_images: Vec::new(),
            draw_on: Vec::new(),
            derive: Vec::new(),
        }
    }

    pub fn root_replace_circle_ir(&self) -> DrawSessionIR {
        DrawSessionIR {
            expected_document_version: self.version(),
            doc_images: vec![DocImageUse::read_write(self.root)],
            session_images: Vec::new(),
            draw_on: vec![DrawOnCommand::with_tool(
                self.root,
                DrawOnToolKind::ReplaceCircle4D,
            )],
            derive: Vec::new(),
        }
    }

    pub fn begin_session(&mut self, ir: &DrawSessionIR) -> Result<DrawSession<'_>, SessionError> {
        DrawSession::begin(ir, &mut self.storage)
    }
}

#[derive(Debug)]
pub enum DocumentWorkspaceBuildError<E> {
    Atlas(NewAtlasError<E>),
    Registry(GlobalStorageError),
}

pub type DocumentWorkspaceError = DocumentWorkspaceBuildError<Infallible>;

impl<E: Display> Display for DocumentWorkspaceBuildError<E> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Atlas(error) => write!(f, "failed to allocate document atlas: {error}"),
            Self::Registry(error) => write!(f, "failed to create document registry: {error}"),
        }
    }
}

impl<E> Error for DocumentWorkspaceBuildError<E>
where
    E: Error + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Atlas(error) => Some(error),
            Self::Registry(error) => Some(error),
        }
    }
}

fn default_canvas_format() -> GlaFormat {
    GlaFormat {
        channel_count: ChannelCount::D4,
        channel_type: ChannelType::F32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_workspace_creates_root_primitive_document() {
        let workspace = DocumentWorkspace::blank(320, 240).unwrap();
        let root = workspace.root();
        let image = workspace.storage().image(root).unwrap();

        assert_eq!(workspace.storage().root(), Some(root));
        assert_eq!(workspace.version(), DocumentVersionId::new(1));
        assert_eq!(workspace.canvas_size_px(), (320, 240));
        assert_eq!(workspace.format(), default_canvas_format());
        assert!(image.role().is_primitive());
        assert_eq!(image.layout().width_px(), 320);
        assert_eq!(image.layout().height_px(), 240);
    }

    #[test]
    fn root_reader_ir_begins_session_at_current_version() {
        let mut workspace = DocumentWorkspace::blank(320, 240).unwrap();
        let expected = workspace.version();
        let ir = workspace.root_reader_ir();
        let session = workspace.begin_session(&ir).unwrap();

        assert_eq!(session.expected_document_version(), expected);
        session.discard();
    }

    #[test]
    fn root_replace_circle_ir_reserves_direct_paint_session() {
        let mut workspace = DocumentWorkspace::blank(320, 240).unwrap();
        let ir = workspace.root_replace_circle_ir();

        assert!(
            ir.required_draw_on_tools()
                .contains(&DrawOnToolKind::ReplaceCircle4D)
        );
        let session = workspace.begin_session(&ir).unwrap();
        session.discard();
    }

    #[test]
    fn blank_workspace_can_use_injected_texture_store() {
        let mut textures = NoAtlasTextures;
        let workspace = DocumentWorkspace::blank_with_textures(128, 96, &mut textures).unwrap();

        assert_eq!(workspace.canvas_size_px(), (128, 96));
        assert_eq!(workspace.storage().root(), Some(workspace.root()));
    }

    #[test]
    fn blank_workspace_rejects_empty_layout() {
        let error = match DocumentWorkspace::blank(0, 240) {
            Ok(_) => panic!("empty layout should be rejected"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            DocumentWorkspaceError::Registry(GlobalStorageError::ImageCreate { .. })
        ));
    }
}
