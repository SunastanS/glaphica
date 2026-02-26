//! Tile draw-instance builders.
//!
//! This module converts resolver/tile-store data into GPU draw-instance vectors
//! for leaf and group rendering paths.

use std::collections::{HashMap, HashSet};

use model::TILE_IMAGE;
use render_protocol::{BlendMode, ImageSource};
use tiles::{GroupTileAtlasStore, TileImage, TileKey};

use crate::{
    CachedLeafDraw, DirtyTileMask, RenderDataResolver, TileCoord, TileDrawInstance, TileInstanceGpu,
};

pub(crate) fn build_leaf_tile_draw_instances(
    blend: BlendMode,
    image_source: ImageSource,
    render_data_resolver: &dyn RenderDataResolver,
) -> Vec<TileDrawInstance> {
    let mut draw_instances = Vec::new();
    let mut collect_tile = |tile_x: u32, tile_y: u32, tile_key: TileKey| {
        let address = render_data_resolver
            .resolve_image_source_tile_address(image_source, tile_key)
            .unwrap_or_else(|| {
                panic!(
                    "layer tile key unresolved while building full leaf draw instances: image_source={:?} tile=({}, {}) key={:?}",
                    image_source,
                    tile_x,
                    tile_y,
                    tile_key
                )
            });
        let document_x = tile_x
            .checked_mul(TILE_IMAGE)
            .expect("tile x position overflow") as f32;
        let document_y = tile_y
            .checked_mul(TILE_IMAGE)
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
    render_data_resolver.visit_image_source_tiles(image_source, &mut collect_tile);
    draw_instances
}

pub(crate) fn build_leaf_tile_draw_instances_for_tiles(
    blend: BlendMode,
    image_source: ImageSource,
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
        let address = render_data_resolver
            .resolve_image_source_tile_address(image_source, tile_key)
            .unwrap_or_else(|| {
                panic!(
                    "layer tile key unresolved while building partial leaf draw instances: image_source={:?} tile=({}, {}) key={:?}",
                    image_source,
                    tile_x,
                    tile_y,
                    tile_key
                )
            });
        let document_x = tile_x
            .checked_mul(TILE_IMAGE)
            .expect("tile x position overflow") as f32;
        let document_y = tile_y
            .checked_mul(TILE_IMAGE)
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
    render_data_resolver.visit_image_source_tiles_for_coords(
        image_source,
        &requested_coords,
        &mut collect_tile,
    );
    draw_instances
}

pub(crate) fn build_group_tile_draw_instances(
    image: &TileImage,
    blend: BlendMode,
    tile_store: &GroupTileAtlasStore,
) -> Vec<TileDrawInstance> {
    #[cfg(debug_assertions)]
    {
        let mut coord_by_key = HashMap::new();
        for (tile_x, tile_y, tile_key) in image.iter_tiles() {
            if let Some((existing_x, existing_y)) = coord_by_key.insert(tile_key, (tile_x, tile_y))
            {
                if (existing_x, existing_y) != (tile_x, tile_y) {
                    panic!(
                        "group tile image uses duplicated key across coordinates: key={:?} first_tile=({}, {}) duplicate_tile=({}, {})",
                        tile_key, existing_x, existing_y, tile_x, tile_y
                    );
                }
            }
        }
    }
    image
        .iter_tiles()
        .map(|(tile_x, tile_y, tile_key)| {
            let tile_address = tile_store
                .resolve(tile_key)
                .expect("group tile key must resolve to atlas address");
            let document_x = tile_x
                .checked_mul(TILE_IMAGE)
                .expect("group tile x position overflow") as f32;
            let document_y = tile_y
                .checked_mul(TILE_IMAGE)
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
        tile_x: (instance.tile.document_x as u32) / TILE_IMAGE,
        tile_y: (instance.tile.document_y as u32) / TILE_IMAGE,
    }
}

pub(crate) fn leaf_should_rebuild(
    dirty_tiles: Option<&DirtyTileMask>,
    cached_leaf: Option<&CachedLeafDraw>,
    blend: BlendMode,
    image_source: ImageSource,
) -> bool {
    if dirty_tiles.is_some() {
        return true;
    }
    let Some(cached_leaf) = cached_leaf else {
        return true;
    };
    cached_leaf.blend != blend
        || cached_leaf.image_source != image_source
        || cached_leaf.draw_instances.is_empty()
}
