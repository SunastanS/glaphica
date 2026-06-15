use glaphica::{
    AppTraceBlendMode, AppTraceEvent, AppTraceUiAction, load_trace_file, save_trace_file,
};

const APP_TRACE_REPLAY_JSON: &str = include_str!("fixtures/app_trace_replay.json");
const DEV_PREVIEW_TRACE_LEGACY_JSON: &str = include_str!("fixtures/dev_preview_trace_legacy.json");

#[test]
fn app_trace_replay_fixture_is_readable_and_writable_json() {
    let source_path = write_fixture("app-trace-replay-source", APP_TRACE_REPLAY_JSON);
    let rendered_path = trace_path("app-trace-replay-rendered");

    let events = load_trace_file(&source_path).expect("app trace replay fixture should parse");
    save_trace_file(&rendered_path, events.clone()).expect("app trace replay fixture renders");
    let reparsed =
        load_trace_file(&rendered_path).expect("rendered app trace replay fixture parses");
    let rendered = std::fs::read_to_string(&rendered_path).unwrap();

    assert_eq!(reparsed, events);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AppTraceEvent::Ui(AppTraceUiAction::CreateLayer)))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AppTraceEvent::BeginStroke(_)))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AppTraceEvent::FinishStroke))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AppTraceEvent::Undo))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AppTraceEvent::Redo))
    );
    assert!(rendered.contains("\"BeginStroke\""));
    assert!(rendered.contains("\"FinishStroke\""));

    let _ = std::fs::remove_file(source_path);
    let _ = std::fs::remove_file(rendered_path);
}

#[test]
fn dev_preview_trace_legacy_fixture_imports_as_app_trace_events() {
    let source_path = write_fixture(
        "dev-preview-trace-legacy-source",
        DEV_PREVIEW_TRACE_LEGACY_JSON,
    );

    let events = load_trace_file(&source_path).expect("dev preview trace fixture should parse");

    assert!(matches!(
        events.as_slice(),
        [
            AppTraceEvent::Ui(AppTraceUiAction::DeleteActiveNode),
            AppTraceEvent::BeginStroke(_),
            AppTraceEvent::StrokeSample(_),
            AppTraceEvent::FinishStroke,
            AppTraceEvent::Ui(AppTraceUiAction::SetLayerBlendMode {
                visible_index: 1,
                blend_mode: AppTraceBlendMode::Multiply
            })
        ]
    ));

    let _ = std::fs::remove_file(source_path);
}

fn write_fixture(name: &str, source: &str) -> std::path::PathBuf {
    let path = trace_path(name);
    std::fs::write(&path, source).unwrap();
    path
}

fn trace_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "glaphica-app-trace-fixture-{name}-{}.json",
        std::process::id()
    ))
}
