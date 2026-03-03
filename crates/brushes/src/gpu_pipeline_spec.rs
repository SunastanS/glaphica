#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrushGpuPipelineSpec {
    pub label: &'static str,
    pub wgsl_source: &'static str,
    pub vertex_entry: &'static str,
    pub fragment_entry: &'static str,
    pub uses_brush_cache_backend: bool,
}
