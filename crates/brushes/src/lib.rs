pub type BrushPipelineError = Box<dyn std::error::Error + Send + Sync + 'static>;

mod brush_registry;

pub mod engine_runtime;
pub mod gpu_runtime;

pub use brush_registry::BrushRegistryError;
pub use engine_runtime::{BrushEngineRuntime, EngineBrushDispatchError, EngineBrushPipeline};
pub use gpu_runtime::{
    BrushGpuApplyOutcome, BrushGpuDispatchError, BrushGpuPipeline, BrushGpuRuntime,
};
