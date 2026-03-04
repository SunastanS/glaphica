#[cfg(test)]
mod tests {
    use glaphica_core::{TileKey, IMAGE_TILE_SIZE};
    use images::{layout::ImageLayout, Image};

    #[test]
    fn tile_canvas_origin_maps_correctly() {
        // Create a 1024x1024 image with 62x62 pixel tiles
        let layout = ImageLayout::new(1024, 1024);
        let image = Image::new(layout, glaphica_core::BackendId::new(0)).unwrap();

        // Tile at row 0, col 0 should be at canvas origin (0, 0)
        let origin = image.tile_canvas_origin(0).unwrap();
        assert_eq!(origin.x, 0.0);
        assert_eq!(origin.y, 0.0);

        // Tile at row 0, col 1 should be at (62, 0)
        let origin = image.tile_canvas_origin(1).unwrap();
        assert_eq!(origin.x, IMAGE_TILE_SIZE as f32);
        assert_eq!(origin.y, 0.0);

        // Tile at row 1, col 0 should be at (0, 62)
        let tiles_per_row = layout.tile_x() as usize;
        let origin = image.tile_canvas_origin(tiles_per_row).unwrap();
        assert_eq!(origin.x, 0.0);
        assert_eq!(origin.y, IMAGE_TILE_SIZE as f32);

        // Tile at row 7, col 7 (middle-ish) should be at (434, 434)
        let tile_index = 7 * tiles_per_row + 7;
        let origin = image.tile_canvas_origin(tile_index).unwrap();
        assert_eq!(origin.x, 7.0 * IMAGE_TILE_SIZE as f32);
        assert_eq!(origin.y, 7.0 * IMAGE_TILE_SIZE as f32);
    }

    #[test]
    fn pixel_to_tile_index_is_correct() {
        let layout = ImageLayout::new(1024, 1024);

        // Pixel at (0, 0) should be in tile 0
        let index = layout.pixel_to_index(0, 0).unwrap();
        assert_eq!(index, 0);

        // Pixel at (62, 0) should be in tile 1
        let index = layout.pixel_to_index(IMAGE_TILE_SIZE, 0).unwrap();
        assert_eq!(index, 1);

        // Pixel at (0, 62) should be in tile 17 (second row)
        let tiles_per_row = layout.tile_x() as usize;
        let index = layout.pixel_to_index(0, IMAGE_TILE_SIZE).unwrap();
        assert_eq!(index, tiles_per_row);

        // Pixel in the middle (500, 500) should be in correct tile
        let x = 500;
        let y = 500;
        let expected_tile_x = x / IMAGE_TILE_SIZE;
        let expected_tile_y = y / IMAGE_TILE_SIZE;
        let expected_index = (expected_tile_y * layout.tile_x() + expected_tile_x) as usize;

        let index = layout.pixel_to_index(x, y).unwrap();
        assert_eq!(index, expected_index);
    }
}
