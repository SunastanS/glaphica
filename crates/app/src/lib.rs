pub mod config;
mod engine_thread;
mod integration;
mod main_thread;
mod screen_blitter;
pub mod trace;

#[cfg(test)]
mod screen_blitter_test;

pub use engine_thread::EngineThreadState;
pub use integration::{AppControl, AppThreadIntegration, GpuError, TileAllocReceipt};
pub use main_thread::{
    BrushRegisterError, InitError, MainThreadState, PresentError, ScreenshotError,
};
