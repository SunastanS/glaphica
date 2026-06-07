use gla_command_core::{FootprintModifier, Mapping, OpId, OpParams};
use gla_image::GlaImageKey;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TileSet {
    Full,
    Tiles(Vec<u32>),
}

impl TileSet {
    pub fn tiles<I>(tiles: I) -> Self
    where
        I: IntoIterator<Item = u32>,
    {
        let mut tiles: Vec<u32> = tiles.into_iter().collect();
        tiles.sort_unstable();
        tiles.dedup();
        Self::Tiles(tiles)
    }

    pub fn single(tile: u32) -> Self {
        Self::Tiles(vec![tile])
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Tiles(tiles) if tiles.is_empty())
    }

    pub fn union_assign(&mut self, other: &Self) {
        match other {
            Self::Full => *self = Self::Full,
            Self::Tiles(right) if right.is_empty() => {}
            Self::Tiles(right) => match self {
                Self::Full => {}
                Self::Tiles(left) => {
                    left.extend(right.iter().copied());
                    left.sort_unstable();
                    left.dedup();
                }
            },
        }
    }
}

impl Default for TileSet {
    fn default() -> Self {
        Self::Tiles(Vec::new())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageCommandRead {
    pub image: GlaImageKey,
    pub mapping: Mapping,
    pub modifier: FootprintModifier,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageCommand {
    pub reads: Vec<ImageCommandRead>,
    pub dst: GlaImageKey,
    pub op: OpId,
    pub params: OpParams,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DrawCommand {
    pub dst: GlaImageKey,
    pub input_mapping: Mapping,
    pub op: OpId,
    pub params: OpParams,
}
