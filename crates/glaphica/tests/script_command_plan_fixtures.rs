use glaphica::{
    ActiveTool, DocumentBlendMode, DocumentWorkspace, NullScriptRuntime, ScriptCommand,
    ScriptCommandOutcome, ScriptHost, ScriptHostError, ScriptRuntime,
    script_command_plan_from_json_str, script_command_plan_to_json_string_pretty,
};

const SCRIPT_COMMAND_PLAN_JSON: &str = include_str!("fixtures/script_command_plan.json");

#[derive(Default)]
struct RecordingHost {
    commands: Vec<ScriptCommand>,
}

impl ScriptHost for RecordingHost {
    fn execute_script_command(
        &mut self,
        command: ScriptCommand,
    ) -> Result<ScriptCommandOutcome, ScriptHostError> {
        self.commands.push(command);
        Ok(ScriptCommandOutcome::None)
    }
}

#[test]
fn script_command_plan_fixture_is_readable_and_writable_json() {
    let plan = script_command_plan_from_json_str(SCRIPT_COMMAND_PLAN_JSON)
        .expect("script command plan fixture should parse");

    let rendered =
        script_command_plan_to_json_string_pretty(&plan).expect("script command plan renders");
    let reparsed =
        script_command_plan_from_json_str(&rendered).expect("rendered command plan parses again");

    assert_eq!(reparsed, plan);
    assert!(rendered.contains("\"CreateLayerAboveActive\""));
    assert!(rendered.contains("\"RunDrawSession\""));
}

#[test]
fn script_command_plan_fixture_reserves_full_rust_command_surface() {
    let plan = script_command_plan_from_json_str(SCRIPT_COMMAND_PLAN_JSON)
        .expect("script command plan fixture should parse");

    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::ApplyRegistryPatch(_)))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::OpenWorkspaceDirectory(_)))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::AppendLayer { .. }))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::AppendGroup { .. }))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::CreateLayerAboveActive))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::CreateGroupAboveActive))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::DeleteNode(_)))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::DeleteActiveNode))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::MoveNode { .. }))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::SetActiveNode(_)))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::SetNodeOpacity { .. }))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::SetNodeBlendMode { .. }))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::RunDrawSession(_)))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::SetActiveTool(ActiveTool::Brush(_))))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::SetRoundBrushSettings(_)))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::BeginStroke(_)))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::PushStrokeInput(_)))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::FinishStroke))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::CancelStroke))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::Undo))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::Redo))
    );
    assert!(
        plan.commands
            .iter()
            .any(|command| matches!(command, ScriptCommand::RequestRedraw))
    );
}

#[test]
fn script_command_plan_fixture_executes_layer_and_input_subset_against_host() {
    let plan = script_command_plan_from_json_str(SCRIPT_COMMAND_PLAN_JSON)
        .expect("script command plan fixture should parse");
    let mut host = RecordingHost::default();

    for command in plan
        .commands
        .iter()
        .filter(|command| {
            matches!(
                command,
                ScriptCommand::CreateLayerAboveActive
                    | ScriptCommand::SetRoundBrushSettings(_)
                    | ScriptCommand::BeginStroke(_)
                    | ScriptCommand::PushStrokeInput(_)
                    | ScriptCommand::CancelStroke
            )
        })
        .cloned()
    {
        host.execute_script_command(command).unwrap();
    }

    assert_eq!(host.commands.len(), 5);
    assert!(matches!(
        host.commands[0],
        ScriptCommand::CreateLayerAboveActive
    ));
    assert!(matches!(host.commands[4], ScriptCommand::CancelStroke));
}

#[test]
fn script_command_plan_layer_subset_executes_against_workspace() {
    let plan = script_command_plan_from_json_str(SCRIPT_COMMAND_PLAN_JSON)
        .expect("script command plan fixture should parse");
    let mut workspace = DocumentWorkspace::blank(320, 240).unwrap();

    let layer = plan
        .commands
        .iter()
        .find_map(|command| match command {
            ScriptCommand::CreateLayerAboveActive => {
                Some(workspace.insert_layer_above_active().unwrap())
            }
            _ => None,
        })
        .expect("fixture should create a layer");
    for command in plan.commands.iter().filter(|command| {
        matches!(
            command,
            ScriptCommand::SetNodeOpacity { .. } | ScriptCommand::SetNodeBlendMode { .. }
        )
    }) {
        match command {
            ScriptCommand::SetNodeOpacity { node_id, opacity } => {
                assert_eq!(*node_id, layer);
                workspace.set_node_opacity(*node_id, *opacity).unwrap();
            }
            ScriptCommand::SetNodeBlendMode {
                node_id,
                blend_mode,
            } => {
                assert_eq!(*node_id, layer);
                workspace
                    .set_node_blend_mode(*node_id, *blend_mode)
                    .unwrap();
            }
            _ => panic!("fixture command should be a layer metadata update"),
        }
    }

    let node = workspace.layer_tree().node(layer).unwrap();
    assert_eq!(node.opacity(), 0.5);
    assert_eq!(node.blend_mode(), DocumentBlendMode::Multiply);
}

#[test]
fn null_runtime_can_load_command_plan_as_future_janet_source() {
    let mut runtime = NullScriptRuntime::new();
    let mut host = RecordingHost::default();

    let module = runtime
        .load_module(glaphica::ScriptModuleSource::new(
            "script_command_plan.json",
            SCRIPT_COMMAND_PLAN_JSON,
        ))
        .unwrap();
    let error = runtime
        .call_entry(&mut host, module, "main", &[])
        .unwrap_err();

    assert_eq!(
        runtime.module_source(module).unwrap().source,
        SCRIPT_COMMAND_PLAN_JSON
    );
    assert!(matches!(
        error,
        glaphica::ScriptRuntimeError::EntryUnavailable { .. }
    ));
}
