use std::convert::Infallible;
use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasLayout, AtlasTextureStore, NoAtlasTextures};
use gla_color::{ChannelCount, ChannelType, GlaFormat, PremultipliedRgbaF32};
use gla_core::CanvasCoordF;
use gla_draw_on::DrawOnInput;
use gla_image::IMAGE_TILE_SIZE;
use gla_ir::{
    DocImageUse, DocumentVersionId, DrawOnCommand, DrawOnToolKind, DrawSessionIR, ImageId,
    ImageLayoutSpec, ImageRole, RegistryPatch, RegistryPatchOp,
};
use gla_renderer::{PresentTile, PresentTileParams, RenderBackend};
use gla_session::{DrawCommit, DrawHistory, DrawRecordId, DrawSession, SessionError};
use gla_storage::{GlobalStorage, GlobalStorageError, GlobalTileError};
use tile_key::{NewAtlasError, TileReadRef, Tiles};

use crate::AppView;

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

    pub fn replace_circle_on_root<B>(
        &mut self,
        history: &mut DrawHistory,
        backend: &mut B,
        center_x: f32,
        center_y: f32,
        radius_px: f32,
        color: PremultipliedRgbaF32,
    ) -> Result<Option<DrawCommit>, SessionError>
    where
        B: RenderBackend,
    {
        let root = self.root;
        let ir = self.root_replace_circle_ir();
        let mut session = self.begin_session(&ir)?;
        {
            let mut frame = session.begin_frame();
            frame.draw_on(
                root,
                DrawOnInput::replace_circle_4d(
                    center_x,
                    center_y,
                    radius_px.max(0.0),
                    radius_px.max(0.0),
                    color,
                ),
            )?;
            frame.flush(backend)?;
        }
        session.commit(history)
    }

    pub fn apply_draw_record<B>(
        &mut self,
        history: &mut DrawHistory,
        backend: &mut B,
        record_id: DrawRecordId,
    ) -> Result<DrawRecordId, SessionError>
    where
        B: RenderBackend,
    {
        history.apply_stored_patch(record_id, &mut self.storage, backend)
    }

    pub fn root_present_tiles(&self) -> Result<Vec<PresentTile>, DocumentPresentError> {
        self.root_present_tiles_for_view(&AppView::identity())
    }

    pub fn root_present_tiles_for_view(
        &self,
        view: &AppView,
    ) -> Result<Vec<PresentTile>, DocumentPresentError> {
        let image = self
            .storage
            .image(self.root)
            .ok_or(DocumentPresentError::MissingRoot { id: self.root })?;
        let layout = image.layout();
        let tile_count_x = layout.tile_count_x();
        let tile_count = layout.tile_count();
        let mut tiles = Vec::new();

        for tile_index in 0..tile_count {
            let tile_ref = self
                .storage
                .read_global_ref(self.root, tile_index)
                .map_err(DocumentPresentError::Tile)?;
            let TileReadRef::Physical(src) = tile_ref else {
                continue;
            };

            let tile_x = tile_index % tile_count_x;
            let tile_y = tile_index / tile_count_x;
            let origin_x = tile_x * IMAGE_TILE_SIZE;
            let origin_y = tile_y * IMAGE_TILE_SIZE;
            let source_width = self
                .layout
                .width_px
                .saturating_sub(origin_x)
                .min(IMAGE_TILE_SIZE);
            let source_height = self
                .layout
                .height_px
                .saturating_sub(origin_y)
                .min(IMAGE_TILE_SIZE);
            if source_width == 0 || source_height == 0 {
                continue;
            }
            let target_min =
                view.document_to_screen_point(CanvasCoordF::new(origin_x as f32, origin_y as f32));
            let target_max = view.document_to_screen_point(CanvasCoordF::new(
                (origin_x + source_width) as f32,
                (origin_y + source_height) as f32,
            ));

            tiles.push(PresentTile {
                src,
                params: PresentTileParams {
                    target_min_px: [target_min.x, target_min.y],
                    target_max_px: [target_max.x, target_max.y],
                    source_width,
                    source_height,
                },
            });
        }

        Ok(tiles)
    }
}

#[derive(Debug)]
pub enum DocumentWorkspaceBuildError<E> {
    Atlas(NewAtlasError<E>),
    Registry(GlobalStorageError),
}

#[derive(Debug)]
pub enum DocumentPresentError {
    MissingRoot { id: ImageId },
    Tile(GlobalTileError),
}

impl Display for DocumentPresentError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingRoot { id } => write!(f, "document root image {id:?} is missing"),
            Self::Tile(error) => write!(f, "document root tile access failed: {error}"),
        }
    }
}

impl Error for DocumentPresentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::MissingRoot { .. } => None,
            Self::Tile(error) => Some(error),
        }
    }
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
    use gla_renderer::{Pass, RenderBackend};
    use std::fmt::{Display, Formatter};

    #[derive(Debug)]
    struct RecordingBackendError;

    impl Display for RecordingBackendError {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            f.write_str("recording backend failed")
        }
    }

    impl Error for RecordingBackendError {}

    #[derive(Default)]
    struct RecordingBackend {
        submitted: Vec<Vec<Pass>>,
    }

    impl RecordingBackend {
        fn submitted_passes(&self) -> impl Iterator<Item = Pass> + '_ {
            self.submitted
                .iter()
                .flat_map(|passes| passes.iter().copied())
        }
    }

    impl RenderBackend for RecordingBackend {
        type Error = RecordingBackendError;

        fn submit(&mut self, passes: &[Pass]) -> Result<(), Self::Error> {
            self.submitted.push(passes.to_vec());
            Ok(())
        }
    }

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
    fn replace_circle_on_root_flushes_and_commits_document_edit() {
        let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
        let mut history = DrawHistory::new();
        let mut backend = RecordingBackend::default();

        let commit = workspace
            .replace_circle_on_root(
                &mut history,
                &mut backend,
                24.0,
                32.0,
                8.0,
                PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0),
            )
            .unwrap()
            .unwrap();

        assert_eq!(commit.version, DocumentVersionId::new(2));
        assert_eq!(workspace.version(), DocumentVersionId::new(2));
        assert!(backend.submitted_passes().any(|pass| matches!(
            pass,
            Pass::DrawOn(gla_draw_on::DrawOnInvocation::ReplaceCircle4D { .. })
        )));
    }

    #[test]
    fn root_present_tiles_skip_zero_tiles_and_include_committed_physical_tiles() {
        let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
        assert!(workspace.root_present_tiles().unwrap().is_empty());

        let mut history = DrawHistory::new();
        let mut backend = RecordingBackend::default();
        workspace
            .replace_circle_on_root(
                &mut history,
                &mut backend,
                24.0,
                32.0,
                8.0,
                PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0),
            )
            .unwrap()
            .unwrap();

        let tiles = workspace.root_present_tiles().unwrap();
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].params.target_min_px, [0.0, 0.0]);
        assert_eq!(tiles[0].params.target_max_px, [62.0, 62.0]);
        assert_eq!(tiles[0].params.source_width, IMAGE_TILE_SIZE);
        assert_eq!(tiles[0].params.source_height, IMAGE_TILE_SIZE);
    }

    #[test]
    fn root_present_tiles_apply_view_transform() {
        let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
        let mut history = DrawHistory::new();
        let mut backend = RecordingBackend::default();
        workspace
            .replace_circle_on_root(
                &mut history,
                &mut backend,
                24.0,
                32.0,
                8.0,
                PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0),
            )
            .unwrap()
            .unwrap();
        let view = AppView::new([2.0, 0.0, 0.0, 2.0, 10.0, 20.0]).unwrap();

        let tiles = workspace.root_present_tiles_for_view(&view).unwrap();

        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].params.target_min_px, [10.0, 20.0]);
        assert_eq!(tiles[0].params.target_max_px, [134.0, 144.0]);
    }

    #[test]
    fn draw_record_can_be_applied_for_undo_and_redo() {
        let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
        let mut history = DrawHistory::new();
        let mut backend = RecordingBackend::default();
        let commit = workspace
            .replace_circle_on_root(
                &mut history,
                &mut backend,
                24.0,
                32.0,
                8.0,
                PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0),
            )
            .unwrap()
            .unwrap();

        assert!(!workspace.root_present_tiles().unwrap().is_empty());
        let redo_record = workspace
            .apply_draw_record(&mut history, &mut backend, commit.record_id)
            .unwrap();
        assert!(workspace.root_present_tiles().unwrap().is_empty());
        let undo_record = workspace
            .apply_draw_record(&mut history, &mut backend, redo_record)
            .unwrap();

        assert_ne!(undo_record, redo_record);
        assert!(!workspace.root_present_tiles().unwrap().is_empty());
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
