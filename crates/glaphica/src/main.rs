use std::io::{Error, ErrorKind};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    glaphica::run_app_window_with_config(config_from_args(std::env::args().skip(1))?)?;
    Ok(())
}

fn config_from_args(
    args: impl IntoIterator<Item = String>,
) -> Result<glaphica::AppRuntimeConfig, Error> {
    let mut config = glaphica::AppRuntimeConfig::default();
    config.perf_trace_config = glaphica::AppPerfTraceConfig::from_env();
    let mut trace_path = PathBuf::from("glaphica-trace.json");
    let mut trace_mode = TraceModeArg::Disabled;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--record-input" => trace_mode = TraceModeArg::Record,
            "--replay-input" => trace_mode = TraceModeArg::Replay,
            "--trace-path" => {
                let Some(path) = args.next() else {
                    return Err(invalid_arg("--trace-path requires a file path"));
                };
                trace_path = PathBuf::from(path);
            }
            "--open-workspace" => {
                let Some(path) = args.next() else {
                    return Err(invalid_arg("--open-workspace requires a directory path"));
                };
                config.workspace_path = Some(PathBuf::from(path));
            }
            "--run-command-plan" => {
                let Some(path) = args.next() else {
                    return Err(invalid_arg("--run-command-plan requires a JSON file path"));
                };
                config.startup_command_plan_path = Some(PathBuf::from(path));
            }
            "--exit-after-frames" => {
                let Some(count) = args.next() else {
                    return Err(invalid_arg(
                        "--exit-after-frames requires a positive integer",
                    ));
                };
                config.exit_after_redraw_frames = Some(parse_positive_u64(
                    &count,
                    "--exit-after-frames requires a positive integer",
                )?);
            }
            "--help" | "-h" => {
                return Err(invalid_arg(
                    "usage: glaphica [--record-input | --replay-input] [--trace-path <file>] [--open-workspace <dir>] [--run-command-plan <file>] [--exit-after-frames <n>]",
                ));
            }
            _ => return Err(invalid_arg(format!("unknown argument {arg}"))),
        }
    }

    config.trace_default_path = trace_path.clone();
    config.trace_config = match trace_mode {
        TraceModeArg::Disabled => glaphica::AppTraceConfig::Disabled,
        TraceModeArg::Record => glaphica::AppTraceConfig::record(trace_path),
        TraceModeArg::Replay => glaphica::AppTraceConfig::replay(trace_path),
    };
    Ok(config)
}

fn parse_positive_u64(value: &str, error_message: &'static str) -> Result<u64, Error> {
    let count = value
        .parse::<u64>()
        .map_err(|_| invalid_arg(error_message))?;
    if count == 0 {
        return Err(invalid_arg(error_message));
    }
    Ok(count)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TraceModeArg {
    Disabled,
    Record,
    Replay,
}

fn invalid_arg(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message.into())
}

#[cfg(test)]
mod tests {
    use super::config_from_args;
    use glaphica::AppTraceConfig;
    use std::path::PathBuf;

    #[test]
    fn parses_record_trace_arguments() {
        let config = config_from_args([
            "--record-input".to_owned(),
            "--trace-path".to_owned(),
            "target/trace.json".to_owned(),
        ])
        .unwrap();

        assert_eq!(
            config.trace_config,
            AppTraceConfig::record("target/trace.json")
        );
        assert_eq!(
            config.trace_default_path,
            PathBuf::from("target/trace.json")
        );
    }

    #[test]
    fn parses_open_workspace_argument() {
        let config = config_from_args([
            "--open-workspace".to_owned(),
            "target/workspace-export".to_owned(),
        ])
        .unwrap();

        assert_eq!(
            config.workspace_path,
            Some(PathBuf::from("target/workspace-export"))
        );
    }

    #[test]
    fn parses_run_command_plan_argument() {
        let config = config_from_args([
            "--run-command-plan".to_owned(),
            "target/startup-plan.json".to_owned(),
        ])
        .unwrap();

        assert_eq!(
            config.startup_command_plan_path,
            Some(PathBuf::from("target/startup-plan.json"))
        );
    }

    #[test]
    fn parses_exit_after_frames_argument() {
        let config = config_from_args(["--exit-after-frames".to_owned(), "2".to_owned()]).unwrap();

        assert_eq!(config.exit_after_redraw_frames, Some(2));
    }

    #[test]
    fn rejects_invalid_exit_after_frames_argument() {
        let zero =
            config_from_args(["--exit-after-frames".to_owned(), "0".to_owned()]).unwrap_err();
        let invalid =
            config_from_args(["--exit-after-frames".to_owned(), "nan".to_owned()]).unwrap_err();

        assert_eq!(zero.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(invalid.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn rejects_unknown_arguments() {
        let error = config_from_args(["--bogus".to_owned()]).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}
