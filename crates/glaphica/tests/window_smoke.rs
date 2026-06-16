use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::{Command, Output};

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

    let output = run_window_smoke([
        OsString::from("--run-command-plan"),
        plan_path.into_os_string(),
        OsString::from("--exit-after-frames"),
        OsString::from("8"),
    ]);
    assert_success(&output);

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

#[test]
#[ignore = "requires xvfb and a GPU-capable wgpu adapter"]
fn dev_style_replay_trace_runs_in_window() {
    let trace_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("app_trace_replay.json");
    let output = run_window_smoke([
        OsString::from("--replay-input"),
        trace_path.into_os_string(),
        OsString::from("--exit-after-frames"),
        OsString::from("6"),
    ]);
    assert_success(&output);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("stroke preview brush input failed"),
        "window replay smoke should not report preview brush input errors:\n{stderr}"
    );
    assert!(
        !stderr.contains("length mismatch"),
        "window replay smoke should not reject round brush input blocks:\n{stderr}"
    );
}

fn smoke_root(name: &str) -> PathBuf {
    let base = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap().join("target/window-smoke"));
    base.join(format!("{name}-{}", std::process::id()))
}

fn run_window_smoke<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("timeout");
    command
        .arg("30s")
        .arg("xvfb-run")
        .arg("-a")
        .arg(env!("CARGO_BIN_EXE_glaphica"));
    for arg in args {
        command.arg(arg);
    }
    command
        .output()
        .expect("timeout and xvfb-run should launch the glaphica window smoke")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "window smoke failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
