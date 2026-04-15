use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::TileKey;
use gla_doc_renderer::{GlaDocRenderer, GlaDocRendererError};
use gla_document::{GlaDoc, GlaDocError};
use glaphica_core::{CanvasVec2, IMAGE_TILE_SIZE};
use renderer::{
    PresentTileCommand, PresentTileParams, RenderCommand, RenderTarget2d, TileRenderer,
    TileRendererError,
};

use crate::AppView;

#[derive(Debug)]
pub enum AppPresentError {
    Document(GlaDocError),
    DocRenderer(GlaDocRendererError),
    TileRenderer(TileRendererError),
}

impl Display for AppPresentError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Document(error) => Display::fmt(error, f),
            Self::DocRenderer(error) => Display::fmt(error, f),
            Self::TileRenderer(error) => Display::fmt(error, f),
        }
    }
}

impl Error for AppPresentError {}

impl From<GlaDocError> for AppPresentError {
    fn from(error: GlaDocError) -> Self {
        Self::Document(error)
    }
}

impl From<GlaDocRendererError> for AppPresentError {
    fn from(error: GlaDocRendererError) -> Self {
        Self::DocRenderer(error)
    }
}

impl From<TileRendererError> for AppPresentError {
    fn from(error: TileRendererError) -> Self {
        Self::TileRenderer(error)
    }
}

pub fn present_root_tiles(
    doc: &GlaDoc,
    doc_renderer: &GlaDocRenderer,
    tile_renderer: &mut TileRenderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    view: &AppView,
    render_target: RenderTarget2d<'_>,
    tile_indices: &[usize],
) -> Result<(), AppPresentError> {
    let root_image = doc_renderer
        .root_active_image()
        .ok_or(GlaDocRendererError::MissingActivePlan)?;
    let layout = doc.layout();
    let mut commands = Vec::new();

    for &tile_index in tile_indices {
        let tile_key = root_image.tile_key(tile_index).unwrap_or(TileKey::EMPTY);
        if tile_key == TileKey::EMPTY {
            continue;
        }

        let origin =
            layout
                .tile_canvas_origin(tile_index)
                .ok_or(GlaDocError::InvalidTileIndex {
                    tile_index,
                    tile_count: root_image.tile_count(),
                })?;
        let source_size = tile_source_extent(layout, origin);
        let target_min = view.document_to_screen_point(origin);
        let target_max = view.document_to_screen_point(CanvasVec2::new(
            origin.x + source_size[0] as f32,
            origin.y + source_size[1] as f32,
        ));
        commands.push(RenderCommand::PresentTile(PresentTileCommand {
            source_tile_key: tile_key,
            params: PresentTileParams {
                target_min_px: [target_min.x, target_min.y],
                target_max_px: [target_max.x, target_max.y],
                source_width: source_size[0],
                source_height: source_size[1],
            },
        }));
    }

    tile_renderer.execute_commands(device, queue, &[], &[], &commands, Some(render_target))?;

    Ok(())
}

fn tile_source_extent(layout: gla_document::GlaImageLayout, origin: CanvasVec2) -> [u32; 2] {
    let remaining_x = layout.size_x().saturating_sub(origin.x.max(0.0) as u32);
    let remaining_y = layout.size_y().saturating_sub(origin.y.max(0.0) as u32);
    [
        remaining_x.min(IMAGE_TILE_SIZE),
        remaining_y.min(IMAGE_TILE_SIZE),
    ]
}

#[cfg(test)]
mod tests {
    use glaphica_core::IMAGE_TILE_SIZE;

    use crate::display::present::tile_source_extent;
    use gla_document::GlaImageLayout;
    use glaphica_core::CanvasVec2;

    #[test]
    fn tile_source_extent_clamps_edge_tiles() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE + 5, IMAGE_TILE_SIZE + 7);
        assert_eq!(
            tile_source_extent(layout, CanvasVec2::new(IMAGE_TILE_SIZE as f32, 0.0)),
            [5, IMAGE_TILE_SIZE]
        );
        assert_eq!(
            tile_source_extent(
                layout,
                CanvasVec2::new(IMAGE_TILE_SIZE as f32, IMAGE_TILE_SIZE as f32)
            ),
            [5, 7]
        );
    }
}
