//! Renderer unit tests.
//!
//! This module validates frame sync behavior, geometry helpers, dirty propagation,
//! planning decisions, and draw-instance construction invariants.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use super::*;

type RenderTreeSnapshot = render_protocol::RenderTreeSnapshot;

fn leaf(layer_id: u64, blend: BlendMode) -> RenderTreeNode {
    RenderTreeNode::Leaf {
        layer_id,
        blend,
        image_source: render_protocol::ImageSource::LayerImage {
            image_handle: image_handle(),
        },
    }
}

fn group(group_id: u64, blend: BlendMode, children: Vec<RenderTreeNode>) -> RenderTreeNode {
    RenderTreeNode::Group {
        group_id,
        blend,
        children: children.into_boxed_slice().into(),
    }
}

fn snapshot(revision: u64, root: RenderTreeNode) -> RenderTreeSnapshot {
    RenderTreeSnapshot {
        revision,
        root: std::sync::Arc::new(root),
    }
}

#[test]
fn frame_sync_rejects_commit_after_epoch_change() {
    let mut sync = FrameSync::default();
    let version = sync.version(4, 10);
    sync.note_state_change();
    assert!(!sync.can_commit(version, 10));
}

#[test]
fn frame_sync_rejects_commit_after_snapshot_change() {
    let sync = FrameSync::default();
    let version = sync.version(7, 20);
    assert!(!sync.can_commit(version, 21));
}

#[test]
fn frame_sync_rejects_non_monotonic_frame_id() {
    let mut sync = FrameSync::default();
    let version0 = sync.version(2, 9);
    sync.commit(version0, 9);
    let stale_version = sync.version(2, 9);
    assert!(!sync.can_commit(stale_version, 9));
}

#[test]
fn rerender_tiles_for_group_force_active_region_ignores_partial_dirty() {
    let partial_tiles = HashSet::from([
        TileCoord {
            tile_x: 10,
            tile_y: 10,
        },
        TileCoord {
            tile_x: 11,
            tile_y: 11,
        },
    ]);
    let active_tiles = HashSet::from([TileCoord {
        tile_x: 1,
        tile_y: 2,
    }]);

    let rerender_tiles = rerender_tiles_for_group(
        true,
        Some(&DirtyTileMask::Partial(partial_tiles)),
        Some(&active_tiles),
    )
    .expect("forced active region should keep active mask");

    assert_eq!(rerender_tiles, active_tiles);
}

#[test]
fn rerender_tiles_for_group_intersects_partial_with_active_tiles() {
    let dirty_tiles = HashSet::from([
        TileCoord {
            tile_x: 1,
            tile_y: 1,
        },
        TileCoord {
            tile_x: 2,
            tile_y: 2,
        },
    ]);
    let active_tiles = HashSet::from([
        TileCoord {
            tile_x: 2,
            tile_y: 2,
        },
        TileCoord {
            tile_x: 3,
            tile_y: 3,
        },
    ]);

    let rerender_tiles = rerender_tiles_for_group(
        false,
        Some(&DirtyTileMask::Partial(dirty_tiles)),
        Some(&active_tiles),
    )
    .expect("partial dirty should produce masked region");

    assert_eq!(
        rerender_tiles,
        HashSet::from([TileCoord {
            tile_x: 2,
            tile_y: 2,
        }])
    );
}

#[test]
fn group_decision_engine_uses_cache_when_group_is_clean() {
    let decision = GroupDecisionEngine::default().decide(false, false, None, None);
    assert_eq!(decision.mode, GroupRerenderMode::UseCache);
    assert_eq!(decision.rerender_tiles, None);
}

#[test]
fn group_decision_engine_marks_group_for_rerender_when_dirty() {
    let dirty_tiles = HashSet::from([TileCoord {
        tile_x: 4,
        tile_y: 7,
    }]);
    let decision = GroupDecisionEngine::default().decide(
        false,
        false,
        Some(&DirtyTileMask::Partial(dirty_tiles.clone())),
        None,
    );
    assert_eq!(decision.mode, GroupRerenderMode::Rerender);
    assert_eq!(decision.rerender_tiles, Some(dirty_tiles));
}

#[test]
fn leaf_should_rebuild_when_dirty_even_with_cache_hit() {
    let cached_leaf = CachedLeafDraw {
        blend: BlendMode::Normal,
        image_source: render_protocol::ImageSource::LayerImage {
            image_handle: image_handle(),
        },
        draw_instances: vec![TileDrawInstance {
            blend_mode: BlendMode::Normal,
            tile: TileInstanceGpu {
                document_x: 0.0,
                document_y: 0.0,
                atlas_layer: 0.0,
                tile_index: 0,
                _padding0: 0,
            },
        }],
        tile_instance_index: HashMap::new(),
    };
    let dirty_tiles = HashSet::from([TileCoord {
        tile_x: 0,
        tile_y: 0,
    }]);
    assert!(leaf_should_rebuild(
        Some(&DirtyTileMask::Partial(dirty_tiles)),
        Some(&cached_leaf),
        BlendMode::Normal,
        cached_leaf.image_source,
    ));
}

#[test]
fn leaf_should_not_rebuild_when_cache_matches_and_clean() {
    let image_handle = image_handle();
    let cached_leaf = CachedLeafDraw {
        blend: BlendMode::Normal,
        image_source: render_protocol::ImageSource::LayerImage { image_handle },
        draw_instances: vec![TileDrawInstance {
            blend_mode: BlendMode::Normal,
            tile: TileInstanceGpu {
                document_x: 0.0,
                document_y: 0.0,
                atlas_layer: 0.0,
                tile_index: 0,
                _padding0: 0,
            },
        }],
        tile_instance_index: HashMap::new(),
    };
    assert!(!leaf_should_rebuild(
        None,
        Some(&cached_leaf),
        BlendMode::Normal,
        cached_leaf.image_source,
    ));
}

#[test]
fn cached_leaf_partial_replace_keeps_index_consistent() {
    let mut cached_leaf = CachedLeafDraw {
        blend: BlendMode::Normal,
        image_source: render_protocol::ImageSource::LayerImage {
            image_handle: image_handle(),
        },
        draw_instances: vec![
            TileDrawInstance {
                blend_mode: BlendMode::Normal,
                tile: TileInstanceGpu {
                    document_x: 0.0,
                    document_y: 0.0,
                    atlas_layer: 0.0,
                    tile_index: 0,
                    _padding0: 0,
                },
            },
            TileDrawInstance {
                blend_mode: BlendMode::Normal,
                tile: TileInstanceGpu {
                    document_x: TILE_IMAGE as f32,
                    document_y: 0.0,
                    atlas_layer: 0.0,
                    tile_index: 0,
                    _padding0: 0,
                },
            },
        ],
        tile_instance_index: HashMap::new(),
    };
    cached_leaf.rebuild_tile_index();

    let dirty_tiles = HashSet::from([TileCoord {
        tile_x: 0,
        tile_y: 0,
    }]);
    cached_leaf.replace_partial_tiles(&dirty_tiles);

    assert_eq!(cached_leaf.draw_instances.len(), 1);
    assert!(cached_leaf.tile_instance_index.contains_key(&TileCoord {
        tile_x: 1,
        tile_y: 0,
    }));
    assert_eq!(cached_leaf.tile_instance_index.len(), 1);
}

#[derive(Default)]
struct DirtyPropagationResolver {
    propagate_calls: Cell<u32>,
    propagated_rects: HashMap<u64, Vec<DirtyRect>>,
}

impl RenderDataResolver for DirtyPropagationResolver {
    fn document_size(&self) -> (u32, u32) {
        (TILE_IMAGE, TILE_IMAGE)
    }

    fn visit_image_tiles(
        &self,
        _image_handle: ImageHandle,
        _visitor: &mut dyn FnMut(u32, u32, TileKey),
    ) {
    }

    fn propagate_layer_dirty_rects(
        &self,
        layer_id: u64,
        incoming_rects: &[DirtyRect],
    ) -> Vec<DirtyRect> {
        self.propagate_calls.set(self.propagate_calls.get() + 1);
        self.propagated_rects
            .get(&layer_id)
            .cloned()
            .unwrap_or_else(|| incoming_rects.to_vec())
    }

    fn resolve_tile_address(&self, _tile_key: TileKey) -> Option<TileAddress> {
        None
    }

    fn layer_dirty_since(
        &self,
        _layer_id: u64,
        _since_version: u64,
    ) -> Option<tiles::DirtySinceResult> {
        None
    }

    fn layer_version(&self, _layer_id: u64) -> Option<u64> {
        None
    }
}

#[test]
fn resolve_layer_dirty_rect_masks_uses_resolver_propagation_hook() {
    let mut dirty_state_store = DirtyStateStore::default();
    let _ = dirty_state_store.mark_layer_rect(
        9,
        DirtyRect {
            min_x: 0,
            min_y: 0,
            max_x: 1,
            max_y: 1,
        },
    );
    let resolver = DirtyPropagationResolver {
        propagated_rects: HashMap::from([(
            9,
            vec![DirtyRect {
                min_x: 0,
                min_y: 0,
                max_x: (TILE_IMAGE as i32) * 2,
                max_y: 1,
            }],
        )]),
        ..Default::default()
    };

    let layer_dirty_rect_masks = dirty_state_store.resolve_layer_dirty_rect_masks(&resolver);

    assert_eq!(resolver.propagate_calls.get(), 1);
    assert!(matches!(
        layer_dirty_rect_masks.get(&9),
        Some(DirtyRectMask::Rects(rects)) if rects == &vec![DirtyRect {
            min_x: 0,
            min_y: 0,
            max_x: (TILE_IMAGE as i32) * 2,
            max_y: 1,
        }]
    ));
}

#[test]
fn resolve_layer_dirty_rect_masks_skips_propagation_for_full_layer_dirty() {
    let mut dirty_state_store = DirtyStateStore::default();
    dirty_state_store.mark_layer_full(3);
    let _ = dirty_state_store.mark_layer_rect(
        3,
        DirtyRect {
            min_x: 0,
            min_y: 0,
            max_x: 10,
            max_y: 10,
        },
    );
    let resolver = DirtyPropagationResolver::default();

    let layer_dirty_rect_masks = dirty_state_store.resolve_layer_dirty_rect_masks(&resolver);

    assert_eq!(resolver.propagate_calls.get(), 0);
    assert!(matches!(
        layer_dirty_rect_masks.get(&3),
        Some(DirtyRectMask::Full)
    ));
}

#[derive(Default)]
struct FakeResolver {
    visit_calls: Cell<u32>,
    resolve_calls: Cell<u32>,
    emit_tiles: bool,
    first_tile_key: Option<TileKey>,
    second_tile_key: Option<TileKey>,
}

impl RenderDataResolver for FakeResolver {
    fn document_size(&self) -> (u32, u32) {
        (TILE_IMAGE, TILE_IMAGE)
    }

    fn visit_image_tiles(
        &self,
        _image_handle: ImageHandle,
        visitor: &mut dyn FnMut(u32, u32, TileKey),
    ) {
        self.visit_calls.set(self.visit_calls.get() + 1);
        if self.emit_tiles {
            visitor(
                1,
                2,
                self.first_tile_key
                    .expect("fake resolver requires first tile key"),
            );
            visitor(
                3,
                4,
                self.second_tile_key
                    .expect("fake resolver requires second tile key"),
            );
        }
    }

    fn resolve_tile_address(&self, tile_key: TileKey) -> Option<TileAddress> {
        self.resolve_calls.set(self.resolve_calls.get() + 1);
        if Some(tile_key) == self.first_tile_key {
            Some(TileAddress {
                atlas_layer: 2,
                tile_index: 9,
            })
        } else {
            None
        }
    }

    fn layer_dirty_since(
        &self,
        _layer_id: u64,
        _since_version: u64,
    ) -> Option<tiles::DirtySinceResult> {
        None
    }

    fn layer_version(&self, _layer_id: u64) -> Option<u64> {
        None
    }
}

#[derive(Default)]
struct FakeBrushBufferResolver {
    visit_calls: Cell<u32>,
    resolve_calls: Cell<u32>,
    brush_tile_key: Option<TileKey>,
}

impl RenderDataResolver for FakeBrushBufferResolver {
    fn document_size(&self) -> (u32, u32) {
        (TILE_IMAGE, TILE_IMAGE)
    }

    fn visit_image_tiles(
        &self,
        _image_handle: ImageHandle,
        _visitor: &mut dyn FnMut(u32, u32, TileKey),
    ) {
    }

    fn visit_image_source_tiles(
        &self,
        image_source: render_protocol::ImageSource,
        visitor: &mut dyn FnMut(u32, u32, TileKey),
    ) {
        self.visit_calls.set(self.visit_calls.get() + 1);
        match image_source {
            render_protocol::ImageSource::BrushBuffer { .. } => {
                visitor(
                    5,
                    7,
                    self.brush_tile_key
                        .expect("fake brush resolver requires brush tile key"),
                );
            }
            render_protocol::ImageSource::LayerImage { .. } => {
                panic!("fake brush resolver only supports brush buffer source");
            }
        }
    }

    fn resolve_tile_address(&self, _tile_key: TileKey) -> Option<TileAddress> {
        None
    }

    fn resolve_image_source_tile_address(
        &self,
        image_source: render_protocol::ImageSource,
        tile_key: TileKey,
    ) -> Option<TileAddress> {
        self.resolve_calls.set(self.resolve_calls.get() + 1);
        match image_source {
            render_protocol::ImageSource::BrushBuffer { .. } => {
                if Some(tile_key) == self.brush_tile_key {
                    Some(TileAddress {
                        atlas_layer: 3,
                        tile_index: 11,
                    })
                } else {
                    None
                }
            }
            render_protocol::ImageSource::LayerImage { .. } => None,
        }
    }

    fn layer_dirty_since(
        &self,
        _layer_id: u64,
        _since_version: u64,
    ) -> Option<tiles::DirtySinceResult> {
        None
    }

    fn layer_version(&self, _layer_id: u64) -> Option<u64> {
        None
    }
}

#[test]
#[should_panic(expected = "layer tile key unresolved while building full leaf draw instances")]
fn build_leaf_tile_draw_instances_panics_on_unresolved_tile_key() {
    let tile_keys = allocate_tile_keys(2);
    let resolver = FakeResolver {
        emit_tiles: true,
        first_tile_key: Some(tile_keys[0]),
        second_tile_key: Some(tile_keys[1]),
        ..Default::default()
    };

    let _ = build_leaf_tile_draw_instances(
        BlendMode::Multiply,
        render_protocol::ImageSource::LayerImage {
            image_handle: image_handle(),
        },
        &resolver,
    );
}

#[test]
fn build_leaf_tile_draw_instances_supports_brush_buffer_source() {
    let tile_key = allocate_tile_keys(1)[0];
    let resolver = FakeBrushBufferResolver {
        brush_tile_key: Some(tile_key),
        ..Default::default()
    };
    let draw_instances = build_leaf_tile_draw_instances(
        BlendMode::Normal,
        render_protocol::ImageSource::BrushBuffer {
            stroke_session_id: 1,
        },
        &resolver,
    );
    assert_eq!(resolver.visit_calls.get(), 1);
    assert_eq!(resolver.resolve_calls.get(), 1);
    assert_eq!(draw_instances.len(), 1);
    assert_eq!(draw_instances[0].tile.document_x, (5 * TILE_IMAGE) as f32);
    assert_eq!(draw_instances[0].tile.document_y, (7 * TILE_IMAGE) as f32);
    assert_eq!(draw_instances[0].tile.atlas_layer, 3.0);
    assert_eq!(draw_instances[0].tile.tile_index, 11);
}

#[test]
#[should_panic(expected = "layer tile key unresolved while building partial leaf draw instances")]
fn build_leaf_tile_draw_instances_for_tiles_panics_on_unresolved_tile_key() {
    let tile_keys = allocate_tile_keys(2);
    let resolver = FakeResolver {
        emit_tiles: true,
        first_tile_key: Some(tile_keys[0]),
        second_tile_key: Some(tile_keys[1]),
        ..Default::default()
    };
    let requested_tiles = HashSet::from([TileCoord {
        tile_x: 3,
        tile_y: 4,
    }]);

    let draw_instances = build_leaf_tile_draw_instances_for_tiles(
        BlendMode::Multiply,
        render_protocol::ImageSource::LayerImage {
            image_handle: image_handle(),
        },
        &resolver,
        &requested_tiles,
    );
    let _ = draw_instances;
}

#[test]
fn render_tree_signature_reconstructs_nested_groups() {
    let tree = group(
        0,
        BlendMode::Normal,
        vec![
            group(
                99,
                BlendMode::Multiply,
                vec![leaf(11, BlendMode::Normal), leaf(12, BlendMode::Multiply)],
            ),
            leaf(13, BlendMode::Normal),
        ],
    );
    let signature = render_tree_signature(&tree);

    assert_eq!(
        signature,
        "G(Normal)[G(Multiply)[L(11:Normal),L(12:Multiply)],L(13:Normal)]"
    );
}

#[test]
fn collect_node_tile_masks_marks_ancestors_of_dirty_leaf_only() {
    let tree = group(
        0,
        BlendMode::Normal,
        vec![
            group(
                101,
                BlendMode::Normal,
                vec![leaf(11, BlendMode::Normal), leaf(12, BlendMode::Normal)],
            ),
            leaf(13, BlendMode::Normal),
        ],
    );
    let layer_dirty_rect_masks = HashMap::from([(
        12u64,
        DirtyRectMask::Rects(vec![DirtyRect {
            min_x: TILE_IMAGE as i32,
            min_y: (2 * TILE_IMAGE) as i32,
            max_x: (TILE_IMAGE as i32) + 1,
            max_y: (2 * TILE_IMAGE) as i32 + 1,
        }]),
    )]);
    let dirty_nodes =
        DirtyPropagationEngine::new(16).collect_node_tile_masks(&tree, &layer_dirty_rect_masks);

    assert!(matches!(
        dirty_nodes.get(&RenderNodeKey::Group(101)),
        Some(DirtyTileMask::Partial(_))
    ));
    assert!(matches!(
        dirty_nodes.get(&RenderNodeKey::Group(0)),
        Some(DirtyTileMask::Partial(_))
    ));
}

#[test]
fn collect_node_tile_masks_promotes_group_to_full_at_threshold() {
    let tree = group(0, BlendMode::Normal, vec![leaf(1, BlendMode::Normal)]);
    let layer_dirty_rect_masks = HashMap::from([(
        1u64,
        DirtyRectMask::Rects(vec![DirtyRect {
            min_x: 0,
            min_y: 0,
            max_x: (2 * TILE_IMAGE) as i32,
            max_y: 1,
        }]),
    )]);
    let dirty_nodes =
        DirtyPropagationEngine::new(5).collect_node_tile_masks(&tree, &layer_dirty_rect_masks);

    assert!(matches!(
        dirty_nodes.get(&RenderNodeKey::Group(0)),
        Some(DirtyTileMask::Full)
    ));
}

#[test]
fn collect_node_tile_masks_keeps_partial_group_below_threshold() {
    let tree = group(0, BlendMode::Normal, vec![leaf(1, BlendMode::Normal)]);
    let dirty_tile = TileCoord {
        tile_x: 3,
        tile_y: 2,
    };
    let layer_dirty_rect_masks = HashMap::from([(
        1u64,
        DirtyRectMask::Rects(vec![DirtyRect {
            min_x: (3 * TILE_IMAGE) as i32,
            min_y: (2 * TILE_IMAGE) as i32,
            max_x: (3 * TILE_IMAGE) as i32 + 1,
            max_y: (2 * TILE_IMAGE) as i32 + 1,
        }]),
    )]);
    let dirty_nodes =
        DirtyPropagationEngine::new(5).collect_node_tile_masks(&tree, &layer_dirty_rect_masks);

    assert!(matches!(
        dirty_nodes.get(&RenderNodeKey::Group(0)),
        Some(DirtyTileMask::Partial(tiles)) if tiles == &HashSet::from([dirty_tile])
    ));
}

#[test]
fn collect_node_tile_masks_full_leaf_marks_all_ancestors_full() {
    let tree = group(
        0,
        BlendMode::Normal,
        vec![group(
            2,
            BlendMode::Normal,
            vec![leaf(10, BlendMode::Normal), leaf(11, BlendMode::Normal)],
        )],
    );
    let layer_dirty_rect_masks = HashMap::from([(11u64, DirtyRectMask::Full)]);
    let dirty_nodes =
        DirtyPropagationEngine::new(64).collect_node_tile_masks(&tree, &layer_dirty_rect_masks);

    assert!(matches!(
        dirty_nodes.get(&RenderNodeKey::Leaf(11)),
        Some(DirtyTileMask::Full)
    ));
    assert!(matches!(
        dirty_nodes.get(&RenderNodeKey::Group(2)),
        Some(DirtyTileMask::Full)
    ));
    assert!(matches!(
        dirty_nodes.get(&RenderNodeKey::Group(0)),
        Some(DirtyTileMask::Full)
    ));
}

#[test]
fn collect_node_dirty_rects_propagates_leaf_rects_to_parent_groups() {
    let tree = group(
        0,
        BlendMode::Normal,
        vec![leaf(7, BlendMode::Normal), leaf(8, BlendMode::Normal)],
    );
    let layer_dirty_rect_masks = HashMap::from([
        (
            7u64,
            DirtyRectMask::Rects(vec![DirtyRect {
                min_x: 0,
                min_y: 0,
                max_x: 8,
                max_y: 8,
            }]),
        ),
        (
            8u64,
            DirtyRectMask::Rects(vec![DirtyRect {
                min_x: 16,
                min_y: 16,
                max_x: 24,
                max_y: 24,
            }]),
        ),
    ]);
    let mut node_dirty_rects = HashMap::new();

    let root_mask = collect_node_dirty_rects(&tree, &layer_dirty_rect_masks, &mut node_dirty_rects)
        .expect("root should be dirty when at least one child is dirty");

    assert!(matches!(root_mask, DirtyRectMask::Rects(_)));
    assert!(node_dirty_rects.contains_key(&RenderNodeKey::Leaf(7)));
    assert!(node_dirty_rects.contains_key(&RenderNodeKey::Leaf(8)));
    assert!(node_dirty_rects.contains_key(&RenderNodeKey::Group(0)));
}

#[test]
fn dirty_rect_to_tile_coords_handles_half_open_bounds() {
    let dirty_rect = DirtyRect {
        min_x: 0,
        min_y: 0,
        max_x: TILE_IMAGE as i32,
        max_y: TILE_IMAGE as i32,
    };
    let tiles = dirty_rect_to_tile_coords(dirty_rect);
    assert_eq!(
        tiles,
        HashSet::from([TileCoord {
            tile_x: 0,
            tile_y: 0
        }])
    );

    let dirty_rect = DirtyRect {
        min_x: TILE_IMAGE as i32,
        min_y: 0,
        max_x: (TILE_IMAGE as i32) + 1,
        max_y: 1,
    };
    let tiles = dirty_rect_to_tile_coords(dirty_rect);
    assert_eq!(
        tiles,
        HashSet::from([TileCoord {
            tile_x: 1,
            tile_y: 0
        }])
    );
}

#[test]
fn dirty_rect_to_tile_coords_clamps_negative_input() {
    let dirty_rect = DirtyRect {
        min_x: -100,
        min_y: -40,
        max_x: 1,
        max_y: 1,
    };
    let tiles = dirty_rect_to_tile_coords(dirty_rect);
    assert_eq!(
        tiles,
        HashSet::from([TileCoord {
            tile_x: 0,
            tile_y: 0
        }])
    );
}

#[test]
fn document_clip_matrix_maps_document_corners_to_clip_space() {
    let matrix = document_clip_matrix_from_size(100, 50);
    assert!((matrix[0] - 0.02).abs() < 1e-6);
    assert!((matrix[5] + 0.04).abs() < 1e-6);
    assert_eq!(matrix[12], -1.0);
    assert_eq!(matrix[13], 1.0);
}

#[test]
fn document_clip_matrix_alone_would_stretch_on_wide_surface() {
    let matrix = document_clip_matrix_from_size(512, 512);
    let surface_width = 1280.0f32;
    let surface_height = 720.0f32;

    let pixels_per_document_pixel_x = matrix[0].abs() * surface_width * 0.5;
    let pixels_per_document_pixel_y = matrix[5].abs() * surface_height * 0.5;

    assert!(
        (pixels_per_document_pixel_x - pixels_per_document_pixel_y).abs() > 1e-6,
        "document clip matrix should not be used for final surface pass, got x={} y={}",
        pixels_per_document_pixel_x,
        pixels_per_document_pixel_y
    );
}

#[test]
fn group_cache_extent_rounds_up_to_tile_boundaries() {
    let extent = group_cache_extent_from_document_size(TILE_IMAGE + 7, TILE_IMAGE * 2 + 1);
    assert_eq!(extent.width, TILE_IMAGE * 2);
    assert_eq!(extent.height, TILE_IMAGE * 3);
    assert_eq!(extent.depth_or_array_layers, 1);
}

#[test]
fn group_cache_slot_extent_uses_tile_stride() {
    let extent = group_cache_slot_extent_from_document_size(TILE_IMAGE + 7, TILE_IMAGE * 2 + 1);
    assert_eq!(extent.width, TILE_STRIDE * 2);
    assert_eq!(extent.height, TILE_STRIDE * 3);
    assert_eq!(extent.depth_or_array_layers, 1);
}

#[test]
fn group_tile_grid_rounds_up_to_tile_boundaries() {
    let (tiles_per_row, tiles_per_column) =
        group_tile_grid_from_document_size(TILE_IMAGE + 7, TILE_IMAGE * 2 + 1);
    assert_eq!(tiles_per_row, 2);
    assert_eq!(tiles_per_column, 3);
}

#[test]
#[should_panic(expected = "document size must be positive")]
fn document_clip_matrix_panics_on_zero_size() {
    let _ = document_clip_matrix_from_size(0, 1);
}

#[test]
#[should_panic(expected = "document size must be positive")]
fn group_cache_extent_panics_on_zero_size() {
    let _ = group_cache_extent_from_document_size(1, 0);
}

#[test]
#[should_panic(expected = "document size must be positive")]
fn group_tile_grid_panics_on_zero_size() {
    let _ = group_tile_grid_from_document_size(0, 1);
}

#[test]
#[should_panic(expected = "render tree root must be group 0")]
fn render_tree_root_guard_rejects_non_zero_group() {
    assert!(
        matches!(
            group(99, BlendMode::Normal, vec![leaf(7, BlendMode::Normal)]),
            RenderTreeNode::Group { group_id: 0, .. }
        ),
        "render tree root must be group 0"
    );
}

#[test]
fn isolated_nested_groups_match_manual_expected_solid_tile() {
    let snapshot = snapshot(
        11,
        group(
            0,
            BlendMode::Normal,
            vec![
                leaf(101, BlendMode::Normal),
                group(
                    22,
                    BlendMode::Multiply,
                    vec![
                        leaf(204, BlendMode::Normal),
                        group(
                            21,
                            BlendMode::Normal,
                            vec![leaf(201, BlendMode::Normal), leaf(202, BlendMode::Multiply)],
                        ),
                        leaf(203, BlendMode::Multiply),
                    ],
                ),
                leaf(102, BlendMode::Multiply),
            ],
        ),
    );

    let layer_colors = HashMap::from([
        (101u64, [0.8, 0.5, 0.4, 1.0]),
        (204u64, [0.3, 0.9, 0.7, 1.0]),
        (201u64, [0.6, 0.2, 0.9, 1.0]),
        (202u64, [0.5, 0.7, 0.4, 1.0]),
        (203u64, [0.4, 0.25, 0.8, 1.0]),
        (102u64, [0.9, 0.6, 0.5, 1.0]),
    ]);

    let actual_tile = render_uniform_tile_from_snapshot(&snapshot, &layer_colors);

    let outside_normal = layer_colors[&101];
    let inner_normal = layer_colors[&201];
    let inner_multiply = layer_colors[&202];
    let in_group_multiply = layer_colors[&203];
    let outside_multiply = layer_colors[&102];
    let expected_color = multiply_color(
        multiply_color(
            outside_normal,
            multiply_color(
                multiply_color(inner_normal, inner_multiply),
                in_group_multiply,
            ),
        ),
        outside_multiply,
    );
    let expected_tile = solid_tile(expected_color);

    assert_eq!(actual_tile, expected_tile);

    let flattened_without_isolation = multiply_color(
        multiply_color(inner_normal, inner_multiply),
        multiply_color(in_group_multiply, outside_multiply),
    );
    assert_ne!(flattened_without_isolation, expected_color);
}

fn render_tree_signature(node: &RenderTreeNode) -> String {
    match node {
        RenderTreeNode::Leaf {
            layer_id, blend, ..
        } => {
            format!("L({layer_id}:{blend:?})")
        }
        RenderTreeNode::Group {
            group_id: _,
            blend,
            children,
        } => {
            let children = children
                .iter()
                .map(render_tree_signature)
                .collect::<Vec<_>>()
                .join(",");
            format!("G({blend:?})[{children}]")
        }
    }
}

fn render_uniform_tile_from_snapshot(
    snapshot: &RenderTreeSnapshot,
    layer_colors: &HashMap<u64, [f32; 4]>,
) -> Vec<u8> {
    let tree = snapshot.root.as_ref().clone();
    let color = evaluate_node_color(&tree, layer_colors);
    solid_tile(color)
}

fn evaluate_node_color(node: &RenderTreeNode, layer_colors: &HashMap<u64, [f32; 4]>) -> [f32; 4] {
    match node {
        RenderTreeNode::Leaf { layer_id, .. } => layer_colors
            .get(layer_id)
            .copied()
            .expect("missing test color for layer"),
        RenderTreeNode::Group { children, .. } => {
            let mut destination = [0.0, 0.0, 0.0, 0.0];
            for child in children.iter() {
                let source = evaluate_node_color(child, layer_colors);
                let blend = match child {
                    RenderTreeNode::Leaf { blend, .. } | RenderTreeNode::Group { blend, .. } => {
                        *blend
                    }
                };

                destination = match blend {
                    BlendMode::Normal => normal_blend_color(source, destination),
                    BlendMode::Multiply => multiply_blend_color(source, destination),
                };
            }
            destination
        }
    }
}

fn normal_blend_color(source: [f32; 4], destination: [f32; 4]) -> [f32; 4] {
    let one_minus_source_alpha = 1.0 - source[3];
    [
        source[0] * source[3] + destination[0] * one_minus_source_alpha,
        source[1] * source[3] + destination[1] * one_minus_source_alpha,
        source[2] * source[3] + destination[2] * one_minus_source_alpha,
        source[3] + destination[3] * one_minus_source_alpha,
    ]
}

fn multiply_blend_color(source: [f32; 4], destination: [f32; 4]) -> [f32; 4] {
    let one_minus_source_alpha = 1.0 - source[3];
    [
        source[0] * destination[0] + destination[0] * one_minus_source_alpha,
        source[1] * destination[1] + destination[1] * one_minus_source_alpha,
        source[2] * destination[2] + destination[2] * one_minus_source_alpha,
        source[3] + destination[3] * one_minus_source_alpha,
    ]
}

fn multiply_color(left: [f32; 4], right: [f32; 4]) -> [f32; 4] {
    [
        left[0] * right[0],
        left[1] * right[1],
        left[2] * right[2],
        left[3] * right[3],
    ]
}

fn solid_tile(color: [f32; 4]) -> Vec<u8> {
    let pixel = [
        float_channel_to_u8(color[0]),
        float_channel_to_u8(color[1]),
        float_channel_to_u8(color[2]),
        float_channel_to_u8(color[3]),
    ];

    let mut bytes = vec![0u8; (TILE_IMAGE as usize) * (TILE_IMAGE as usize) * 4];
    for index in (0..bytes.len()).step_by(4) {
        bytes[index] = pixel[0];
        bytes[index + 1] = pixel[1];
        bytes[index + 2] = pixel[2];
        bytes[index + 3] = pixel[3];
    }
    bytes
}

fn float_channel_to_u8(value: f32) -> u8 {
    let clamped = value.clamp(0.0, 1.0);
    (clamped * 255.0).round() as u8
}

fn image_handle() -> ImageHandle {
    ImageHandle::default()
}

fn allocate_tile_keys(count: usize) -> Vec<TileKey> {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: true,
            })
            .await
            .expect("request test adapter");
        let (device, _queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("renderer.test_device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .expect("request test device");

        let (atlas_store, _gpu) = tiles::TileAtlasStore::new(
            &device,
            tiles::TileAtlasFormat::Rgba8Unorm,
            tiles::TileAtlasUsage::TEXTURE_BINDING
                | tiles::TileAtlasUsage::COPY_DST
                | tiles::TileAtlasUsage::COPY_SRC,
        )
        .expect("create test tile atlas");

        atlas_store
            .reserve_tile_set(u32::try_from(count).expect("test tile key count exceeds u32"))
            .expect("reserve test tile set")
            .iter_keys()
            .collect()
    })
}

#[test]
fn brush_dispatch_does_not_panic_when_endstroke_enqueued_with_pending_chunk() {
    let key = BrushProgramKey {
        brush_id: 10,
        program_revision: 99,
    };

    let mut brush_work_state = BrushWorkState {
        pending_commands: std::collections::VecDeque::new(),
        pending_dab_count: 0,
        carry_credit_dabs: 0,
        prepared_programs: HashMap::new(),
        active_program_by_brush: HashMap::new(),
        active_strokes: HashMap::new(),
        executing_strokes: HashMap::new(),
        reference_sets: HashMap::new(),
        stroke_reference_set: HashMap::new(),
        stroke_target_layer: HashMap::new(),
        ended_strokes_pending_merge: HashMap::new(),
        bound_buffer_tile_keys_by_stroke: HashMap::new(),
    };

    brush_work_state.executing_strokes.insert(1, key);
    brush_work_state
        .bound_buffer_tile_keys_by_stroke
        .insert(1, HashMap::new());
    brush_work_state.stroke_target_layer.insert(1, 1234);

    let chunk =
        render_protocol::BrushDabChunkF32::from_slices(1, &[1.0], &[2.0], &[0.5]).expect("chunk");
    brush_work_state
        .pending_commands
        .push_back(BrushRenderCommand::PushDabChunkF32(chunk));

    // Simulate `enqueue_brush_render_command(EndStroke)` after a chunk is already pending.
    brush_work_state.enqueue_end_stroke(1);

    let pending = brush_work_state
        .pending_commands
        .front()
        .cloned()
        .expect("pending command");
    match pending {
        BrushRenderCommand::PushDabChunkF32(chunk) => {
            let _ = brush_work_state.dispatch_context_for_brush_chunk(chunk.stroke_session_id);
        }
        other => panic!("unexpected pending command: {other:?}"),
    }
}

fn rgba8_texture_upload_bytes_padded(
    width: u32,
    height: u32,
    mut pixel_at: impl FnMut(u32, u32) -> [u8; 4],
) -> (Vec<u8>, u32) {
    // wgpu requires bytes_per_row to be a multiple of 256 for texture uploads/copies.
    let unpadded_bytes_per_row = width.checked_mul(4).expect("rgba8 bytes_per_row overflow");
    let padded_bytes_per_row = unpadded_bytes_per_row
        .checked_add(255)
        .expect("bytes_per_row pad overflow")
        / 256
        * 256;
    let mut bytes = vec![
        0u8;
        (padded_bytes_per_row as usize)
            .checked_mul(height as usize)
            .expect("rgba8 upload buffer size overflow")
    ];
    for y in 0..height {
        for x in 0..width {
            let rgba = pixel_at(x, y);
            let row_start = (y as usize)
                .checked_mul(padded_bytes_per_row as usize)
                .expect("row start overflow");
            let offset = row_start
                .checked_add((x as usize) * 4)
                .expect("pixel offset overflow");
            bytes[offset..offset + 4].copy_from_slice(&rgba);
        }
    }
    (bytes, padded_bytes_per_row)
}

fn rgba8_texture_readback_bytes_padded(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    label: &str,
) -> (Vec<u8>, u32) {
    // wgpu requires bytes_per_row to be a multiple of 256 for texture copies to buffers.
    let unpadded_bytes_per_row = width
        .checked_mul(4)
        .expect("rgba8 readback bytes_per_row overflow");
    let padded_bytes_per_row = unpadded_bytes_per_row
        .checked_add(255)
        .expect("readback bytes_per_row pad overflow")
        / 256
        * 256;
    let buffer_size = (padded_bytes_per_row as u64)
        .checked_mul(height as u64)
        .expect("rgba8 readback buffer size overflow");
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("renderer.test.quadrant.readback_encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    let (sender, receiver) = std::sync::mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).expect("send map result");
        });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll must succeed for readback mapping");
    receiver
        .recv()
        .expect("receive map result")
        .expect("map readback buffer");
    let mapped = readback.slice(..).get_mapped_range();
    let bytes = mapped.to_vec();
    drop(mapped);
    readback.unmap();
    (bytes, padded_bytes_per_row)
}

#[test]
#[ignore = "repro for tile coordinate mapping regression; run explicitly while debugging"]
fn composite_tile_mapping_renders_quadrant_image_exactly() {
    // Repro harness for "tile coordinate mapping" regressions:
    // - Build a 256x256 image composed of 4 differently-colored tiles (2x2).
    // - Render through the slot composite shader into a slot-sized scratch texture.
    // - Copy scratch into a tile atlas (stride layout).
    // - Render through the content composite shader into a 256x256 output texture.
    // - Read back and assert pixel-perfect match.
    pollster::block_on(async {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: true,
            })
            .await
            .expect("request test adapter");
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("renderer.test_device.quadrant_mapping"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .expect("request test device");

        let tile_size = TILE_IMAGE;
        assert_eq!(tile_size, 128, "test assumes TILE_IMAGE=128");
        let tile_stride = TILE_STRIDE;
        assert!(
            tile_stride >= tile_size,
            "tile stride must be at least tile size"
        );

        let content_width = 256u32;
        let content_height = 256u32;
        let tiles_per_row = content_width / tile_size;
        let tiles_per_column = content_height / tile_size;
        assert_eq!(tiles_per_row, 2);
        assert_eq!(tiles_per_column, 2);
        let atlas_width = tiles_per_row * tile_stride;
        let atlas_height = tiles_per_column * tile_stride;

        let layer_atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("renderer.test.quadrant.layer_atlas"),
            size: wgpu::Extent3d {
                width: atlas_width,
                height: atlas_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let (layer_upload, layer_bytes_per_row) =
            rgba8_texture_upload_bytes_padded(atlas_width, atlas_height, |x, y| {
                let tile_x = x / tile_stride;
                let tile_y = y / tile_stride;
                match (tile_x, tile_y) {
                    (0, 0) => [255, 0, 0, 255],   // top-left: red
                    (1, 0) => [0, 255, 0, 255],   // top-right: green
                    (0, 1) => [0, 0, 255, 255],   // bottom-left: blue
                    (1, 1) => [255, 255, 0, 255], // bottom-right: yellow
                    _ => [0, 0, 0, 255],
                }
            });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &layer_atlas,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &layer_upload,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(layer_bytes_per_row),
                rows_per_image: Some(atlas_height),
            },
            wgpu::Extent3d {
                width: atlas_width,
                height: atlas_height,
                depth_or_array_layers: 1,
            },
        );

        let layer_atlas_view = layer_atlas.create_view(&wgpu::TextureViewDescriptor {
            label: Some("renderer.test.quadrant.layer_atlas.view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        let group_atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("renderer.test.quadrant.group_atlas"),
            size: wgpu::Extent3d {
                width: atlas_width,
                height: atlas_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let group_atlas_view = group_atlas.create_view(&wgpu::TextureViewDescriptor {
            label: Some("renderer.test.quadrant.group_atlas.view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        let scratch = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("renderer.test.quadrant.scratch"),
            size: wgpu::Extent3d {
                width: atlas_width,
                height: atlas_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let scratch_view = scratch.create_view(&wgpu::TextureViewDescriptor::default());

        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("renderer.test.quadrant.output"),
            size: wgpu::Extent3d {
                width: content_width,
                height: content_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());

        let view_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("renderer.test.quadrant.view_uniform"),
            size: std::mem::size_of::<TransformMatrix4x4>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let tile_instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("renderer.test.quadrant.tile_instances"),
            size: (std::mem::size_of::<TileInstanceGpu>() * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let instances = [
            TileInstanceGpu {
                document_x: 0.0,
                document_y: 0.0,
                atlas_layer: 0.0,
                tile_index: 0,
                _padding0: 0,
            },
            TileInstanceGpu {
                document_x: tile_size as f32,
                document_y: 0.0,
                atlas_layer: 0.0,
                tile_index: 1,
                _padding0: 0,
            },
            TileInstanceGpu {
                document_x: 0.0,
                document_y: tile_size as f32,
                atlas_layer: 0.0,
                tile_index: 2,
                _padding0: 0,
            },
            TileInstanceGpu {
                document_x: tile_size as f32,
                document_y: tile_size as f32,
                atlas_layer: 0.0,
                tile_index: 3,
                _padding0: 0,
            },
        ];
        queue.write_buffer(&tile_instance_buffer, 0, bytemuck::cast_slice(&instances));

        let tile_texture_manager = TileTextureManagerGpu {
            atlas_width: atlas_width as f32,
            atlas_height: atlas_height as f32,
            tiles_per_row,
            tiles_per_column,
            tile_size: tile_size as f32,
            tile_stride: tile_stride as f32,
            tile_gutter: TILE_GUTTER as f32,
            _padding0: 0.0,
        };
        let manager_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("renderer.test.quadrant.tile_texture_manager"),
            size: std::mem::size_of::<TileTextureManagerGpu>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(
            &manager_buffer,
            0,
            bytemuck::bytes_of(&tile_texture_manager),
        );

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("renderer.test.quadrant.sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let per_frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("renderer.test.quadrant.per_frame_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("renderer.test.quadrant.atlas_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let per_frame_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("renderer.test.quadrant.per_frame"),
            layout: &per_frame_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: view_uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: tile_instance_buffer.as_entire_binding(),
                },
            ],
        });
        let layer_atlas_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("renderer.test.quadrant.layer_atlas_bind_group"),
            layout: &atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&layer_atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: manager_buffer.as_entire_binding(),
                },
            ],
        });
        let group_atlas_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("renderer.test.quadrant.group_atlas_bind_group"),
            layout: &atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&group_atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: manager_buffer.as_entire_binding(),
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("renderer.test.quadrant.pipeline_layout"),
            bind_group_layouts: &[&per_frame_layout, &atlas_layout],
            immediate_size: 0,
        });
        let slot_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("renderer.test.quadrant.slot_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("tile_composite_slot.wgsl").into()),
        });
        let content_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("renderer.test.quadrant.content_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("tile_composite.wgsl").into()),
        });
        let slot_pipeline = crate::renderer_pipeline::create_composite_pipeline(
            &device,
            &pipeline_layout,
            &slot_shader,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::BlendState::REPLACE,
            "renderer.test.quadrant.slot_pipeline",
        );
        let content_pipeline = crate::renderer_pipeline::create_composite_pipeline(
            &device,
            &pipeline_layout,
            &content_shader,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::BlendState::REPLACE,
            "renderer.test.quadrant.content_pipeline",
        );

        // Pass 1: layer atlas -> scratch (slot composite space).
        queue.write_buffer(
            &view_uniform_buffer,
            0,
            bytemuck::bytes_of(&document_clip_matrix_from_size(atlas_width, atlas_height)),
        );
        {
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("renderer.test.quadrant.encoder.slot"),
            });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("renderer.test.quadrant.pass.slot"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &scratch_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&slot_pipeline);
                pass.set_bind_group(0, &per_frame_bind_group, &[]);
                pass.set_bind_group(1, &layer_atlas_bind_group, &[]);
                pass.draw(0..6, 0..4);
            }
            queue.submit(Some(encoder.finish()));
        }

        // Stage readback: validate slot pass output in scratch.
        let (scratch_bytes, scratch_bpr) = rgba8_texture_readback_bytes_padded(
            &device,
            &queue,
            &scratch,
            atlas_width,
            atlas_height,
            "renderer.test.quadrant.readback.scratch",
        );
        let scratch_pixel = |x: u32, y: u32| -> [u8; 4] {
            let offset = (y as usize) * (scratch_bpr as usize) + (x as usize) * 4;
            [
                scratch_bytes[offset],
                scratch_bytes[offset + 1],
                scratch_bytes[offset + 2],
                scratch_bytes[offset + 3],
            ]
        };
        assert_eq!(
            scratch_pixel(0, 0),
            [255, 0, 0, 255],
            "scratch top-left must be red"
        );
        assert_eq!(
            scratch_pixel(tile_stride, 0),
            [0, 255, 0, 255],
            "scratch top-right slot origin must be green"
        );
        assert_eq!(
            scratch_pixel(0, tile_stride),
            [0, 0, 255, 255],
            "scratch bottom-left slot origin must be blue"
        );
        assert_eq!(
            scratch_pixel(tile_stride, tile_stride),
            [255, 255, 0, 255],
            "scratch bottom-right slot origin must be yellow"
        );

        // Mimic group cache tile-slot writeback into an atlas + content pass in a separate submission.
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("renderer.test.quadrant.encoder.content"),
        });
        for tile_y in 0..tiles_per_column {
            for tile_x in 0..tiles_per_row {
                encoder.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &scratch,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: tile_x * tile_stride,
                            y: tile_y * tile_stride,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &group_atlas,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: tile_x * tile_stride,
                            y: tile_y * tile_stride,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: tile_stride,
                        height: tile_stride,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }

        // Pass 2: group atlas -> output (content composite space).
        queue.write_buffer(
            &view_uniform_buffer,
            0,
            bytemuck::bytes_of(&document_clip_matrix_from_size(
                content_width,
                content_height,
            )),
        );
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("renderer.test.quadrant.pass.content"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &output_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&content_pipeline);
            pass.set_bind_group(0, &per_frame_bind_group, &[]);
            pass.set_bind_group(1, &group_atlas_bind_group, &[]);
            pass.draw(0..6, 0..4);
        }
        queue.submit(Some(encoder.finish()));
        let (output_bytes, output_bpr) = rgba8_texture_readback_bytes_padded(
            &device,
            &queue,
            &output,
            content_width,
            content_height,
            "renderer.test.quadrant.readback.output",
        );
        let expected_pixel = |x: u32, y: u32| -> [u8; 4] {
            match (x < tile_size, y < tile_size) {
                (true, true) => [255, 0, 0, 255],
                (false, true) => [0, 255, 0, 255],
                (true, false) => [0, 0, 255, 255],
                (false, false) => [255, 255, 0, 255],
            }
        };

        for y in 0..content_height {
            for x in 0..content_width {
                let offset = (y as usize) * (output_bpr as usize) + (x as usize) * 4;
                let got = [
                    output_bytes[offset],
                    output_bytes[offset + 1],
                    output_bytes[offset + 2],
                    output_bytes[offset + 3],
                ];
                let expected = expected_pixel(x, y);
                assert_eq!(
                    got, expected,
                    "pixel mismatch at ({}, {}): got={:?} expected={:?}",
                    x, y, got, expected
                );
            }
        }
    });
}

#[test]
#[ignore = "repro for nested group-cache mapping regressions; run explicitly while debugging"]
fn composite_tile_mapping_survives_nested_group_cache_levels() {
    // This is closer to the renderer's real execution structure after introducing per-layer groups:
    // - leaf(layer atlas) -> layer group scratch (slot) -> group atlas A (tile copy)
    // - group atlas A -> root scratch (slot) -> group atlas B (tile copy)
    // - group atlas B -> output (content)
    pollster::block_on(async {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: true,
            })
            .await
            .expect("request test adapter");
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("renderer.test_device.nested_group_mapping"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .expect("request test device");

        let tile_size = TILE_IMAGE;
        assert_eq!(tile_size, 128, "test assumes TILE_IMAGE=128");
        let tile_stride = TILE_STRIDE;

        let content_width = 256u32;
        let content_height = 256u32;
        let tiles_per_row_doc = content_width / tile_size;
        let tiles_per_col_doc = content_height / tile_size;
        assert_eq!(tiles_per_row_doc, 2);
        assert_eq!(tiles_per_col_doc, 2);

        // Use an atlas layout that is NOT a tight 2x2 grid, and choose sparse tile indices to
        // ensure we don't accidentally depend on sequential indexing.
        let atlas_tiles_per_row = 8u32;
        let atlas_tiles_per_col = 8u32;
        let atlas_width = atlas_tiles_per_row * tile_stride;
        let atlas_height = atlas_tiles_per_col * tile_stride;

        let tile_indices_layer = [0u32, 7, 8, 63];
        let tile_indices_group_a = [1u32, 6, 9, 62];
        let tile_indices_group_b = [2u32, 5, 10, 61];

        let make_atlas = |label: &str| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: atlas_width,
                    height: atlas_height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
        };

        let layer_atlas = make_atlas("renderer.test.nested.layer_atlas");
        let group_atlas_a = make_atlas("renderer.test.nested.group_atlas_a");
        let group_atlas_b = make_atlas("renderer.test.nested.group_atlas_b");

        let layer_atlas_view = layer_atlas.create_view(&wgpu::TextureViewDescriptor {
            label: Some("renderer.test.nested.layer_atlas.view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let group_atlas_a_view = group_atlas_a.create_view(&wgpu::TextureViewDescriptor {
            label: Some("renderer.test.nested.group_atlas_a.view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let group_atlas_b_view = group_atlas_b.create_view(&wgpu::TextureViewDescriptor {
            label: Some("renderer.test.nested.group_atlas_b.view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        // Upload per-tile slot regions into the layer atlas at sparse indices.
        let tile_slot_bytes = |rgba: [u8; 4]| -> (Vec<u8>, u32) {
            rgba8_texture_upload_bytes_padded(tile_stride, tile_stride, |_x, _y| rgba)
        };
        let colors = [
            [255, 0, 0, 255],   // TL
            [0, 255, 0, 255],   // TR
            [0, 0, 255, 255],   // BL
            [255, 255, 0, 255], // BR
        ];
        for (slot_i, tile_index) in tile_indices_layer.iter().copied().enumerate() {
            let tile_x = tile_index % atlas_tiles_per_row;
            let tile_y = tile_index / atlas_tiles_per_row;
            let (bytes, bpr) = tile_slot_bytes(colors[slot_i]);
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &layer_atlas,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: tile_x * tile_stride,
                        y: tile_y * tile_stride,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(tile_stride),
                },
                wgpu::Extent3d {
                    width: tile_stride,
                    height: tile_stride,
                    depth_or_array_layers: 1,
                },
            );
        }

        let view_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("renderer.test.nested.view_uniform"),
            size: std::mem::size_of::<TransformMatrix4x4>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let tile_texture_manager = TileTextureManagerGpu {
            atlas_width: atlas_width as f32,
            atlas_height: atlas_height as f32,
            tiles_per_row: atlas_tiles_per_row,
            tiles_per_column: atlas_tiles_per_col,
            tile_size: tile_size as f32,
            tile_stride: tile_stride as f32,
            tile_gutter: TILE_GUTTER as f32,
            _padding0: 0.0,
        };
        let manager_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("renderer.test.nested.tile_texture_manager"),
            size: std::mem::size_of::<TileTextureManagerGpu>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(
            &manager_buffer,
            0,
            bytemuck::bytes_of(&tile_texture_manager),
        );

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("renderer.test.nested.sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let tile_instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("renderer.test.nested.tile_instances"),
            size: (std::mem::size_of::<TileInstanceGpu>() * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let instances_for_indices = |indices: [u32; 4]| -> [TileInstanceGpu; 4] {
            [
                TileInstanceGpu {
                    document_x: 0.0,
                    document_y: 0.0,
                    atlas_layer: 0.0,
                    tile_index: indices[0],
                    _padding0: 0,
                },
                TileInstanceGpu {
                    document_x: tile_size as f32,
                    document_y: 0.0,
                    atlas_layer: 0.0,
                    tile_index: indices[1],
                    _padding0: 0,
                },
                TileInstanceGpu {
                    document_x: 0.0,
                    document_y: tile_size as f32,
                    atlas_layer: 0.0,
                    tile_index: indices[2],
                    _padding0: 0,
                },
                TileInstanceGpu {
                    document_x: tile_size as f32,
                    document_y: tile_size as f32,
                    atlas_layer: 0.0,
                    tile_index: indices[3],
                    _padding0: 0,
                },
            ]
        };

        let per_frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("renderer.test.nested.per_frame_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("renderer.test.nested.atlas_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let per_frame_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("renderer.test.nested.per_frame"),
            layout: &per_frame_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: view_uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: tile_instance_buffer.as_entire_binding(),
                },
            ],
        });
        let atlas_bind_group = |view: &wgpu::TextureView, label: &str| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &atlas_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: manager_buffer.as_entire_binding(),
                    },
                ],
            })
        };
        let layer_bind_group = atlas_bind_group(&layer_atlas_view, "renderer.test.nested.layer_bg");
        let group_a_bind_group =
            atlas_bind_group(&group_atlas_a_view, "renderer.test.nested.group_a_bg");
        let group_b_bind_group =
            atlas_bind_group(&group_atlas_b_view, "renderer.test.nested.group_b_bg");

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("renderer.test.nested.pipeline_layout"),
            bind_group_layouts: &[&per_frame_layout, &atlas_layout],
            immediate_size: 0,
        });
        let slot_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("renderer.test.nested.slot_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("tile_composite_slot.wgsl").into()),
        });
        let content_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("renderer.test.nested.content_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("tile_composite.wgsl").into()),
        });
        let slot_pipeline = crate::renderer_pipeline::create_composite_pipeline(
            &device,
            &pipeline_layout,
            &slot_shader,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::BlendState::REPLACE,
            "renderer.test.nested.slot_pipeline",
        );
        let content_pipeline = crate::renderer_pipeline::create_composite_pipeline(
            &device,
            &pipeline_layout,
            &content_shader,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::BlendState::REPLACE,
            "renderer.test.nested.content_pipeline",
        );

        let make_scratch = |label: &str| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: tiles_per_row_doc * tile_stride,
                    height: tiles_per_col_doc * tile_stride,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };

        let layer_scratch = make_scratch("renderer.test.nested.layer_scratch");
        let root_scratch = make_scratch("renderer.test.nested.root_scratch");
        let layer_scratch_view = layer_scratch.create_view(&wgpu::TextureViewDescriptor::default());
        let root_scratch_view = root_scratch.create_view(&wgpu::TextureViewDescriptor::default());

        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("renderer.test.nested.output"),
            size: wgpu::Extent3d {
                width: content_width,
                height: content_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());

        // Stage 1: layer atlas -> layer scratch (slot), then copy to group atlas A at sparse indices.
        queue.write_buffer(
            &view_uniform_buffer,
            0,
            bytemuck::bytes_of(&document_clip_matrix_from_size(
                tiles_per_row_doc * tile_stride,
                tiles_per_col_doc * tile_stride,
            )),
        );
        queue.write_buffer(
            &tile_instance_buffer,
            0,
            bytemuck::cast_slice(&instances_for_indices(tile_indices_layer)),
        );
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("renderer.test.nested.encoder.stage1"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("renderer.test.nested.pass.stage1_slot"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &layer_scratch_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&slot_pipeline);
            pass.set_bind_group(0, &per_frame_bind_group, &[]);
            pass.set_bind_group(1, &layer_bind_group, &[]);
            pass.draw(0..6, 0..4);
        }
        for (coord_i, dest_tile_index) in tile_indices_group_a.iter().copied().enumerate() {
            let src_tile_x = (coord_i as u32) % tiles_per_row_doc;
            let src_tile_y = (coord_i as u32) / tiles_per_row_doc;
            let dest_tile_x = dest_tile_index % atlas_tiles_per_row;
            let dest_tile_y = dest_tile_index / atlas_tiles_per_row;
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &layer_scratch,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: src_tile_x * tile_stride,
                        y: src_tile_y * tile_stride,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &group_atlas_a,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: dest_tile_x * tile_stride,
                        y: dest_tile_y * tile_stride,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: tile_stride,
                    height: tile_stride,
                    depth_or_array_layers: 1,
                },
            );
        }
        queue.submit(Some(encoder.finish()));

        let sample_from_padded = |bytes: &[u8], bpr: u32, x: u32, y: u32| -> [u8; 4] {
            let offset = (y as usize) * (bpr as usize) + (x as usize) * 4;
            [
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ]
        };

        // Stage readback: layer_scratch must contain the expected colors at slot origins.
        let (layer_scratch_bytes, layer_scratch_bpr) = rgba8_texture_readback_bytes_padded(
            &device,
            &queue,
            &layer_scratch,
            tiles_per_row_doc * tile_stride,
            tiles_per_col_doc * tile_stride,
            "renderer.test.nested.readback.layer_scratch",
        );
        assert_eq!(
            sample_from_padded(&layer_scratch_bytes, layer_scratch_bpr, 0, 0),
            [255, 0, 0, 255],
            "layer_scratch TL must be red"
        );
        assert_eq!(
            sample_from_padded(&layer_scratch_bytes, layer_scratch_bpr, tile_stride, 0),
            [0, 255, 0, 255],
            "layer_scratch TR must be green"
        );
        assert_eq!(
            sample_from_padded(&layer_scratch_bytes, layer_scratch_bpr, 0, tile_stride),
            [0, 0, 255, 255],
            "layer_scratch BL must be blue"
        );
        assert_eq!(
            sample_from_padded(
                &layer_scratch_bytes,
                layer_scratch_bpr,
                tile_stride,
                tile_stride
            ),
            [255, 255, 0, 255],
            "layer_scratch BR must be yellow"
        );

        // Stage readback: group_atlas_a must contain the copied slots at the chosen sparse indices.
        let (group_a_bytes, group_a_bpr) = rgba8_texture_readback_bytes_padded(
            &device,
            &queue,
            &group_atlas_a,
            atlas_width,
            atlas_height,
            "renderer.test.nested.readback.group_atlas_a",
        );
        for (coord_i, tile_index) in tile_indices_group_a.iter().copied().enumerate() {
            let tx = tile_index % atlas_tiles_per_row;
            let ty = tile_index / atlas_tiles_per_row;
            let got = sample_from_padded(
                &group_a_bytes,
                group_a_bpr,
                tx * tile_stride,
                ty * tile_stride,
            );
            assert_eq!(
                got, colors[coord_i],
                "group_atlas_a slot origin mismatch for coord_i={} tile_index={}",
                coord_i, tile_index
            );
        }

        // Stage 2: group atlas A -> root scratch (slot), then copy to group atlas B at sparse indices.
        queue.write_buffer(
            &view_uniform_buffer,
            0,
            bytemuck::bytes_of(&document_clip_matrix_from_size(
                tiles_per_row_doc * tile_stride,
                tiles_per_col_doc * tile_stride,
            )),
        );
        queue.write_buffer(
            &tile_instance_buffer,
            0,
            bytemuck::cast_slice(&instances_for_indices(tile_indices_group_a)),
        );
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("renderer.test.nested.encoder.stage2"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("renderer.test.nested.pass.stage2_slot"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &root_scratch_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&slot_pipeline);
            pass.set_bind_group(0, &per_frame_bind_group, &[]);
            pass.set_bind_group(1, &group_a_bind_group, &[]);
            pass.draw(0..6, 0..4);
        }
        for (coord_i, dest_tile_index) in tile_indices_group_b.iter().copied().enumerate() {
            let src_tile_x = (coord_i as u32) % tiles_per_row_doc;
            let src_tile_y = (coord_i as u32) / tiles_per_row_doc;
            let dest_tile_x = dest_tile_index % atlas_tiles_per_row;
            let dest_tile_y = dest_tile_index / atlas_tiles_per_row;
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &root_scratch,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: src_tile_x * tile_stride,
                        y: src_tile_y * tile_stride,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &group_atlas_b,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: dest_tile_x * tile_stride,
                        y: dest_tile_y * tile_stride,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: tile_stride,
                    height: tile_stride,
                    depth_or_array_layers: 1,
                },
            );
        }

        queue.submit(Some(encoder.finish()));

        // Stage readback: root_scratch must match expected colors at slot origins.
        let (root_scratch_bytes, root_scratch_bpr) = rgba8_texture_readback_bytes_padded(
            &device,
            &queue,
            &root_scratch,
            tiles_per_row_doc * tile_stride,
            tiles_per_col_doc * tile_stride,
            "renderer.test.nested.readback.root_scratch",
        );
        assert_eq!(
            sample_from_padded(&root_scratch_bytes, root_scratch_bpr, 0, 0),
            [255, 0, 0, 255],
            "root_scratch TL must be red"
        );
        assert_eq!(
            sample_from_padded(&root_scratch_bytes, root_scratch_bpr, tile_stride, 0),
            [0, 255, 0, 255],
            "root_scratch TR must be green"
        );
        assert_eq!(
            sample_from_padded(&root_scratch_bytes, root_scratch_bpr, 0, tile_stride),
            [0, 0, 255, 255],
            "root_scratch BL must be blue"
        );
        assert_eq!(
            sample_from_padded(
                &root_scratch_bytes,
                root_scratch_bpr,
                tile_stride,
                tile_stride
            ),
            [255, 255, 0, 255],
            "root_scratch BR must be yellow"
        );

        // Stage readback: group_atlas_b must contain the copied slots at the chosen sparse indices.
        let (group_b_bytes, group_b_bpr) = rgba8_texture_readback_bytes_padded(
            &device,
            &queue,
            &group_atlas_b,
            atlas_width,
            atlas_height,
            "renderer.test.nested.readback.group_atlas_b",
        );
        for (coord_i, tile_index) in tile_indices_group_b.iter().copied().enumerate() {
            let tx = tile_index % atlas_tiles_per_row;
            let ty = tile_index / atlas_tiles_per_row;
            let got = sample_from_padded(
                &group_b_bytes,
                group_b_bpr,
                tx * tile_stride,
                ty * tile_stride,
            );
            assert_eq!(
                got, colors[coord_i],
                "group_atlas_b slot origin mismatch for coord_i={} tile_index={}",
                coord_i, tile_index
            );
        }

        // Stage 3: group atlas B -> output (content).
        queue.write_buffer(
            &view_uniform_buffer,
            0,
            bytemuck::bytes_of(&document_clip_matrix_from_size(
                content_width,
                content_height,
            )),
        );
        queue.write_buffer(
            &tile_instance_buffer,
            0,
            bytemuck::cast_slice(&instances_for_indices(tile_indices_group_b)),
        );
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("renderer.test.nested.encoder.stage3"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("renderer.test.nested.pass.stage3_content"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &output_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&content_pipeline);
            pass.set_bind_group(0, &per_frame_bind_group, &[]);
            pass.set_bind_group(1, &group_b_bind_group, &[]);
            pass.draw(0..6, 0..4);
        }
        queue.submit(Some(encoder.finish()));

        let (output_bytes, output_bpr) = rgba8_texture_readback_bytes_padded(
            &device,
            &queue,
            &output,
            content_width,
            content_height,
            "renderer.test.nested.readback.output",
        );
        let expected_pixel = |x: u32, y: u32| -> [u8; 4] {
            match (x < tile_size, y < tile_size) {
                (true, true) => [255, 0, 0, 255],
                (false, true) => [0, 255, 0, 255],
                (true, false) => [0, 0, 255, 255],
                (false, false) => [255, 255, 0, 255],
            }
        };
        for y in 0..content_height {
            for x in 0..content_width {
                let offset = (y as usize) * (output_bpr as usize) + (x as usize) * 4;
                let got = [
                    output_bytes[offset],
                    output_bytes[offset + 1],
                    output_bytes[offset + 2],
                    output_bytes[offset + 3],
                ];
                let expected = expected_pixel(x, y);
                assert_eq!(
                    got, expected,
                    "nested mapping pixel mismatch at ({}, {}): got={:?} expected={:?}",
                    x, y, got, expected
                );
            }
        }
    });
}
