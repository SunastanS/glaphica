use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct RunConfig {
    pub replay_input_path: Option<PathBuf>,
    pub record_input_path: Option<PathBuf>,
    pub record_output_path: Option<PathBuf>,
    pub screenshot_path: Option<PathBuf>,
    pub frontend_screenshot_jpg_path: Option<PathBuf>,
    pub document_bundle_path: Option<PathBuf>,
    pub exit_after_ms: Option<u64>,
}

impl RunConfig {
    pub fn from_args(args: Vec<String>) -> Self {
        let mut config = Self::default();
        let project_root = resolve_project_root()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));

        let resolve_path = |raw: &str| resolve_cli_path(raw, &project_root);
        let mut index = 0usize;
        while index < args.len() {
            match args[index].as_str() {
                "--replay-input" => {
                    if let Some(path) = args.get(index + 1) {
                        config.replay_input_path = Some(resolve_path(path));
                    }
                    index += 2;
                }
                "--record-input" => {
                    if let Some(path) = args.get(index + 1) {
                        config.record_input_path = Some(resolve_path(path));
                    }
                    index += 2;
                }
                "--record-output" => {
                    if let Some(path) = args.get(index + 1) {
                        config.record_output_path = Some(resolve_path(path));
                    }
                    index += 2;
                }
                "--screenshot" => {
                    if let Some(path) = args.get(index + 1) {
                        config.screenshot_path = Some(resolve_path(path));
                    }
                    index += 2;
                }
                "--frontend-screenshot-jpg" => {
                    if let Some(path) = args.get(index + 1) {
                        config.frontend_screenshot_jpg_path = Some(resolve_path(path));
                    }
                    index += 2;
                }
                "--document-bundle" => {
                    if let Some(path) = args.get(index + 1) {
                        config.document_bundle_path = Some(resolve_path(path));
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

fn resolve_project_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    find_workspace_root(&cwd)
}

fn find_workspace_root(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        let manifest = ancestor.join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        if text.contains("[workspace]") {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

fn resolve_cli_path(raw: &str, project_root: &Path) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        return path;
    }
    project_root.join(path)
}

#[cfg(test)]
mod tests {
    use super::{find_workspace_root, resolve_cli_path};
    use std::path::Path;

    #[test]
    fn resolve_cli_path_keeps_absolute_paths() {
        let root = Path::new("/tmp/project-root");
        let absolute = Path::new("/tmp/records/input.json");
        let raw = absolute.to_string_lossy().to_string();
        assert_eq!(resolve_cli_path(&raw, root), absolute);
    }

    #[test]
    fn resolve_cli_path_uses_workspace_root_for_relative_paths() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = find_workspace_root(manifest_dir).expect("workspace root");
        assert_eq!(
            resolve_cli_path("test/records/input.json", &workspace_root),
            workspace_root.join("test/records/input.json")
        );
    }
}
