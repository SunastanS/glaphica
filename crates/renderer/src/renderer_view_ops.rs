use render_protocol::{
    RenderOp, RenderStepSnapshot, RenderStepSupportMatrix, TransformMatrix4x4, Viewport,
};

use crate::{DirtyRect, DirtyStateStore, PresentError, Renderer};

impl Renderer {
    pub fn drain_view_ops(&mut self) {
        while let Ok(operation) = self.view_receiver.try_recv() {
            self.apply_view_op(operation);
        }
    }

    pub(super) fn apply_view_op(&mut self, operation: RenderOp) {
        let mut state_changed = false;
        match operation {
            RenderOp::SetViewTransform { matrix } => {
                if self.view_matrix != matrix {
                    self.view_matrix = matrix;
                    self.view_matrix_dirty = true;
                    state_changed = true;
                }
            }
            RenderOp::SetViewport(viewport) => {
                if self.viewport != Some(viewport) {
                    self.viewport = Some(viewport);
                    state_changed = true;
                }
            }
            RenderOp::BindRenderSteps(snapshot) => {
                if snapshot.revision >= self.drop_before_revision {
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
                    self.bound_steps = Some(snapshot);
                    self.render_tree_dirty = true;
                    self.dirty_state_store.mark_document_composite_dirty();
                    state_changed = true;
                }
            }
            RenderOp::MarkLayerDirty { layer_id } => {
                self.dirty_state_store.mark_layer_full(layer_id);
                state_changed = true;
            }
            RenderOp::SetFrameBudgetMicros { budget_micros } => {
                if self.frame_budget_micros != budget_micros {
                    self.frame_budget_micros = budget_micros;
                    state_changed = true;
                }
            }
            RenderOp::DropStaleWorkBeforeRevision { revision } => {
                if self.drop_before_revision != revision {
                    self.drop_before_revision = revision;
                    state_changed = true;
                }
                if self
                    .bound_steps
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.revision < revision)
                {
                    self.bound_steps = None;
                    self.cached_render_tree = None;
                    self.render_tree_dirty = false;
                    self.leaf_draw_cache.clear();
                    self.clear_group_target_cache();
                    self.dirty_state_store = DirtyStateStore::with_document_dirty(true);
                    state_changed = true;
                }
            }
            RenderOp::PresentToSurface => {
                self.present_requested = true;
                state_changed = true;
            }
        }
        if state_changed {
            self.frame_sync.note_state_change();
        }
    }

    #[allow(dead_code)]
    pub(crate) fn take_present_request(&mut self) -> bool {
        let requested = self.present_requested;
        self.present_requested = false;
        requested
    }

    #[allow(dead_code)]
    pub(crate) fn frame_budget_micros(&self) -> u32 {
        self.frame_budget_micros
    }

    #[allow(dead_code)]
    pub(crate) fn viewport(&self) -> Option<Viewport> {
        self.viewport
    }

    #[allow(dead_code)]
    pub(crate) fn bound_steps(&self) -> Option<&RenderStepSnapshot> {
        self.bound_steps.as_ref()
    }

    #[allow(dead_code)]
    pub(crate) fn view_matrix(&self) -> TransformMatrix4x4 {
        self.view_matrix
    }

    #[allow(dead_code)]
    pub(crate) fn mark_layer_dirty_rect(&mut self, layer_id: u64, dirty_rect: DirtyRect) {
        if self.dirty_state_store.mark_layer_rect(layer_id, dirty_rect) {
            self.frame_sync.note_state_change();
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if self.surface_config.width == width && self.surface_config.height == height {
            return;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
        self.frame_sync.note_state_change();
    }

    #[allow(dead_code)]
    pub(crate) fn present(&mut self) -> Result<(), PresentError> {
        let next_frame_id = self
            .frame_sync
            .last_committed_frame_id
            .map_or(0, |frame_id| frame_id.saturating_add(1));
        self.present_frame(next_frame_id)
    }
}
