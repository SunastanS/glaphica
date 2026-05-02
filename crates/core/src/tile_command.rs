use atlas::TileKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyTileCommand {
    pub source_tile_key: TileKey,
    pub destination_tile_key: TileKey,
}
