use std::error::Error;
use std::fmt::{Display, Formatter};
use tile_key::{Tile, Tiles};

#[derive(Debug)]
pub struct ImageEdit {
    edits: Vec<(u32, Tile)>,
}

impl ImageEdit {
    pub fn new() -> Self {
        Self { edits: Vec::new() }
    }

    pub fn from_sorted_unique(edits: Vec<(u32, Tile)>) -> Result<Self, ImageEditCreateError> {
        for pair in edits.windows(2) {
            if pair[0].0 >= pair[1].0 {
                return Err(ImageEditCreateError { edits });
            }
        }
        Ok(Self { edits })
    }

    pub fn edits(&self) -> &[(u32, Tile)] {
        &self.edits
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    pub fn into_edits(self) -> Vec<(u32, Tile)> {
        self.edits
    }

    pub fn take(&mut self) -> Self {
        Self {
            edits: std::mem::take(&mut self.edits),
        }
    }

    pub fn tile(&self, tile_index: u32) -> Option<&Tile> {
        self.edits
            .binary_search_by_key(&tile_index, |(index, _)| *index)
            .ok()
            .map(|index| &self.edits[index].1)
    }

    pub fn tile_mut(&mut self, tile_index: u32) -> Option<&mut Tile> {
        self.edits
            .binary_search_by_key(&tile_index, |(index, _)| *index)
            .ok()
            .map(|index| &mut self.edits[index].1)
    }

    pub fn insert_tile(&mut self, tile_index: u32, tile: Tile) -> &mut Tile {
        let index = self
            .edits
            .binary_search_by_key(&tile_index, |(index, _)| *index)
            .expect_err("image edit tile must not already exist");
        self.edits.insert(index, (tile_index, tile));
        &mut self.edits[index].1
    }

    pub fn release_tiles(self, tiles: &mut Tiles) {
        for (_, tile) in self.edits {
            tiles.release(tile);
        }
    }
}

impl Default for ImageEdit {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct ImageEditCreateError {
    edits: Vec<(u32, Tile)>,
}

impl ImageEditCreateError {
    pub fn into_edits(self) -> Vec<(u32, Tile)> {
        self.edits
    }
}

impl Display for ImageEditCreateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("image edit entries must have strictly increasing unique tile indices")
    }
}

impl Error for ImageEditCreateError {}
