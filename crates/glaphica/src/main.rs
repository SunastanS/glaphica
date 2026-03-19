mod brush_ui;
mod components;
mod desktop_app;
mod egui_renderer;
mod input;
mod overlay;
mod run_config;
mod theme;

use desktop_app::run_app;
use run_config::RunConfig;

fn main() {
    let run_config = RunConfig::from_args(std::env::args().skip(1).collect());
    run_app(run_config);
}
