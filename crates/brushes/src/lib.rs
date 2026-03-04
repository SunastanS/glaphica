pub type BrushPipelineError = Box<dyn std::error::Error + Send + Sync + 'static>;

mod brush_registry;

pub mod brush_spec;
pub mod builtin_brushes;
pub mod draw_layout;
pub mod engine_runtime;
pub mod gpu_pipeline_registry;
pub mod gpu_pipeline_spec;
pub mod layout_registry;

pub use brush_registry::{BrushRegistry, BrushRegistryError};
pub use brush_spec::{BrushSpec, BrushSpecRegisterError};
pub use draw_layout::{BrushDrawInputLayout, BrushDrawInputShape, BrushDrawKind};
pub use engine_runtime::{
    BrushEngineRuntime, EngineBrushDispatchError, EngineBrushPipeline, StrokeDrawOutput,
    StrokeTileKey, TileSlotAllocator,
};
pub use gpu_pipeline_registry::BrushGpuPipelineRegistry;
pub use gpu_pipeline_spec::BrushGpuPipelineSpec;
pub use layout_registry::BrushLayoutRegistry;
