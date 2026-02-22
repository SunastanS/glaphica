//! View-operation ingestion and state mutation.
//!
//! This module drains `RenderOp` commands and applies them to state compartments,
//! while coordinating side effects such as cache retention and cache invalidation.

use render_protocol::{
    RenderNodeSnapshot, RenderOp, RenderStepSupportMatrix, RenderTreeSnapshot, TransformMatrix4x4,
    Viewport,
};

use crate::{
    CacheState, DirtyRect, DirtyStateStore, FrameState, PresentError, Renderer, ViewState,
};

struct DropStaleWorkResult {
    state_changed: bool,
    clear_group_target_cache: bool,
}

fn render_node_semantics_equal_ignoring_image_handle(
    left: &RenderNodeSnapshot,
    right: &RenderNodeSnapshot,
) -> bool {
    match (left, right) {
        (
            RenderNodeSnapshot::Leaf {
                layer_id: left_layer_id,
                blend: left_blend,
                ..
            },
            RenderNodeSnapshot::Leaf {
                layer_id: right_layer_id,
                blend: right_blend,
                ..
            },
        ) => left_layer_id == right_layer_id && left_blend == right_blend,
        (
            RenderNodeSnapshot::Group {
                group_id: left_group_id,
                blend: left_blend,
                children: left_children,
            },
            RenderNodeSnapshot::Group {
                group_id: right_group_id,
                blend: right_blend,
                children: right_children,
            },
        ) => {
            left_group_id == right_group_id
                && left_blend == right_blend
                && left_children.len() == right_children.len()
                && left_children.iter().zip(right_children.iter()).all(
                    |(left_child, right_child)| {
                        render_node_semantics_equal_ignoring_image_handle(left_child, right_child)
                    },
                )
        }
        _ => false,
    }
}

fn should_force_document_composite_dirty(
    current_snapshot: Option<&RenderTreeSnapshot>,
    incoming_snapshot: &RenderTreeSnapshot,
) -> bool {
    let Some(current_snapshot) = current_snapshot else {
        return true;
    };
    !render_node_semantics_equal_ignoring_image_handle(
        current_snapshot.root.as_ref(),
        incoming_snapshot.root.as_ref(),
    )
}

fn should_accept_bound_snapshot(view_state: &ViewState, snapshot: &RenderTreeSnapshot) -> bool {
    snapshot.revision >= view_state.drop_before_revision
}

fn apply_view_matrix(view_state: &mut ViewState, matrix: TransformMatrix4x4) -> bool {
    if view_state.view_matrix == matrix {
        return false;
    }
    view_state.view_matrix = matrix;
    view_state.view_matrix_dirty = true;
    true
}

fn apply_viewport(view_state: &mut ViewState, viewport: Viewport) -> bool {
    if view_state.viewport == Some(viewport) {
        return false;
    }
    view_state.viewport = Some(viewport);
    true
}

fn apply_brush_command_quota(view_state: &mut ViewState, max_commands: u32) -> bool {
    if view_state.brush_command_quota == max_commands {
        return false;
    }
    view_state.brush_command_quota = max_commands;
    true
}

fn apply_present_request(view_state: &mut ViewState) {
    view_state.present_requested = true;
}

fn apply_bound_snapshot(frame_state: &mut FrameState, snapshot: RenderTreeSnapshot) {
    assert!(
        matches!(
            snapshot.root.as_ref(),
            RenderNodeSnapshot::Group { group_id: 0, .. }
        ),
        "render tree root must be group 0"
    );
    let force_document_composite_dirty =
        should_force_document_composite_dirty(frame_state.bound_tree.as_ref(), &snapshot);
    frame_state.bound_tree = Some(snapshot);
    frame_state.render_tree_dirty = true;
    if force_document_composite_dirty {
        frame_state
            .dirty_state_store
            .mark_document_composite_dirty();
    }
}

fn drop_stale_work_before_revision(
    view_state: &mut ViewState,
    frame_state: &mut FrameState,
    cache_state: &mut CacheState,
    revision: u64,
) -> DropStaleWorkResult {
    let mut state_changed = false;
    if view_state.drop_before_revision != revision {
        view_state.drop_before_revision = revision;
        state_changed = true;
    }

    if frame_state
        .bound_tree
        .as_ref()
        .is_some_and(|snapshot| snapshot.revision < revision)
    {
        frame_state.bound_tree = None;
        frame_state.cached_render_tree = None;
        frame_state.render_tree_dirty = false;
        cache_state.leaf_draw_cache.clear();
        frame_state.dirty_state_store = DirtyStateStore::with_document_dirty(true);
        state_changed = true;
        return DropStaleWorkResult {
            state_changed,
            clear_group_target_cache: true,
        };
    }

    DropStaleWorkResult {
        state_changed,
        clear_group_target_cache: false,
    }
}

impl Renderer {
    pub fn drain_view_ops(&mut self) {
        while let Ok(operation) = self.input_state.view_receiver.try_recv() {
            self.apply_view_op(operation);
        }
    }

    pub(super) fn apply_view_op(&mut self, operation: RenderOp) {
        let mut state_changed = false;
        match operation {
            RenderOp::SetViewTransform { matrix } => {
                state_changed |= apply_view_matrix(&mut self.view_state, matrix);
            }
            RenderOp::SetViewport(viewport) => {
                state_changed |= apply_viewport(&mut self.view_state, viewport);
            }
            RenderOp::BindRenderTree(snapshot) => {
                if should_accept_bound_snapshot(&self.view_state, &snapshot) {
                    eprintln!(
                        "[renderer] bind render tree: revision={} accepted=true",
                        snapshot.revision
                    );
                    snapshot
                        .validate_executable(
                            &RenderStepSupportMatrix::current_executable_semantics(),
                        )
                        .unwrap_or_else(|error| {
                            panic!(
                                "bound render steps include unsupported feature at step {}: {:?}",
                                error.step_index, error.reason
                            )
                        });
                    self.retain_live_leaf_caches(&snapshot);
                    self.retain_live_group_targets(&snapshot);
                    apply_bound_snapshot(&mut self.frame_state, snapshot);
                    state_changed = true;
                } else {
                    eprintln!("[renderer] bind render tree: accepted=false");
                }
            }
            RenderOp::SetBrushCommandQuota { max_commands } => {
                state_changed |= apply_brush_command_quota(&mut self.view_state, max_commands);
            }
            RenderOp::DropStaleWorkBeforeRevision { revision } => {
                let stale_work_result = drop_stale_work_before_revision(
                    &mut self.view_state,
                    &mut self.frame_state,
                    &mut self.cache_state,
                    revision,
                );
                if stale_work_result.clear_group_target_cache {
                    self.clear_group_target_cache();
                }
                state_changed |= stale_work_result.state_changed;
            }
            RenderOp::PresentToSurface => {
                apply_present_request(&mut self.view_state);
                state_changed = true;
            }
        }
        if state_changed {
            self.frame_state.frame_sync.note_state_change();
        }
    }

    #[allow(dead_code)]
    pub(crate) fn take_present_request(&mut self) -> bool {
        let requested = self.view_state.present_requested;
        self.view_state.present_requested = false;
        requested
    }

    #[allow(dead_code)]
    pub(crate) fn brush_command_quota(&self) -> u32 {
        self.view_state.brush_command_quota
    }

    #[allow(dead_code)]
    pub(crate) fn viewport(&self) -> Option<Viewport> {
        self.view_state.viewport
    }

    #[allow(dead_code)]
    pub(crate) fn bound_tree(&self) -> Option<&RenderTreeSnapshot> {
        self.frame_state.bound_tree.as_ref()
    }

    #[allow(dead_code)]
    pub(crate) fn view_matrix(&self) -> TransformMatrix4x4 {
        self.view_state.view_matrix
    }

    #[allow(dead_code)]
    pub(crate) fn mark_layer_dirty_rect(&mut self, layer_id: u64, dirty_rect: DirtyRect) {
        if self
            .frame_state
            .dirty_state_store
            .mark_layer_rect(layer_id, dirty_rect)
        {
            self.frame_state.frame_sync.note_state_change();
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if self.gpu_state.surface_config.width == width
            && self.gpu_state.surface_config.height == height
        {
            return;
        }
        self.gpu_state.surface_config.width = width;
        self.gpu_state.surface_config.height = height;
        self.gpu_state
            .surface
            .configure(&self.gpu_state.device, &self.gpu_state.surface_config);
        self.frame_state.frame_sync.note_state_change();
    }

    #[allow(dead_code)]
    pub(crate) fn present(&mut self) -> Result<(), PresentError> {
        let next_frame_id = self
            .frame_state
            .frame_sync
            .last_committed_frame_id
            .map_or(0, |frame_id| frame_id.saturating_add(1));
        self.present_frame(next_frame_id)
    }
}
