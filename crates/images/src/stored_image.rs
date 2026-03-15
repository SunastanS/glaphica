use std::error::Error;
use std::fmt::{Display, Formatter};

use glaphica_core::IMAGE_TILE_SIZE;
use serde::{Deserialize, Serialize};

use crate::layout::ImageLayout;

const RGBA_BYTES_PER_PIXEL: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredImageError {
    InvalidPixelCount { expected: usize, actual: usize },
    TooLarge,
    TileOutOfBounds,
}

impl Display for StoredImageError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPixelCount { expected, actual } => {
                write!(
                    f,
                    "stored image pixel count mismatch: expected {expected} bytes, got {actual}"
                )
            }
            Self::TooLarge => write!(f, "stored image dimensions are too large"),
            Self::TileOutOfBounds => write!(f, "stored image tile index is out of bounds"),
        }
    }
}

impl Error for StoredImageError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredImage {
    width: u32,
    height: u32,
    pixels_rgba8: Vec<u8>,
}

impl StoredImage {
    pub fn new_rgba8(
        width: u32,
        height: u32,
        pixels_rgba8: Vec<u8>,
    ) -> Result<Self, StoredImageError> {
        let expected = expected_rgba8_len(width, height)?;
        if pixels_rgba8.len() != expected {
            return Err(StoredImageError::InvalidPixelCount {
                expected,
                actual: pixels_rgba8.len(),
            });
        }
        Ok(Self {
            width,
            height,
            pixels_rgba8,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn layout(&self) -> ImageLayout {
        ImageLayout::new(self.width, self.height)
    }

    pub fn pixels_rgba8(&self) -> &[u8] {
        &self.pixels_rgba8
    }

    pub fn collect_non_empty_tile_indices(&self, output: &mut Vec<usize>) {
        output.clear();
        let layout = self.layout();
        for tile_index in 0..layout.total_tiles() as usize {
            if self.tile_has_non_zero_pixel(tile_index) {
                output.push(tile_index);
            }
        }
    }

    pub fn copy_tile_rgba8(
        &self,
        tile_index: usize,
        output: &mut Vec<u8>,
    ) -> Result<(), StoredImageError> {
        let layout = self.layout();
        let tile_origin = layout
            .tile_canvas_origin(tile_index)
            .ok_or(StoredImageError::TileOutOfBounds)?;
        let tile_width = IMAGE_TILE_SIZE as usize;
        let tile_len = tile_width * tile_width * RGBA_BYTES_PER_PIXEL;
        output.clear();
        output.resize(tile_len, 0);

        let image_width = self.width as usize;
        let image_height = self.height as usize;
        let origin_x = tile_origin.x as usize;
        let origin_y = tile_origin.y as usize;

        for row in 0..tile_width {
            let src_y = origin_y + row;
            if src_y >= image_height {
                break;
            }
            let copy_width = image_width.saturating_sub(origin_x).min(tile_width);
            if copy_width == 0 {
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
        let layout = self.layout();
        let Some(tile_origin) = layout.tile_canvas_origin(tile_index) else {
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

fn expected_rgba8_len(width: u32, height: u32) -> Result<usize, StoredImageError> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(StoredImageError::TooLarge)?;
    let bytes = pixels
        .checked_mul(RGBA_BYTES_PER_PIXEL as u64)
        .ok_or(StoredImageError::TooLarge)?;
    usize::try_from(bytes).map_err(|_| StoredImageError::TooLarge)
}

#[cfg(test)]
mod tests {
    use glaphica_core::IMAGE_TILE_SIZE;

    use super::{StoredImage, StoredImageError};

    #[test]
    fn rejects_invalid_rgba8_len() {
        let image = StoredImage::new_rgba8(2, 2, vec![0; 15]);
        assert_eq!(
            image,
            Err(StoredImageError::InvalidPixelCount {
                expected: 16,
                actual: 15,
            })
        );
    }

    #[test]
    fn collects_non_empty_tiles_from_rgba_content() {
        let width = IMAGE_TILE_SIZE + 4;
        let height = IMAGE_TILE_SIZE;
        let mut pixels = vec![0; (width * height * 4) as usize];
        pixels[(IMAGE_TILE_SIZE as usize - 1) * 4] = 1;
        pixels[(IMAGE_TILE_SIZE as usize) * 4] = 2;
        let image = StoredImage::new_rgba8(width, height, pixels).unwrap();

        let mut indices = Vec::new();
        image.collect_non_empty_tile_indices(&mut indices);

        assert_eq!(indices, vec![0, 1]);
    }

    #[test]
    fn copies_partial_edge_tile_into_fixed_tile_buffer() {
        let width = IMAGE_TILE_SIZE + 1;
        let height = 1;
        let mut pixels = vec![0; (width * height * 4) as usize];
        let edge_pixel_offset = (IMAGE_TILE_SIZE as usize) * 4;
        pixels[edge_pixel_offset..edge_pixel_offset + 4].copy_from_slice(&[9, 8, 7, 6]);
        let image = StoredImage::new_rgba8(width, height, pixels).unwrap();

        let mut tile = Vec::new();
        image.copy_tile_rgba8(1, &mut tile).unwrap();

        assert_eq!(&tile[..4], &[9, 8, 7, 6]);
        assert!(tile[4..].iter().all(|value| *value == 0));
    }
}
