pub mod config;
pub mod trace;
mod main_thread;
mod engine_thread;
mod integration;
mod screen_blitter;

#[cfg(test)]
mod screen_blitter_test;

pub use main_thread::{
    MainThreadState, InitError, BrushRegisterError, PresentError, ScreenshotError,
};
pub use engine_thread::EngineThreadState;
pub use integration::{AppThreadIntegration, StrokeControl, TileAllocReceipt, GpuError};
