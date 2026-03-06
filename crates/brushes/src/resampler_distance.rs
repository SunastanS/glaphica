#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrushResamplerDistance {
    pub min_distance: f32,
    pub max_distance: f32,
}

pub trait BrushResamplerDistancePolicy {
    fn brush_size(&self) -> u32;
    fn max_distance_rate(&self) -> f32;
    fn min_distance_rate(&self) -> f32;

    fn resampler_distance(&self) -> BrushResamplerDistance {
        let brush_size = self.brush_size() as f32;
        let min_distance = (brush_size * self.min_distance_rate()).max(0.0);
        let max_distance = (brush_size * self.max_distance_rate()).max(min_distance);

        BrushResamplerDistance {
            min_distance,
            max_distance,
        }
    }
}
