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
        image_handle: image_handle(),
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
        image_handle: image_handle(),
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
        cached_leaf.image_handle,
    ));
}

#[test]
fn leaf_should_not_rebuild_when_cache_matches_and_clean() {
    let image_handle = image_handle();
    let cached_leaf = CachedLeafDraw {
        blend: BlendMode::Normal,
        image_handle,
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
        image_handle,
    ));
}

#[test]
fn cached_leaf_partial_replace_keeps_index_consistent() {
    let mut cached_leaf = CachedLeafDraw {
        blend: BlendMode::Normal,
        image_handle: image_handle(),
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
                    document_x: TILE_SIZE as f32,
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
        (TILE_SIZE, TILE_SIZE)
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
                max_x: (TILE_SIZE as i32) * 2,
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
            max_x: (TILE_SIZE as i32) * 2,
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
        (TILE_SIZE, TILE_SIZE)
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
}

#[test]
fn build_leaf_tile_draw_instances_keeps_blend_and_filters_unresolved_tiles() {
    let tile_keys = allocate_tile_keys(2);
    let resolver = FakeResolver {
        emit_tiles: true,
        first_tile_key: Some(tile_keys[0]),
        second_tile_key: Some(tile_keys[1]),
        ..Default::default()
    };

    let draw_instances =
        build_leaf_tile_draw_instances(BlendMode::Multiply, image_handle(), &resolver);

    assert_eq!(resolver.visit_calls.get(), 1);
    assert_eq!(resolver.resolve_calls.get(), 2);
    assert_eq!(draw_instances.len(), 1);
    let draw_instance = draw_instances[0];
    assert_eq!(draw_instance.blend_mode, BlendMode::Multiply);
    assert_eq!(draw_instance.tile.document_x, TILE_SIZE as f32);
    assert_eq!(draw_instance.tile.document_y, (2 * TILE_SIZE) as f32);
    assert_eq!(draw_instance.tile.atlas_layer, 2.0);
}

#[test]
fn build_leaf_tile_draw_instances_for_tiles_filters_to_requested_tiles() {
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
        image_handle(),
        &resolver,
        &requested_tiles,
    );

    assert_eq!(resolver.visit_calls.get(), 1);
    assert_eq!(resolver.resolve_calls.get(), 1);
    assert_eq!(draw_instances.len(), 0);
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
            min_x: TILE_SIZE as i32,
            min_y: (2 * TILE_SIZE) as i32,
            max_x: (TILE_SIZE as i32) + 1,
            max_y: (2 * TILE_SIZE) as i32 + 1,
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
            max_x: (2 * TILE_SIZE) as i32,
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
            min_x: (3 * TILE_SIZE) as i32,
            min_y: (2 * TILE_SIZE) as i32,
            max_x: (3 * TILE_SIZE) as i32 + 1,
            max_y: (2 * TILE_SIZE) as i32 + 1,
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
        max_x: TILE_SIZE as i32,
        max_y: TILE_SIZE as i32,
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
        min_x: TILE_SIZE as i32,
        min_y: 0,
        max_x: (TILE_SIZE as i32) + 1,
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
    let extent = group_cache_extent_from_document_size(TILE_SIZE + 7, TILE_SIZE * 2 + 1);
    assert_eq!(extent.width, TILE_SIZE * 2);
    assert_eq!(extent.height, TILE_SIZE * 3);
    assert_eq!(extent.depth_or_array_layers, 1);
}

#[test]
fn group_cache_slot_extent_uses_tile_stride() {
    let extent = group_cache_slot_extent_from_document_size(TILE_SIZE + 7, TILE_SIZE * 2 + 1);
    assert_eq!(extent.width, TILE_STRIDE * 2);
    assert_eq!(extent.height, TILE_STRIDE * 3);
    assert_eq!(extent.depth_or_array_layers, 1);
}

#[test]
fn group_tile_grid_rounds_up_to_tile_boundaries() {
    let (tiles_per_row, tiles_per_column) =
        group_tile_grid_from_document_size(TILE_SIZE + 7, TILE_SIZE * 2 + 1);
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

    let mut bytes = vec![0u8; (TILE_SIZE as usize) * (TILE_SIZE as usize) * 4];
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
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
        )
        .expect("create test tile atlas");

        atlas_store
            .reserve_tile_set(u32::try_from(count).expect("test tile key count exceeds u32"))
            .expect("reserve test tile set")
            .iter_keys()
            .collect()
    })
}
