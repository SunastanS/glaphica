/// Thread channel capacities for main thread and engine thread communication
pub mod thread_channels {
    /// Capacity of the main-to-engine input ring buffer
    pub const MAIN_TO_ENGINE_INPUT_RING: usize = 256;

    /// Capacity of the engine-to-main input control queue
    pub const ENGINE_TO_MAIN_INPUT_CONTROL: usize = 64;

    /// Capacity of the engine-to-main GPU command channel
    pub const ENGINE_TO_MAIN_GPU_COMMAND: usize = 1024;

    /// Capacity of the main-to-engine feedback channel
    pub const MAIN_TO_ENGINE_FEEDBACK: usize = 256;
}

/// Vector pre-allocation capacities for batch processing
pub mod batch_capacities {
    /// Pre-allocated capacity for input samples vector
    pub const INPUT_SAMPLES: usize = 256;

    /// Pre-allocated capacity for brush inputs vector
    pub const BRUSH_INPUTS: usize = 64;

    /// Pre-allocated capacity for GPU commands vector
    pub const GPU_COMMANDS: usize = 64;
}

/// Brush processing configuration
pub mod brush_processing {
    /// Maximum number of brushes that can be registered
    pub const MAX_BRUSHES: usize = 16;

    /// Maximum batch size for draining input samples with wait
    pub const MAX_INPUT_BATCH_SIZE: usize = 256;
}

/// Atlas storage configuration
pub mod atlas_storage {
    /// Initial capacity for atlas backend storage
    pub const INITIAL_BACKEND_CAPACITY: usize = 2;
}

/// Registry capacities for brush-related registries
pub mod registry_capacities {
    /// Capacity for brush layout registry
    pub const BRUSH_LAYOUT_REGISTRY: usize = 16;

    /// Capacity for brush GPU pipeline registry
    pub const BRUSH_PIPELINE_REGISTRY: usize = 16;
}

/// Stroke input processing configuration
pub mod stroke_input {
    /// Velocity calculation window size (number of samples)
    pub const VELOCITY_WINDOW_SIZE: usize = 3;

    /// Curvature calculation window size (number of samples)
    pub const CURVATURE_WINDOW_SIZE: usize = 5;

    /// History buffer capacity for processed samples
    pub const HISTORY_CAPACITY: usize = 16;
}
