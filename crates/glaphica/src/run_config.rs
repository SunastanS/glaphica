use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct RunConfig {
    pub replay_input_path: Option<PathBuf>,
    pub record_input_path: Option<PathBuf>,
    pub record_output_path: Option<PathBuf>,
    pub screenshot_path: Option<PathBuf>,
    pub document_bundle_path: Option<PathBuf>,
    pub exit_after_ms: Option<u64>,
}

impl RunConfig {
    pub fn from_args(args: Vec<String>) -> Self {
        let mut config = Self::default();
        let mut index = 0usize;
        while index < args.len() {
            match args[index].as_str() {
                "--replay-input" => {
                    if let Some(path) = args.get(index + 1) {
                        config.replay_input_path = Some(Path::new(path).to_path_buf());
                    }
                    index += 2;
                }
                "--record-input" => {
                    if let Some(path) = args.get(index + 1) {
                        config.record_input_path = Some(Path::new(path).to_path_buf());
                    }
                    index += 2;
                }
                "--record-output" => {
                    if let Some(path) = args.get(index + 1) {
                        config.record_output_path = Some(Path::new(path).to_path_buf());
                    }
                    index += 2;
                }
                "--screenshot" => {
                    if let Some(path) = args.get(index + 1) {
                        config.screenshot_path = Some(Path::new(path).to_path_buf());
                    }
                    index += 2;
                }
                "--document-bundle" => {
                    if let Some(path) = args.get(index + 1) {
                        config.document_bundle_path = Some(Path::new(path).to_path_buf());
                    }
                    index += 2;
                }
                "--exit-after-ms" => {
                    if let Some(value) = args.get(index + 1) {
                        if let Ok(ms) = value.parse::<u64>() {
                            config.exit_after_ms = Some(ms);
                        }
                    }
                    index += 2;
                }
                _ => {
                    index += 1;
                }
            }
        }
        config
    }
}
