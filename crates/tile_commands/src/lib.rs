use atlas::Position;

pub enum TileCommand {
    CopyTile { src: Position, dst: Position },
    ClearTile { dst: Position },
    // MergeTile {src, dst, pipeline, para},
    // DrawOn {dst, pipeline, para},
}

pub struct TileOpRecorder {
    commands: Vec<TileCommand>,
}

impl TileOpRecorder {
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
        }
    }

    pub fn copy_tile(&mut self, src: Position, dst: Position) {
        self.commands.push(TileCommand::CopyTile { src, dst });
    }

    pub fn clear_tile(&mut self, dst: Position) {
        self.commands.push(TileCommand::ClearTile { dst });
    }
}
