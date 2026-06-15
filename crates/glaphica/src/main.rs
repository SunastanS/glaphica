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
            "--help" | "-h" => {
                return Err(invalid_arg(
                    "usage: glaphica [--record-input | --replay-input] [--trace-path <file>]",
                ));
            }
            _ => return Err(invalid_arg(format!("unknown argument {arg}"))),
        }
    }

    config.trace_config = match trace_mode {
        TraceModeArg::Disabled => glaphica::AppTraceConfig::Disabled,
        TraceModeArg::Record => glaphica::AppTraceConfig::record(trace_path),
        TraceModeArg::Replay => glaphica::AppTraceConfig::replay(trace_path),
    };
    Ok(config)
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
    }

    #[test]
    fn rejects_unknown_arguments() {
        let error = config_from_args(["--bogus".to_owned()]).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}
