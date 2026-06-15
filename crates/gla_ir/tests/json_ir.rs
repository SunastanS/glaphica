use gla_color::{ChannelCount, ChannelType, GlaFormat};
use gla_ir::{
    DocumentImageAccess, DocumentVersionId, DrawOnToolKind, ImageId, ImageRole, MetadataRef,
    SessionImageDecl, SessionReadImage, draw_session_ir_from_json_str,
    draw_session_ir_to_json_string_pretty, registry_patch_from_json_str,
    registry_patch_to_json_string_pretty,
};

#[test]
fn registry_patch_fixture_is_readable_and_writable_json() {
    let source = include_str!("fixtures/basic_registry_patch.json");
    let patch = registry_patch_from_json_str(source).expect("registry fixture should parse");

    assert_eq!(patch.ops.len(), 3);
    let rendered = registry_patch_to_json_string_pretty(&patch).expect("patch should render");
    let reparsed =
        registry_patch_from_json_str(&rendered).expect("rendered patch should parse again");

    assert_eq!(reparsed, patch);
}

#[test]
fn pixel_round_session_fixture_preserves_draw_ir_contract() {
    let source = include_str!("fixtures/pixel_round_session.json");
    let ir = draw_session_ir_from_json_str(source).expect("session fixture should parse");

    assert_eq!(ir.expected_document_version, DocumentVersionId::new(1));
    assert_eq!(ir.doc_images.len(), 1);
    assert_eq!(ir.doc_images[0].id, ImageId::new(1));
    assert_eq!(ir.doc_images[0].access, DocumentImageAccess::ReadWrite);
    assert_eq!(ir.draw_on[0].tool, DrawOnToolKind::RadialKernel1D);
    assert_eq!(ir.required_draw_on_tools().len(), 1);

    let SessionImageDecl::Primitive { id, format, layout } = &ir.session_images[0] else {
        panic!("pixel round fixture should declare primitive coverage");
    };
    assert_eq!(*id, ImageId::new(10));
    assert_eq!(
        *format,
        MetadataRef::Concrete(GlaFormat {
            channel_count: ChannelCount::D1,
            channel_type: ChannelType::F32,
        })
    );
    assert_eq!(*layout, MetadataRef::Like(ImageId::new(1)));
    assert_eq!(
        ir.derive[0].command.reads[0].image,
        SessionReadImage::Backup(ImageId::new(1))
    );
    assert_eq!(
        ir.derive[0].command.reads[1].image,
        SessionReadImage::Current(ImageId::new(10))
    );

    let rendered = draw_session_ir_to_json_string_pretty(&ir).expect("session IR should render");
    let reparsed =
        draw_session_ir_from_json_str(&rendered).expect("rendered session IR should parse again");

    assert_eq!(reparsed, ir);
}

#[test]
fn registry_patch_fixture_contains_a_derived_root_image() {
    let patch =
        registry_patch_from_json_str(include_str!("fixtures/basic_registry_patch.json")).unwrap();

    let derived_count = patch
        .ops
        .iter()
        .filter(|op| {
            matches!(
                op,
                gla_ir::RegistryPatchOp::NewImage {
                    role: ImageRole::Derived(_),
                    ..
                }
            )
        })
        .count();

    assert_eq!(derived_count, 1);
}
