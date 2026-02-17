//! Geometry and coordinate-space helpers.
//!
//! This module computes clip matrices, cache extents, and tile-grid dimensions
//! derived from document size.

use render_protocol::TransformMatrix4x4;
use tiles::{TILE_SIZE, TILE_STRIDE};

pub(super) fn document_clip_matrix_from_size(
    document_width: u32,
    document_height: u32,
) -> TransformMatrix4x4 {
    assert!(
        document_width > 0 && document_height > 0,
        "document size must be positive"
    );
    let width_f32 = document_width as f32;
    let height_f32 = document_height as f32;
    [
        2.0 / width_f32,
        0.0,
        0.0,
        0.0,
        0.0,
        -2.0 / height_f32,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
        -1.0,
        1.0,
        0.0,
        1.0,
    ]
}

pub(super) fn group_cache_extent_from_document_size(
    document_width: u32,
    document_height: u32,
) -> wgpu::Extent3d {
    let (tiles_per_row, tiles_per_column) =
        group_tile_grid_from_document_size(document_width, document_height);
    wgpu::Extent3d {
        width: tiles_per_row
            .checked_mul(TILE_SIZE)
            .expect("group cache width overflow"),
        height: tiles_per_column
            .checked_mul(TILE_SIZE)
            .expect("group cache height overflow"),
        depth_or_array_layers: 1,
    }
}

pub(super) fn group_cache_slot_extent_from_document_size(
    document_width: u32,
    document_height: u32,
) -> wgpu::Extent3d {
    let (tiles_per_row, tiles_per_column) =
        group_tile_grid_from_document_size(document_width, document_height);
    wgpu::Extent3d {
        width: tiles_per_row
            .checked_mul(TILE_STRIDE)
            .expect("group slot cache width overflow"),
        height: tiles_per_column
            .checked_mul(TILE_STRIDE)
            .expect("group slot cache height overflow"),
        depth_or_array_layers: 1,
    }
}

pub(super) fn group_tile_grid_from_document_size(
    document_width: u32,
    document_height: u32,
) -> (u32, u32) {
    assert!(
        document_width > 0 && document_height > 0,
        "document size must be positive"
    );
    (
        document_width.div_ceil(TILE_SIZE),
        document_height.div_ceil(TILE_SIZE),
    )
}
