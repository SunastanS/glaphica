use std::path::PathBuf;
use std::process::Command;

use glaphica::{
    ScriptCommand, read_workspace_manifest, script_command_plan_from_json_str,
    script_command_plan_to_json_string_pretty,
};

const STARTUP_EXPORT_OPEN_COMMAND_PLAN_JSON: &str =
    include_str!("fixtures/startup_export_open_command_plan.json");

#[test]
#[ignore = "requires xvfb and a GPU-capable wgpu adapter"]
fn startup_export_open_command_plan_runs_in_window() {
    let smoke_root = smoke_root("startup-export-open");
    let plan_path = smoke_root.join("startup_export_open_command_plan.json");
    let export_path = smoke_root.join("workspace");
    let _ = std::fs::remove_dir_all(&smoke_root);
    std::fs::create_dir_all(&smoke_root).unwrap();

    let mut plan = script_command_plan_from_json_str(STARTUP_EXPORT_OPEN_COMMAND_PLAN_JSON)
        .expect("startup export/open command plan fixture should parse");
    for command in &mut plan.commands {
        match command {
            ScriptCommand::ExportWorkspaceDirectory(path)
            | ScriptCommand::OpenWorkspaceDirectory(path) => {
                *path = export_path.clone();
            }
            _ => {}
        }
    }
    let rendered =
        script_command_plan_to_json_string_pretty(&plan).expect("smoke command plan renders");
    std::fs::write(&plan_path, rendered).unwrap();

    let output = Command::new("timeout")
        .arg("30s")
        .arg("xvfb-run")
        .arg("-a")
        .arg(env!("CARGO_BIN_EXE_glaphica"))
        .arg("--run-command-plan")
        .arg(&plan_path)
        .arg("--exit-after-frames")
        .arg("8")
        .output()
        .expect("timeout and xvfb-run should launch the glaphica window smoke");

    assert!(
        output.status.success(),
        "window smoke failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let manifest = read_workspace_manifest(&export_path)
        .expect("window smoke should export a readable workspace manifest");
    assert!(
        !manifest.tiles.is_empty(),
        "window smoke should export at least one physical tile"
    );
    assert!(
        export_path.join("tiles").is_dir(),
        "window smoke should export tile assets"
    );
}

fn smoke_root(name: &str) -> PathBuf {
    let base = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap().join("target/window-smoke"));
    base.join(format!("{name}-{}", std::process::id()))
}
