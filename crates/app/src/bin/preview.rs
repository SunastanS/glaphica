fn main() {
    if let Err(error) = app::run_preview_window() {
        eprintln!("preview failed: {error}");
    }
}
