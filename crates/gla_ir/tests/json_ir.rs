use gla_color::{ChannelCount, ChannelType, GlaFormat};
use gla_ir::{
    DeriveCommand, DocImageUse, DocumentImageAccess, DocumentVersionId, DrawOnCommand,
    DrawOnToolKind, ImageId, ImageLayoutSpec, ImageRole, MetadataRef, SessionCommand,
    SessionImageDecl, SessionRead, SessionReadImage, draw_session_ir_from_json_str,
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
fn edited_draw_session_fixture_is_writable_and_readable_json() {
    let source = include_str!("fixtures/pixel_round_session.json");
    let mut ir = draw_session_ir_from_json_str(source).expect("session fixture should parse");

    ir.expected_document_version = DocumentVersionId::new(9);
    ir.doc_images[0] = DocImageUse::read_write(ImageId::new(1));
    ir.doc_images.push(DocImageUse::read(ImageId::new(2)));
    ir.session_images[0] = SessionImageDecl::Primitive {
        id: ImageId::new(10),
        format: MetadataRef::Concrete(GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::F32,
        }),
        layout: MetadataRef::Concrete(ImageLayoutSpec::new(64, 32)),
    };
    ir.session_images.push(SessionImageDecl::Derived {
        id: ImageId::new(11),
        format: MetadataRef::Like(ImageId::new(10)),
        layout: MetadataRef::Like(ImageId::new(10)),
        command: SessionCommand::new(vec![
            SessionRead::backup(ImageId::new(1)),
            SessionRead::current(ImageId::new(10)),
        ]),
    });
    ir.draw_on[0] = DrawOnCommand::with_tool(ImageId::new(10), DrawOnToolKind::ReplaceCircle4D);
    ir.derive[0] = DeriveCommand::new(
        vec![
            SessionRead::backup(ImageId::new(1)),
            SessionRead::current(ImageId::new(11)),
        ],
        ImageId::new(1),
    );

    let rendered = draw_session_ir_to_json_string_pretty(&ir).expect("edited IR should render");
    let reparsed =
        draw_session_ir_from_json_str(&rendered).expect("edited rendered IR should parse again");

    assert_eq!(reparsed, ir);
    assert!(rendered.contains("\"ReplaceCircle4D\""));
    assert!(matches!(
        &reparsed.session_images[1],
        SessionImageDecl::Derived { id, .. } if *id == ImageId::new(11)
    ));
    assert_eq!(reparsed.required_draw_on_tools().len(), 1);
    assert!(
        reparsed
            .required_draw_on_tools()
            .contains(&DrawOnToolKind::ReplaceCircle4D)
    );
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
