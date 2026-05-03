use std::error::Error;
use std::fmt::{Display, Formatter};

use glaphica_core::IMAGE_TILE_SIZE;
use serde::{Deserialize, Serialize};

use crate::{PixelTileSource, TileGrid, layout::GlaImageLayout};

const RGBA_BYTES_PER_PIXEL: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlaStoredImageError {
    InvalidPixelCount { expected: usize, actual: usize },
    TooLarge,
    TileOutOfBounds,
}

impl Display for GlaStoredImageError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPixelCount { expected, actual } => write!(
                f,
                "stored image pixel count mismatch: expected {expected} bytes, got {actual}"
            ),
            Self::TooLarge => write!(f, "stored image dimensions are too large"),
            Self::TileOutOfBounds => write!(f, "stored image tile index is out of bounds"),
        }
    }
}

impl Error for GlaStoredImageError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "GlaStoredImageRaw", into = "GlaStoredImageRaw")]
pub struct GlaStoredImage {
    width: u32,
    height: u32,
    pixels_rgba8: Vec<u8>,
    #[serde(skip)]
    layout: GlaImageLayout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct GlaStoredImageRaw {
    width: u32,
    height: u32,
    pixels_rgba8: Vec<u8>,
}

impl From<GlaStoredImage> for GlaStoredImageRaw {
    fn from(image: GlaStoredImage) -> Self {
        Self {
            width: image.width,
            height: image.height,
            pixels_rgba8: image.pixels_rgba8,
        }
    }
}

impl TryFrom<GlaStoredImageRaw> for GlaStoredImage {
    type Error = GlaStoredImageError;

    fn try_from(raw: GlaStoredImageRaw) -> Result<Self, Self::Error> {
        GlaStoredImage::new_rgba8(raw.width, raw.height, raw.pixels_rgba8)
    }
}

impl GlaStoredImage {
    pub fn new_rgba8(
        width: u32,
        height: u32,
        pixels_rgba8: Vec<u8>,
    ) -> Result<Self, GlaStoredImageError> {
        let expected = expected_rgba8_len(width, height)?;
        if pixels_rgba8.len() != expected {
            return Err(GlaStoredImageError::InvalidPixelCount {
                expected,
                actual: pixels_rgba8.len(),
            });
        }

        let layout = GlaImageLayout::new(width, height);
        Ok(Self {
            width,
            height,
            pixels_rgba8,
            layout,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn layout(&self) -> GlaImageLayout {
        GlaImageLayout::new(self.width, self.height)
    }

    pub fn pixels_rgba8(&self) -> &[u8] {
        &self.pixels_rgba8
    }

    pub fn collect_non_empty_slot_indices(&self, output: &mut Vec<usize>) {
        output.clear();
        for tile_index in 0..self.layout.total_slots() as usize {
            if self.tile_has_non_zero_pixel(tile_index) {
                output.push(tile_index);
            }
        }
    }

    pub fn copy_tile_rgba8(
        &self,
        tile_index: usize,
        output: &mut Vec<u8>,
    ) -> Result<(), GlaStoredImageError> {
        let tile_origin = self
            .layout
            .tile_canvas_origin(tile_index)
            .ok_or(GlaStoredImageError::TileOutOfBounds)?;
        let tile_width = IMAGE_TILE_SIZE as usize;
        let tile_len = tile_width * tile_width * RGBA_BYTES_PER_PIXEL;
        output.clear();
        output.resize(tile_len, 0);

        let image_width = self.width as usize;
        let image_height = self.height as usize;
        let origin_x = tile_origin.x as usize;
        let origin_y = tile_origin.y as usize;
        let copy_width = image_width.saturating_sub(origin_x).min(tile_width);

        for row in 0..tile_width {
            let src_y = origin_y + row;
            if src_y >= image_height || copy_width == 0 {
                break;
            }

            let src_start = (src_y * image_width + origin_x) * RGBA_BYTES_PER_PIXEL;
            let src_end = src_start + copy_width * RGBA_BYTES_PER_PIXEL;
            let dst_start = row * tile_width * RGBA_BYTES_PER_PIXEL;
            let dst_end = dst_start + copy_width * RGBA_BYTES_PER_PIXEL;
            output[dst_start..dst_end].copy_from_slice(&self.pixels_rgba8[src_start..src_end]);
        }

        Ok(())
    }

    fn tile_has_non_zero_pixel(&self, tile_index: usize) -> bool {
        let Some(tile_origin) = self.layout.tile_canvas_origin(tile_index) else {
            return false;
        };

        let tile_size = IMAGE_TILE_SIZE as usize;
        let image_width = self.width as usize;
        let image_height = self.height as usize;
        let origin_x = tile_origin.x as usize;
        let origin_y = tile_origin.y as usize;
        let max_x = (origin_x + tile_size).min(image_width);
        let max_y = (origin_y + tile_size).min(image_height);

        for y in origin_y..max_y {
            let row_start = (y * image_width + origin_x) * RGBA_BYTES_PER_PIXEL;
            let row_end = (y * image_width + max_x) * RGBA_BYTES_PER_PIXEL;
            if self.pixels_rgba8[row_start..row_end]
                .iter()
                .any(|channel| *channel != 0)
            {
                return true;
            }
        }

        false
    }
}

impl TileGrid for GlaStoredImage {
    fn layout(&self) -> GlaImageLayout {
        GlaStoredImage::layout(self)
    }

    fn slot_count(&self) -> usize {
        self.layout.total_slots() as usize
    }
}

impl PixelTileSource for GlaStoredImage {
    type Error = GlaStoredImageError;

    fn copy_tile_rgba8(&self, tile_index: usize, output: &mut Vec<u8>) -> Result<(), Self::Error> {
        GlaStoredImage::copy_tile_rgba8(self, tile_index, output)
    }
}

fn expected_rgba8_len(width: u32, height: u32) -> Result<usize, GlaStoredImageError> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(GlaStoredImageError::TooLarge)?;
    let bytes = pixels
        .checked_mul(RGBA_BYTES_PER_PIXEL as u64)
        .ok_or(GlaStoredImageError::TooLarge)?;
    usize::try_from(bytes).map_err(|_| GlaStoredImageError::TooLarge)
}

#[cfg(test)]
mod tests {
    use glaphica_core::IMAGE_TILE_SIZE;

    use crate::{GlaImageLayout, TileGrid};

    use super::{GlaStoredImage, GlaStoredImageError};

    #[test]
    fn tile_grid_reports_logical_tiles() {
        let image = GlaStoredImage::new_rgba8(2, 2, vec![0; 16]).unwrap();
        assert_eq!(image.slot_count(), 1);
        assert_eq!(TileGrid::layout(&image), GlaImageLayout::new(2, 2));
    }

    #[test]
    fn rejects_invalid_rgba8_len() {
        let image = GlaStoredImage::new_rgba8(2, 2, vec![0; 15]);
        assert_eq!(
            image,
            Err(GlaStoredImageError::InvalidPixelCount {
                expected: 16,
                actual: 15,
            })
        );
    }

    #[test]
    fn serde_round_trip_rebuilds_layout() {
        let width = IMAGE_TILE_SIZE + 1;
        let height = IMAGE_TILE_SIZE + 1;
        let image =
            GlaStoredImage::new_rgba8(width, height, vec![0; (width * height * 4) as usize])
                .unwrap();

        let json = serde_json::to_string(&image).unwrap();
        let decoded: GlaStoredImage = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.layout(), GlaImageLayout::new(width, height));
        assert_eq!(decoded.slot_count(), 4);
    }

    #[test]
    fn serde_does_not_emit_cached_layout() {
        let image = GlaStoredImage::new_rgba8(2, 2, vec![0; 16]).unwrap();
        let json = serde_json::to_string(&image).unwrap();

        assert!(!json.contains("layout"));
    }

    #[test]
    fn deserialize_rejects_invalid_pixel_count() {
        let decoded = serde_json::from_str::<GlaStoredImage>(
            r#"{"width":2,"height":2,"pixels_rgba8":[0,0,0]}"#,
        );

        assert!(matches!(
            decoded,
            Err(error) if error.to_string().contains("stored image pixel count mismatch")
        ));
    }

    #[test]
    fn collects_non_empty_slots_from_rgba_content() {
        let width = IMAGE_TILE_SIZE + 4;
        let height = IMAGE_TILE_SIZE;
        let mut pixels = vec![0; (width * height * 4) as usize];
        pixels[(IMAGE_TILE_SIZE as usize - 1) * 4] = 1;
        pixels[(IMAGE_TILE_SIZE as usize) * 4] = 2;
        let image = GlaStoredImage::new_rgba8(width, height, pixels).unwrap();

        let mut indices = Vec::new();
        image.collect_non_empty_slot_indices(&mut indices);

        assert_eq!(indices, vec![0, 1]);
    }

    #[test]
    fn copies_partial_edge_tile_into_fixed_tile_buffer() {
        let width = IMAGE_TILE_SIZE + 1;
        let height = 1;
        let mut pixels = vec![0; (width * height * 4) as usize];
        let edge_pixel_offset = (IMAGE_TILE_SIZE as usize) * 4;
        pixels[edge_pixel_offset..edge_pixel_offset + 4].copy_from_slice(&[9, 8, 7, 6]);
        let image = GlaStoredImage::new_rgba8(width, height, pixels).unwrap();

        let mut tile = Vec::new();
        image.copy_tile_rgba8(1, &mut tile).unwrap();

        assert_eq!(&tile[..4], &[9, 8, 7, 6]);
        assert!(tile[4..].iter().all(|value| *value == 0));
    }

    #[test]
    fn rejects_out_of_bounds_tile_copy() {
        let image = GlaStoredImage::new_rgba8(1, 1, vec![0; 4]).unwrap();
        let mut tile = Vec::new();

        let copied = image.copy_tile_rgba8(1, &mut tile);

        assert_eq!(copied, Err(GlaStoredImageError::TileOutOfBounds));
    }
}
