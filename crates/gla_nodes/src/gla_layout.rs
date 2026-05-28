use gla_core::{CanvasCoordF, CanvasCoordU, IMAGE_TILE_SIZE};

pub enum HitMode {
    Box,
    Circle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlaImageLayout {
    width_px: u32,
    height_px: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlaLayoutError {
    InvalidTileIndex { index: usize },
}

impl GlaImageLayout {
    pub fn new(width_px: u32, height_px: u32) -> Self {
        Self {
            width_px,
            height_px,
        }
    }

    pub const fn tile_x(&self) -> usize {
        self.width_px.div_ceil(IMAGE_TILE_SIZE) as usize
    }

    pub const fn tile_y(&self) -> usize {
        self.height_px.div_ceil(IMAGE_TILE_SIZE) as usize
    }

    pub const fn total_tiles(&self) -> usize {
        self.tile_x() * self.tile_y()
    }

    pub fn tile_canvas_origin(&self, index: usize) -> Result<CanvasCoordU, GlaLayoutError> {
        let x = index % self.tile_x() * IMAGE_TILE_SIZE as usize;
        let y = index / self.tile_x() * IMAGE_TILE_SIZE as usize;
        Ok(CanvasCoordU::new(x, y))
    }

    pub fn for_each_affected_tile_index<F, E>(
        &self,
        center: CanvasCoordF,
        radius: f32,
        mode: HitMode,
        mut f: F,
    ) -> Result<(), E>
    where
        F: FnMut(usize) -> Result<(), E>,
    {
        let tile_w = self.tile_x();
        let tile_h = self.tile_y();
        let ts = IMAGE_TILE_SIZE as f32;

        match mode {
            HitMode::Box => {
                let min_x = center.x - radius;
                let max_x = center.x + radius;
                let min_y = center.y - radius;
                let max_y = center.y + radius;

                if max_x < 0.0
                    || min_x >= self.width_px as f32
                    || max_y < 0.0
                    || min_y >= self.height_px as f32
                {
                    return Ok(());
                }

                let min_x = min_x.max(0.0);
                let max_x = max_x.min(self.width_px as f32);
                let min_y = min_y.max(0.0);
                let max_y = max_y.min(self.height_px as f32);

                let min_tx = (min_x / ts).floor() as usize;
                let max_tx = ((max_x / ts).floor() as usize).min(tile_w.saturating_sub(1));
                let min_ty = (min_y / ts).floor() as usize;
                let max_ty = ((max_y / ts).floor() as usize).min(tile_h.saturating_sub(1));

                for ty in min_ty..=max_ty {
                    for tx in min_tx..=max_tx {
                        f(ty * tile_w + tx)?;
                    }
                }
            }
            HitMode::Circle => {
                let tcx = center.x / ts;
                let tcy = center.y / ts;
                let tr = radius / ts;

                let min_ty_f = tcy - tr;
                let max_ty_f = tcy + tr;

                if max_ty_f < 0.0 || min_ty_f >= tile_h as f32 {
                    return Ok(());
                }

                let min_ty = (min_ty_f.max(0.0).floor() as usize).min(tile_h.saturating_sub(1));
                let max_ty = (max_ty_f.floor() as usize).min(tile_h.saturating_sub(1));

                let tr2 = tr * tr;

                for ty in min_ty..=max_ty {
                    let tyf = ty as f32;
                    let dy = if tcy < tyf {
                        tyf - tcy
                    } else if tcy > tyf + 1.0 {
                        tcy - tyf - 1.0
                    } else {
                        0.0
                    };

                    let dy2 = dy * dy;
                    if dy2 > tr2 {
                        continue;
                    }

                    let dx = (tr2 - dy2).sqrt();
                    let min_tx_f = tcx - dx;
                    let max_tx_f = tcx + dx;

                    if max_tx_f < 0.0 || min_tx_f >= tile_w as f32 {
                        continue;
                    }

                    let min_tx = min_tx_f.max(0.0).floor() as usize;
                    let max_tx = (max_tx_f.floor() as usize).min(tile_w.saturating_sub(1));

                    for tx in min_tx..=max_tx {
                        f(ty * tile_w + tx)?;
                    }
                }
            }
        }

        Ok(())
    }
}
