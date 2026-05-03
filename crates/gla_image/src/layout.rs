use glaphica_core::{CanvasVec2, GUTTER_SIZE, IMAGE_TILE_SIZE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlaImageLayout {
    size_x: u32,
    size_y: u32,
    slot_x: u32,
    slot_y: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlaImageLayoutError {
    OutOfBounds,
}

impl GlaImageLayout {
    pub fn new(size_x: u32, size_y: u32) -> Self {
        let tile_x = size_x.div_ceil(IMAGE_TILE_SIZE);
        let tile_y = size_y.div_ceil(IMAGE_TILE_SIZE);
        Self {
            size_x,
            size_y,
            slot_x: tile_x,
            slot_y: tile_y,
        }
    }

    pub const fn size_x(&self) -> u32 {
        self.size_x
    }

    pub const fn size_y(&self) -> u32 {
        self.size_y
    }

    pub const fn slot_x(&self) -> u32 {
        self.slot_x
    }

    pub const fn slot_y(&self) -> u32 {
        self.slot_y
    }

    pub const fn total_slots(&self) -> u32 {
        self.slot_x * self.slot_y
    }

    pub fn pixel_to_index(&self, x: u32, y: u32) -> Result<usize, GlaImageLayoutError> {
        if x >= self.size_x || y >= self.size_y {
            return Err(GlaImageLayoutError::OutOfBounds);
        }
        let tile_x = x / IMAGE_TILE_SIZE;
        let tile_y = y / IMAGE_TILE_SIZE;
        self.tile_coords_to_index(tile_x, tile_y)
            .ok_or(GlaImageLayoutError::OutOfBounds)
    }

    pub fn collect_affected_tile_indices(
        &self,
        center: CanvasVec2,
        max_affected_radius_px: u32,
        output: &mut Vec<usize>,
    ) {
        output.clear();
        self.for_each_affected_tile_index(center, max_affected_radius_px, |index| {
            output.push(index);
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
        let Some((min_tile_x, max_tile_x, min_tile_y, max_tile_y)) =
            self.affected_tile_range(center, max_affected_radius_px)
        else {
            return;
        };

        for tile_y in min_tile_y..=max_tile_y {
            for tile_x in min_tile_x..=max_tile_x {
                if let Some(index) = self.tile_coords_to_index(tile_x, tile_y) {
                    visit(index);
                }
            }
        }
    }

    pub fn tile_canvas_origin(&self, tile_index: usize) -> Option<CanvasVec2> {
        let tile_x = self.slot_x as usize;
        if tile_index >= tile_x * self.slot_y as usize {
            return None;
        }

        let tile_coord_x = (tile_index % tile_x) as u32;
        let tile_coord_y = (tile_index / tile_x) as u32;
        Some(CanvasVec2::new(
            tile_coord_x as f32 * IMAGE_TILE_SIZE as f32,
            tile_coord_y as f32 * IMAGE_TILE_SIZE as f32,
        ))
    }

    fn tile_coords_to_index(&self, tile_x: u32, tile_y: u32) -> Option<usize> {
        let index = tile_y.checked_mul(self.slot_x)?.checked_add(tile_x)?;
        usize::try_from(index).ok()
    }

    fn affected_tile_range(
        &self,
        center: CanvasVec2,
        max_affected_radius_px: u32,
    ) -> Option<(u32, u32, u32, u32)> {
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

        Some((
            clamped_min_x / IMAGE_TILE_SIZE,
            clamped_max_x / IMAGE_TILE_SIZE,
            clamped_min_y / IMAGE_TILE_SIZE,
            clamped_max_y / IMAGE_TILE_SIZE,
        ))
    }
}

#[cfg(test)]
mod tests {
    use glaphica_core::{CanvasVec2, IMAGE_TILE_SIZE};

    use super::{GlaImageLayout, GlaImageLayoutError};

    #[test]
    fn pixel_to_index_maps_pixels_to_tile_indices() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE * 2);
        let index = layout.pixel_to_index(IMAGE_TILE_SIZE + 1, 0);
        assert_eq!(index, Ok(1usize));
    }

    #[test]
    fn pixel_to_index_rejects_out_of_bounds_pixels() {
        let layout = GlaImageLayout::new(32, 16);
        let index = layout.pixel_to_index(99, 0);
        assert_eq!(index, Err(GlaImageLayoutError::OutOfBounds));
    }

    #[test]
    fn affected_tiles_include_one_gutter_even_with_zero_radius() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE);
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
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE);
        let mut indices = vec![77usize];

        layout.collect_affected_tile_indices(CanvasVec2::new(-1000.0, -1000.0), 5, &mut indices);

        assert!(indices.is_empty());
    }

    #[test]
    fn affected_tiles_cover_full_clamped_span() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE * 3, IMAGE_TILE_SIZE * 2);
        let mut indices = Vec::new();

        layout.collect_affected_tile_indices(
            CanvasVec2::new(IMAGE_TILE_SIZE as f32, IMAGE_TILE_SIZE as f32),
            IMAGE_TILE_SIZE,
            &mut indices,
        );

        assert_eq!(indices, vec![0, 1, 2, 3, 4, 5]);
    }
}
