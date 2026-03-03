use glaphica_core::{CanvasVec2, GUTTER_SIZE, IMAGE_TILE_SIZE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageLayout {
    size_x: u32,
    size_y: u32,
    tile_x: u32,
    tile_y: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageLayoutError {
    OutOfBounds,
}

impl ImageLayout {
    pub fn new(size_x: u32, size_y: u32) -> Self {
        let tile_x = size_x.div_ceil(IMAGE_TILE_SIZE);
        let tile_y = size_y.div_ceil(IMAGE_TILE_SIZE);
        Self {
            size_x,
            size_y,
            tile_x,
            tile_y,
        }
    }

    pub const fn size_x(&self) -> u32 {
        self.size_x
    }

    pub const fn size_y(&self) -> u32 {
        self.size_y
    }

    pub const fn tile_x(&self) -> u32 {
        self.tile_x
    }

    pub const fn tile_y(&self) -> u32 {
        self.tile_y
    }

    pub const fn total_tiles(&self) -> u32 {
        self.tile_x * self.tile_y
    }

    pub fn pixel_to_index(&self, x: u32, y: u32) -> Result<usize, ImageLayoutError> {
        if x >= self.size_x || y >= self.size_y {
            return Err(ImageLayoutError::OutOfBounds);
        }
        let tile_x = x / IMAGE_TILE_SIZE;
        let tile_y = y / IMAGE_TILE_SIZE;
        self.tile_coords_to_index(tile_x, tile_y)
            .ok_or(ImageLayoutError::OutOfBounds)
    }

    pub fn collect_affected_tile_indices(
        &self,
        center: CanvasVec2,
        max_affected_radius_px: u32,
        output: &mut Vec<usize>,
    ) {
        output.clear();
        self.for_each_affected_tile_index(center, max_affected_radius_px, |index| {
            output.push(index)
        });
    }

    pub fn for_each_affected_tile_index<F>(
        &self,
        center: CanvasVec2,
        max_affected_radius_px: u32,
        mut visit: F,
    ) where
        F: FnMut(usize),
    {
        let Some(bounds) = self.affected_tile_bounds(center, max_affected_radius_px) else {
            return;
        };

        for tile_y in bounds.min_tile_y..=bounds.max_tile_y {
            for tile_x in bounds.min_tile_x..=bounds.max_tile_x {
                if let Some(index) = self.tile_coords_to_index(tile_x, tile_y) {
                    visit(index);
                }
            }
        }
    }

    fn tile_coords_to_index(&self, tile_x: u32, tile_y: u32) -> Option<usize> {
        let index = tile_y.checked_mul(self.tile_x)?.checked_add(tile_x)?;
        usize::try_from(index).ok()
    }

    fn affected_tile_bounds(
        &self,
        center: CanvasVec2,
        max_affected_radius_px: u32,
    ) -> Option<AffectedTileBounds> {
        if self.size_x == 0 || self.size_y == 0 {
            return None;
        }
        if !center.x.is_finite() || !center.y.is_finite() {
            return None;
        }

        let effective_radius_px = max_affected_radius_px.saturating_add(GUTTER_SIZE) as f32;
        let min_x = (center.x - effective_radius_px).floor() as i64;
        let max_x = (center.x + effective_radius_px).floor() as i64;
        let min_y = (center.y - effective_radius_px).floor() as i64;
        let max_y = (center.y + effective_radius_px).floor() as i64;

        let max_pixel_x = i64::from(self.size_x.saturating_sub(1));
        let max_pixel_y = i64::from(self.size_y.saturating_sub(1));

        if max_x < 0 || max_y < 0 || min_x > max_pixel_x || min_y > max_pixel_y {
            return None;
        }

        let clamped_min_x = min_x.clamp(0, max_pixel_x) as u32;
        let clamped_max_x = max_x.clamp(0, max_pixel_x) as u32;
        let clamped_min_y = min_y.clamp(0, max_pixel_y) as u32;
        let clamped_max_y = max_y.clamp(0, max_pixel_y) as u32;

        Some(AffectedTileBounds {
            min_tile_x: clamped_min_x / IMAGE_TILE_SIZE,
            max_tile_x: clamped_max_x / IMAGE_TILE_SIZE,
            min_tile_y: clamped_min_y / IMAGE_TILE_SIZE,
            max_tile_y: clamped_max_y / IMAGE_TILE_SIZE,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AffectedTileBounds {
    min_tile_x: u32,
    max_tile_x: u32,
    min_tile_y: u32,
    max_tile_y: u32,
}

#[cfg(test)]
mod tests {
    use glaphica_core::{CanvasVec2, IMAGE_TILE_SIZE};

    use super::{ImageLayout, ImageLayoutError};

    #[test]
    fn pixel_to_index_maps_pixels_to_tile_indices() {
        let layout = ImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE * 2);
        let index = layout.pixel_to_index(IMAGE_TILE_SIZE + 1, 0);
        assert_eq!(index, Ok(1usize));
    }

    #[test]
    fn pixel_to_index_rejects_out_of_bounds_pixels() {
        let layout = ImageLayout::new(32, 16);
        let index = layout.pixel_to_index(99, 0);
        assert_eq!(index, Err(ImageLayoutError::OutOfBounds));
    }

    #[test]
    fn affected_tiles_include_one_gutter_even_with_zero_radius() {
        let layout = ImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE);
        let mut indices = Vec::new();

        layout.collect_affected_tile_indices(
            CanvasVec2::new(IMAGE_TILE_SIZE as f32, 10.0),
            0,
            &mut indices,
        );

        assert_eq!(indices, vec![0, 1]);
    }

    #[test]
    fn affected_tiles_are_empty_when_center_is_outside_image() {
        let layout = ImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE);
        let mut indices = vec![77usize];

        layout.collect_affected_tile_indices(CanvasVec2::new(-1000.0, -1000.0), 5, &mut indices);

        assert!(indices.is_empty());
    }
}
