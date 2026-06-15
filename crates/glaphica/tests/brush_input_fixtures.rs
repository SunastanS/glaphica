use gla_core::CanvasCoordF;
use glaphica::{
    BrushId, BrushInput, BrushInputProcessor, ROUND_BRUSH_INPUT_BLOCK_VALUE_COUNT,
    RoundBrushInputProcessor, encode_round_apply_payload,
};

const ROUND_BRUSH_INPUT_FIXTURE: &str = include_str!("fixtures/round_brush_input.json");

#[test]
fn round_brush_input_fixture_is_readable_and_writable_json() {
    let input: BrushInput = serde_json::from_str(ROUND_BRUSH_INPUT_FIXTURE).unwrap();

    assert_eq!(input.brush_id, BrushId::DEFAULT);
    assert_eq!(input.blocks.brush_id(), BrushId::DEFAULT);
    assert_eq!(input.blocks.blocks().len(), 1);
    assert_eq!(
        input.blocks.blocks()[0].values().len(),
        ROUND_BRUSH_INPUT_BLOCK_VALUE_COUNT
    );

    let rendered = serde_json::to_string_pretty(&input).unwrap();
    let round_trip: BrushInput = serde_json::from_str(&rendered).unwrap();

    assert_eq!(round_trip, input);
    assert!(rendered.contains("\"values\""));
}

#[test]
fn round_brush_input_fixture_drives_processor_payload_interface() {
    let input: BrushInput = serde_json::from_str(ROUND_BRUSH_INPUT_FIXTURE).unwrap();
    let processor = RoundBrushInputProcessor::default();

    assert_eq!(
        processor.block_center(&input, 0).unwrap(),
        CanvasCoordF::new(0.0, 0.0)
    );
    assert_eq!(
        processor
            .encode_apply_dab_payload(&input, 0, CanvasCoordF::new(-2.0, -3.0))
            .unwrap(),
        encode_round_apply_payload([2.0, 3.0], 5.0, 0.5)
    );
}
