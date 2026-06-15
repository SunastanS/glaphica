use atlas::{AtlasLayout, NoAtlasTextures};
use gla_color::{ChannelCount, ChannelType, GlaFormat};
use gla_ir::{
    DeriveCommand, DocumentImageAccess, DocumentVersionId, DrawOnToolKind, ImageId, MetadataRef,
    SessionCommand, SessionImageDecl, SessionRead, draw_session_ir_from_json_str,
    registry_patch_from_json_str,
};
use gla_session::{DrawSession, SessionError};
use gla_storage::GlobalStorage;
use tile_key::Tiles;

const BASIC_REGISTRY_PATCH_JSON: &str =
    include_str!("../../gla_ir/tests/fixtures/basic_registry_patch.json");
const PIXEL_ROUND_SESSION_JSON: &str =
    include_str!("../../gla_ir/tests/fixtures/pixel_round_session.json");

#[test]
fn registry_patch_fixture_applies_to_global_storage() {
    let mut storage = storage_with_fixture_atlases();
    let patch =
        registry_patch_from_json_str(BASIC_REGISTRY_PATCH_JSON).expect("patch fixture parses");

    storage
        .apply_registry_patch(patch)
        .expect("patch fixture applies to storage");

    assert_eq!(storage.version(), DocumentVersionId::new(1));
    assert_eq!(storage.root(), Some(ImageId::new(2)));
    assert!(
        storage
            .image(ImageId::new(1))
            .expect("primitive image exists")
            .role()
            .is_primitive()
    );
    assert!(
        storage
            .image(ImageId::new(2))
            .expect("derived root image exists")
            .role()
            .is_derived()
    );
}

#[test]
fn pixel_round_session_fixture_begins_and_routes_draw_input() {
    let mut storage = storage_from_registry_fixture();
    let ir =
        draw_session_ir_from_json_str(PIXEL_ROUND_SESSION_JSON).expect("session fixture parses");

    assert!(
        ir.required_draw_on_tools()
            .contains(&DrawOnToolKind::RadialKernel1D)
    );

    let mut session = DrawSession::begin(&ir, &mut storage).expect("session fixture begins");
    assert_eq!(
        session.expected_document_version(),
        DocumentVersionId::new(1)
    );

    let routes = {
        let frame = session.begin_frame();
        frame
            .route_draw_targets(ImageId::new(1), 12.0, 13.0)
            .expect("draw input routes through session derive command")
    };

    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].target, ImageId::new(10));
    assert_eq!(routes[0].tool, DrawOnToolKind::RadialKernel1D);
    assert_eq!(routes[0].target_x, 12.0);
    assert_eq!(routes[0].target_y, 13.0);

    session.discard();
}

#[test]
fn edited_session_fixture_reports_stable_errors() {
    let ir =
        draw_session_ir_from_json_str(PIXEL_ROUND_SESSION_JSON).expect("session fixture parses");

    let mut wrong_version_ir = ir.clone();
    wrong_version_ir.expected_document_version = DocumentVersionId::default();
    let mut storage = storage_from_registry_fixture();
    let err = DrawSession::begin(&wrong_version_ir, &mut storage).unwrap_err();
    assert!(matches!(
        err,
        SessionError::ExpectedDocumentVersion {
            expected,
            actual
        } if expected == DocumentVersionId::default() && actual == DocumentVersionId::new(1)
    ));

    let mut read_only_ir = ir.clone();
    read_only_ir.doc_images[0].access = DocumentImageAccess::Read;
    let mut storage = storage_from_registry_fixture();
    let err = DrawSession::begin(&read_only_ir, &mut storage).unwrap_err();
    assert!(matches!(
        err,
        SessionError::DestinationNotWritable { id } if id == ImageId::new(1)
    ));

    let mut wrong_tool_ir = ir.clone();
    wrong_tool_ir.draw_on[0].tool = DrawOnToolKind::ReplaceCircle4D;
    let mut storage = storage_from_registry_fixture();
    let err = DrawSession::begin(&wrong_tool_ir, &mut storage).unwrap_err();
    assert!(matches!(
        err,
        SessionError::DrawOnFormatMismatch { id, tool, format }
            if id == ImageId::new(10)
                && tool == DrawOnToolKind::ReplaceCircle4D
                && format == value_format()
    ));
}

#[test]
fn edited_session_fixture_begins_and_routes_through_session_derive() {
    let mut ir =
        draw_session_ir_from_json_str(PIXEL_ROUND_SESSION_JSON).expect("session fixture parses");
    ir.session_images.push(SessionImageDecl::Derived {
        id: ImageId::new(11),
        format: MetadataRef::Like(ImageId::new(10)),
        layout: MetadataRef::Like(ImageId::new(10)),
        command: SessionCommand::new(vec![
            SessionRead::backup(ImageId::new(1)),
            SessionRead::current(ImageId::new(10)),
        ]),
    });
    ir.derive[0] = DeriveCommand::new(
        vec![
            SessionRead::backup(ImageId::new(1)),
            SessionRead::current(ImageId::new(11)),
        ],
        ImageId::new(1),
    );
    let mut storage = storage_from_registry_fixture();

    let mut session = DrawSession::begin(&ir, &mut storage).expect("edited session fixture begins");
    let routes = {
        let frame = session.begin_frame();
        frame
            .route_draw_targets(ImageId::new(1), 18.0, 19.0)
            .expect("edited derive chain routes draw input")
    };

    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].target, ImageId::new(10));
    assert_eq!(routes[0].tool, DrawOnToolKind::RadialKernel1D);
    assert_eq!(routes[0].target_x, 18.0);
    assert_eq!(routes[0].target_y, 19.0);
    session.discard();
}

fn storage_from_registry_fixture() -> GlobalStorage {
    let mut storage = storage_with_fixture_atlases();
    let patch =
        registry_patch_from_json_str(BASIC_REGISTRY_PATCH_JSON).expect("patch fixture parses");
    storage
        .apply_registry_patch(patch)
        .expect("patch fixture applies to storage");
    storage
}

fn storage_with_fixture_atlases() -> GlobalStorage {
    let mut tiles = Tiles::new();
    let mut textures = NoAtlasTextures;
    tiles
        .new_atlas(AtlasLayout::TINY8, rgba_format(), &mut textures)
        .expect("rgba fixture atlas allocates");
    tiles
        .new_atlas(AtlasLayout::TINY8, value_format(), &mut textures)
        .expect("value fixture atlas allocates");
    GlobalStorage::new(tiles)
}

fn rgba_format() -> GlaFormat {
    GlaFormat {
        channel_count: ChannelCount::D4,
        channel_type: ChannelType::F32,
    }
}

fn value_format() -> GlaFormat {
    GlaFormat {
        channel_count: ChannelCount::D1,
        channel_type: ChannelType::F32,
    }
}
