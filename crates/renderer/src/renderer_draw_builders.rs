//! Tile draw-instance builders.
//!
//! This module converts resolver/tile-store data into GPU draw-instance vectors
//! for leaf and group rendering paths.

use std::collections::HashSet;

use render_protocol::{BlendMode, ImageHandle};
use tiles::{GroupTileAtlasStore, TILE_SIZE, TileKey, VirtualImage};

use crate::{
    CachedLeafDraw, DirtyTileMask, RenderDataResolver, TileCoord, TileDrawInstance, TileInstanceGpu,
};

pub(crate) fn build_leaf_tile_draw_instances(
    blend: BlendMode,
    image_handle: ImageHandle,
    render_data_resolver: &dyn RenderDataResolver,
) -> Vec<TileDrawInstance> {
    let mut draw_instances = Vec::new();
    let mut collect_tile = |tile_x: u32, tile_y: u32, tile_key: TileKey| {
        let Some(address) = render_data_resolver.resolve_tile_address(tile_key) else {
            return;
        };
        let document_x = tile_x
            .checked_mul(TILE_SIZE)
            .expect("tile x position overflow") as f32;
        let document_y = tile_y
            .checked_mul(TILE_SIZE)
            .expect("tile y position overflow") as f32;

        draw_instances.push(TileDrawInstance {
            blend_mode: blend,
            tile: TileInstanceGpu {
                document_x,
                document_y,
                atlas_layer: address.atlas_layer as f32,
                tile_index: address.tile_index as u32,
                _padding0: 0,
            },
        });
    };
    render_data_resolver.visit_image_tiles(image_handle, &mut collect_tile);
    draw_instances
}

pub(crate) fn build_leaf_tile_draw_instances_for_tiles(
    blend: BlendMode,
    image_handle: ImageHandle,
    render_data_resolver: &dyn RenderDataResolver,
    tiles: &HashSet<TileCoord>,
) -> Vec<TileDrawInstance> {
    if tiles.is_empty() {
        return Vec::new();
    }

    let requested_coords: Vec<(u32, u32)> = tiles
        .iter()
        .map(|coord| (coord.tile_x, coord.tile_y))
        .collect();
    let mut draw_instances = Vec::new();
    let mut collect_tile = |tile_x: u32, tile_y: u32, tile_key: TileKey| {
        let Some(address) = render_data_resolver.resolve_tile_address(tile_key) else {
            return;
        };
        let document_x = tile_x
            .checked_mul(TILE_SIZE)
            .expect("tile x position overflow") as f32;
        let document_y = tile_y
            .checked_mul(TILE_SIZE)
            .expect("tile y position overflow") as f32;

        draw_instances.push(TileDrawInstance {
            blend_mode: blend,
            tile: TileInstanceGpu {
                document_x,
                document_y,
                atlas_layer: address.atlas_layer as f32,
                tile_index: address.tile_index as u32,
                _padding0: 0,
            },
        });
    };
    render_data_resolver.visit_image_tiles_for_coords(
        image_handle,
        &requested_coords,
        &mut collect_tile,
    );
    draw_instances
}

pub(crate) fn build_group_tile_draw_instances(
    image: &VirtualImage<TileKey>,
    blend: BlendMode,
    tile_store: &GroupTileAtlasStore,
) -> Vec<TileDrawInstance> {
    image
        .iter_tiles()
        .map(|(tile_x, tile_y, tile_key)| {
            let tile_address = tile_store
                .resolve(*tile_key)
                .expect("group tile key must resolve to atlas address");
            let document_x = tile_x
                .checked_mul(TILE_SIZE)
                .expect("group tile x position overflow") as f32;
            let document_y = tile_y
                .checked_mul(TILE_SIZE)
                .expect("group tile y position overflow") as f32;
            TileDrawInstance {
                blend_mode: blend,
                tile: TileInstanceGpu {
                    document_x,
                    document_y,
                    atlas_layer: tile_address.atlas_layer as f32,
                    tile_index: tile_address.tile_index as u32,
                    _padding0: 0,
                },
            }
        })
        .collect()
}

pub(crate) fn tile_coord_from_draw_instance(instance: &TileDrawInstance) -> TileCoord {
    TileCoord {
        tile_x: (instance.tile.document_x as u32) / TILE_SIZE,
        tile_y: (instance.tile.document_y as u32) / TILE_SIZE,
    }
}

pub(crate) fn leaf_should_rebuild(
    dirty_tiles: Option<&DirtyTileMask>,
    cached_leaf: Option<&CachedLeafDraw>,
    blend: BlendMode,
    image_handle: ImageHandle,
) -> bool {
    if dirty_tiles.is_some() {
        return true;
    }
    let Some(cached_leaf) = cached_leaf else {
        return true;
    };
    cached_leaf.blend != blend
        || cached_leaf.image_handle != image_handle
        || cached_leaf.draw_instances.is_empty()
}
