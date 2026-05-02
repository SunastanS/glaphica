#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyTileCommand<K> {
    pub source_tile_key: K,
    pub destination_tile_key: K,
}
