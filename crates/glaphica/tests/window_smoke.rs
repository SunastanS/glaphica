use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use glaphica::{
    ScriptCommand, load_trace_file, read_workspace_directory, read_workspace_manifest,
    script_command_plan_from_json_str, script_command_plan_to_json_string_pretty,
};

const STARTUP_EXPORT_OPEN_COMMAND_PLAN_JSON: &str =
    include_str!("fixtures/startup_export_open_command_plan.json");

#[test]
#[ignore = "requires xvfb and a GPU-capable wgpu adapter"]
fn startup_export_open_command_plan_runs_in_window() {
    let smoke_root = smoke_root("startup-export-open");
    let _ = std::fs::remove_dir_all(&smoke_root);
    std::fs::create_dir_all(&smoke_root).unwrap();
    let (plan_path, export_path) = write_startup_export_open_plan(&smoke_root);

    let output = run_window_smoke([
        OsString::from("--run-command-plan"),
        plan_path.into_os_string(),
        OsString::from("--exit-after-frames"),
        OsString::from("8"),
    ]);
    assert_success(&output);
    assert_perf_frames(&output, 8);
    assert_readable_workspace_export(&export_path);
}

#[test]
#[ignore = "requires xvfb and a GPU-capable wgpu adapter"]
fn open_workspace_argument_runs_exported_workspace_in_window() {
    let smoke_root = smoke_root("open-workspace");
    let _ = std::fs::remove_dir_all(&smoke_root);
    std::fs::create_dir_all(&smoke_root).unwrap();
    let (plan_path, export_path) = write_startup_export_open_plan(&smoke_root);

    let export_output = run_window_smoke([
        OsString::from("--run-command-plan"),
        plan_path.into_os_string(),
        OsString::from("--exit-after-frames"),
        OsString::from("8"),
    ]);
    assert_success(&export_output);
    assert_perf_frames(&export_output, 8);
    assert_readable_workspace_export(&export_path);

    let open_output = run_window_smoke([
        OsString::from("--open-workspace"),
        export_path.into_os_string(),
        OsString::from("--exit-after-frames"),
        OsString::from("4"),
    ]);
    assert_success(&open_output);
    assert_perf_frames(&open_output, 4);
    let output_text = command_output_text(&open_output);
    assert!(
        !output_text.contains("failed to import workspace"),
        "open-workspace smoke should not report import failures:\n{output_text}"
    );
}

#[test]
#[ignore = "requires xvfb and a GPU-capable wgpu adapter"]
fn dev_style_replay_trace_runs_in_window() {
    let smoke_root = smoke_root("replay-export-on-exit");
    let export_path = smoke_root.join("workspace");
    let _ = std::fs::remove_dir_all(&smoke_root);
    std::fs::create_dir_all(&smoke_root).unwrap();
    let trace_path = fixture_path("app_trace_replay.json");
    let output = run_window_smoke([
        OsString::from("--replay-input"),
        trace_path.into_os_string(),
        OsString::from("--export-workspace-on-exit"),
        export_path.clone().into_os_string(),
        OsString::from("--exit-after-frames"),
        OsString::from("10"),
    ]);
    assert_success(&output);
    assert_perf_frames(&output, 10);
    assert_readable_workspace_export(&export_path);

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

#[test]
#[ignore = "requires xvfb and a GPU-capable wgpu adapter"]
fn dev_preview_legacy_trace_runs_in_window() {
    let smoke_root = smoke_root("legacy-replay-export-on-exit");
    let export_path = smoke_root.join("workspace");
    let _ = std::fs::remove_dir_all(&smoke_root);
    std::fs::create_dir_all(&smoke_root).unwrap();
    let trace_path = fixture_path("dev_preview_trace_legacy.json");
    let output = run_window_smoke([
        OsString::from("--replay-input"),
        trace_path.into_os_string(),
        OsString::from("--export-workspace-on-exit"),
        export_path.clone().into_os_string(),
        OsString::from("--exit-after-frames"),
        OsString::from("10"),
    ]);
    assert_success(&output);
    assert_perf_frames(&output, 10);
    assert_readable_workspace_export(&export_path);

    let output_text = command_output_text(&output);
    assert!(
        !output_text.contains("trace replay event failed"),
        "legacy dev replay smoke should not report trace replay errors:\n{output_text}"
    );
    assert!(
        !output_text.contains("length mismatch"),
        "legacy dev replay smoke should not reject round brush input blocks:\n{output_text}"
    );
}

#[test]
#[ignore = "requires xvfb, glaphica-dev, and a GPU-capable wgpu adapter"]
fn manual_legacy_replay_perf_matches_dev_preview_baseline() {
    let Some(dev_root) = std::env::var_os("GLAPHICA_DEV_ROOT").map(PathBuf::from) else {
        eprintln!("skipping perf comparison because GLAPHICA_DEV_ROOT is not set");
        return;
    };
    let trace_path = fixture_path("dev_preview_trace_legacy.json");

    let dev_output = run_dev_preview_perf_smoke(&dev_root, &trace_path);
    assert_success_or_timeout(&dev_output);
    let dev_frames = preview_perf_frames(&dev_output);
    assert!(
        dev_frames.len() >= 2,
        "dev preview perf baseline should report at least 2 frames, got {}\n{}",
        dev_frames.len(),
        command_output_text(&dev_output)
    );
    assert!(
        dev_frames.iter().any(|frame| frame.dirty_tiles > 0),
        "dev preview perf baseline should include at least one dirty frame\n{}",
        command_output_text(&dev_output)
    );

    let manual_frame_count = dev_frames.len().max(10);
    let manual_output = run_window_smoke([
        OsString::from("--replay-input"),
        trace_path.into_os_string(),
        OsString::from("--exit-after-frames"),
        OsString::from(manual_frame_count.to_string()),
    ]);
    assert_success(&manual_output);
    let manual_frames = app_perf_frames(&manual_output);
    assert!(
        manual_frames.len() >= manual_frame_count,
        "manual perf sample should report at least {} frames, got {}\n{}",
        manual_frame_count,
        manual_frames.len(),
        command_output_text(&manual_output)
    );

    let dev_stats = PerfStats::from_frames(&dev_frames);
    let manual_stats = PerfStats::from_frames(&manual_frames);
    assert_perf_not_worse(&manual_stats, &dev_stats);
}

#[test]
#[ignore = "requires xvfb and a GPU-capable wgpu adapter"]
fn dev_style_record_trace_writes_file_on_exit() {
    let smoke_root = smoke_root("record-trace");
    let trace_path = smoke_root.join("recorded_trace.json");
    let _ = std::fs::remove_dir_all(&smoke_root);
    std::fs::create_dir_all(&smoke_root).unwrap();

    let output = run_window_smoke([
        OsString::from("--record-input"),
        trace_path.clone().into_os_string(),
        OsString::from("--exit-after-frames"),
        OsString::from("1"),
    ]);
    assert_success(&output);
    assert_perf_frames(&output, 1);

    let events = load_trace_file(&trace_path).expect("window smoke should write readable trace");
    assert!(
        events.is_empty(),
        "record-only window smoke should not invent trace events"
    );
}

fn smoke_root(name: &str) -> PathBuf {
    let base = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap().join("target/window-smoke"));
    base.join(format!("{name}-{}", std::process::id()))
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn write_startup_export_open_plan(smoke_root: &Path) -> (PathBuf, PathBuf) {
    let plan_path = smoke_root.join("startup_export_open_command_plan.json");
    let export_path = smoke_root.join("workspace");
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
    (plan_path, export_path)
}

fn assert_readable_workspace_export(export_path: &Path) {
    let manifest = read_workspace_manifest(export_path)
        .expect("window smoke should export a readable workspace manifest");
    assert!(
        !manifest.tiles.is_empty(),
        "window smoke should export at least one physical tile"
    );
    assert!(
        manifest.layer_tree.is_some(),
        "window smoke should export layer tree metadata"
    );
    assert!(
        export_path.join("tiles").is_dir(),
        "window smoke should export tile assets"
    );
    for tile in &manifest.tiles {
        assert!(
            export_path.join(&tile.path).is_file(),
            "window smoke should export tile asset {}",
            tile.path.display()
        );
    }
    let snapshot = read_workspace_directory(export_path)
        .expect("window smoke should export a readable workspace snapshot");
    assert_eq!(snapshot.tiles.len(), manifest.tiles.len());
}

#[test]
fn parses_app_perf_line() {
    let line = "[PERF][app][frame=7] total_ms=2.000 bottleneck=process_inputs (1.500ms) dirty_tiles=3 stages_ms={process_inputs:1.500, update_cache:0.250, acquire_frame:0.125, present_surface:0.125}";
    let frame = parse_app_perf_line(line).expect("perf line should parse");

    assert_eq!(frame.index, 7);
    assert_eq!(frame.dirty_tiles, 3);
    assert_eq!(frame.total_ms, 2.0);
    assert_eq!(frame.process_inputs_ms, 1.5);
    assert_eq!(frame.update_cache_ms, 0.25);
    assert_eq!(frame.acquire_frame_ms, 0.125);
    assert_eq!(frame.present_surface_ms, 0.125);
}

fn run_window_smoke<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("timeout");
    command
        .arg("30s")
        .arg("env")
        .arg("GLAPHICA_APP_PERF_TRACE_STDERR=1")
        .arg("GLAPHICA_APP_PERF_TRACE_SLOW_MS=0")
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

fn assert_success_or_timeout(output: &Output) {
    assert!(
        output.status.success() || output.status.code() == Some(124),
        "window smoke failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_perf_frames(output: &Output, expected_minimum: usize) {
    let output_text = command_output_text(output);
    let frames = app_perf_frames(output);

    assert!(
        frames.len() >= expected_minimum,
        "window smoke should report at least {expected_minimum} perf frames, got {}\n{}",
        frames.len(),
        output_text
    );
    for frame in frames {
        assert_perf_frame_is_valid(frame);
    }
}

fn app_perf_frames(output: &Output) -> Vec<AppPerfFrame> {
    command_output_text(output)
        .lines()
        .filter_map(parse_app_perf_line)
        .collect()
}

fn preview_perf_frames(output: &Output) -> Vec<AppPerfFrame> {
    command_output_text(output)
        .lines()
        .filter_map(parse_preview_perf_line)
        .collect()
}

fn command_output_text(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn run_dev_preview_perf_smoke(dev_root: &Path, trace_path: &Path) -> Output {
    Command::new("timeout")
        .arg("12s")
        .arg("env")
        .arg("CARGO_BUILD_JOBS=1")
        .arg("GLAPHICA_PREVIEW_PERF_TRACE_STDERR=1")
        .arg("GLAPHICA_PREVIEW_PERF_TRACE_SLOW_MS=0")
        .arg("xvfb-run")
        .arg("-a")
        .arg("cargo")
        .arg("run")
        .arg("-p")
        .arg("app")
        .arg("--bin")
        .arg("preview")
        .arg("--")
        .arg("--replay-input")
        .arg(trace_path)
        .current_dir(dev_root)
        .output()
        .expect("timeout, xvfb-run, and cargo should launch glaphica-dev preview")
}

fn assert_perf_frame_is_valid(frame: AppPerfFrame) {
    assert!(
        frame.total_ms.is_finite()
            && frame.process_inputs_ms.is_finite()
            && frame.update_cache_ms.is_finite()
            && frame.acquire_frame_ms.is_finite()
            && frame.present_surface_ms.is_finite(),
        "perf frame should contain finite timings: {frame:?}"
    );
    assert!(
        frame.total_ms >= 0.0
            && frame.process_inputs_ms >= 0.0
            && frame.update_cache_ms >= 0.0
            && frame.acquire_frame_ms >= 0.0
            && frame.present_surface_ms >= 0.0,
        "perf frame timings should be non-negative: {frame:?}"
    );
}

fn assert_perf_not_worse(manual: &PerfStats, dev: &PerfStats) {
    const TOLERANCE: f64 = 1.15;

    assert!(
        manual.dirty_max_total_ms <= dev.dirty_max_total_ms * TOLERANCE,
        "manual dirty-frame max should stay within 15% of dev preview\nmanual: {manual:?}\ndev: {dev:?}"
    );
    if manual.warm_frame_count >= 20 && dev.warm_frame_count >= 20 {
        assert!(
            manual.warm_avg_total_ms <= dev.warm_avg_total_ms * TOLERANCE,
            "manual warm average should stay within 15% of dev preview\nmanual: {manual:?}\ndev: {dev:?}"
        );
        assert!(
            manual.warm_p95_total_ms <= dev.warm_p95_total_ms * TOLERANCE,
            "manual warm p95 should stay within 15% of dev preview\nmanual: {manual:?}\ndev: {dev:?}"
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct AppPerfFrame {
    index: u64,
    total_ms: f64,
    dirty_tiles: usize,
    process_inputs_ms: f64,
    update_cache_ms: f64,
    acquire_frame_ms: f64,
    present_surface_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PerfStats {
    frame_count: usize,
    dirty_frame_count: usize,
    warm_frame_count: usize,
    warm_avg_total_ms: f64,
    warm_p95_total_ms: f64,
    dirty_max_total_ms: f64,
}

impl PerfStats {
    fn from_frames(frames: &[AppPerfFrame]) -> Self {
        let warm_frames = frames
            .iter()
            .copied()
            .filter(|frame| frame.index > 10)
            .collect::<Vec<_>>();
        let warm_totals = warm_frames
            .iter()
            .map(|frame| frame.total_ms)
            .collect::<Vec<_>>();
        let dirty_frames = frames
            .iter()
            .copied()
            .filter(|frame| frame.dirty_tiles > 0)
            .collect::<Vec<_>>();
        Self {
            frame_count: frames.len(),
            dirty_frame_count: dirty_frames.len(),
            warm_frame_count: warm_frames.len(),
            warm_avg_total_ms: average(&warm_totals),
            warm_p95_total_ms: percentile(warm_totals, 0.95),
            dirty_max_total_ms: dirty_frames
                .iter()
                .map(|frame| frame.total_ms)
                .fold(0.0, f64::max),
        }
    }
}

fn parse_app_perf_line(line: &str) -> Option<AppPerfFrame> {
    parse_perf_line(line, "app")
}

fn parse_preview_perf_line(line: &str) -> Option<AppPerfFrame> {
    parse_perf_line(line, "preview")
}

fn parse_perf_line(line: &str, label: &str) -> Option<AppPerfFrame> {
    let frame_marker = format!("[PERF][{label}][frame=");
    if !line.starts_with(&frame_marker) {
        return None;
    }
    let stages = between(line, "stages_ms={", "}")?;
    Some(AppPerfFrame {
        index: parse_between(line, &frame_marker, "]")?,
        total_ms: parse_between(line, " total_ms=", " ")?,
        dirty_tiles: parse_between(line, " dirty_tiles=", " stages_ms=")?,
        process_inputs_ms: parse_stage_ms(stages, "process_inputs")?,
        update_cache_ms: parse_stage_ms(stages, "update_cache")?,
        acquire_frame_ms: parse_stage_ms(stages, "acquire_frame")?,
        present_surface_ms: parse_stage_ms(stages, "present_surface")?,
    })
}

fn average(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

fn percentile(mut values: Vec<f64>, percentile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let index = ((values.len() - 1) as f64 * percentile).round() as usize;
    values[index]
}

fn parse_stage_ms(stages: &str, name: &str) -> Option<f64> {
    let marker = format!("{name}:");
    let value = stages.split_once(&marker)?.1;
    let value = value.split_once(',').map_or(value, |(head, _)| head);
    value.trim().parse().ok()
}

fn parse_between<T>(source: &str, start_marker: &str, end_marker: &str) -> Option<T>
where
    T: std::str::FromStr,
{
    between(source, start_marker, end_marker)?.parse().ok()
}

fn between<'a>(source: &'a str, start_marker: &str, end_marker: &str) -> Option<&'a str> {
    let start = source.find(start_marker)? + start_marker.len();
    let rest = &source[start..];
    let end = rest.find(end_marker)?;
    Some(&rest[..end])
}
