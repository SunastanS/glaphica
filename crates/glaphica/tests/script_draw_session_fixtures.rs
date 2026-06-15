use std::error::Error;
use std::fmt::{Display, Formatter};

use glaphica::{
    DocumentWorkspace, DrawHistory, ScriptDrawCommand, script_draw_session_from_json_str,
    script_draw_session_to_json_string_pretty,
};

const SCRIPT_REPLACE_CIRCLE_SESSION_JSON: &str =
    include_str!("fixtures/script_replace_circle_session.json");

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
    submitted: Vec<Vec<gla_renderer::Pass>>,
}

impl RecordingBackend {
    fn submitted_passes(&self) -> impl Iterator<Item = gla_renderer::Pass> + '_ {
        self.submitted
            .iter()
            .flat_map(|passes| passes.iter().copied())
    }
}

impl gla_renderer::RenderBackend for RecordingBackend {
    type Error = RecordingBackendError;

    fn submit(&mut self, passes: &[gla_renderer::Pass]) -> Result<(), Self::Error> {
        self.submitted.push(passes.to_vec());
        Ok(())
    }
}

#[test]
fn script_draw_session_fixture_is_readable_and_writable_json() {
    let request = script_draw_session_from_json_str(SCRIPT_REPLACE_CIRCLE_SESSION_JSON)
        .expect("script draw session fixture should parse");

    assert_eq!(request.frames.len(), 1);
    assert!(matches!(
        request.frames[0].commands.as_slice(),
        [ScriptDrawCommand::DrawOn { .. }]
    ));

    let rendered =
        script_draw_session_to_json_string_pretty(&request).expect("script draw session renders");
    let reparsed =
        script_draw_session_from_json_str(&rendered).expect("rendered session parses again");

    assert_eq!(reparsed, request);
}

#[test]
fn script_draw_session_fixture_executes_against_workspace() {
    let request = script_draw_session_from_json_str(SCRIPT_REPLACE_CIRCLE_SESSION_JSON)
        .expect("script draw session fixture should parse");
    let mut workspace = DocumentWorkspace::blank(128, 96).unwrap();
    let mut history = DrawHistory::new();
    let mut backend = RecordingBackend::default();

    let commit = workspace
        .run_script_draw_session(&mut history, &mut backend, &request)
        .unwrap()
        .unwrap();

    assert_eq!(workspace.version(), commit.version);
    assert_eq!(workspace.root_dirty_tile_indices(&commit), vec![0]);
    assert!(backend.submitted_passes().any(|pass| matches!(
        pass,
        gla_renderer::Pass::DrawOn(gla_draw_on::DrawOnInvocation::ReplaceCircle4D { .. })
    )));
}
