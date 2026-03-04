pub mod config;
pub mod input_processor;
pub mod resampler;
pub mod smoother;

pub use config::{CURVATURE_WINDOW_SIZE, HISTORY_CAPACITY, VELOCITY_WINDOW_SIZE};
pub use input_processor::{InputProcessingConfig, StrokeInputProcessor};
pub use resampler::ResamplerConfig;
pub use smoother::{ExponentialMovingAverageConfig, SmoothingStrategy};
