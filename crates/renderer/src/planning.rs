//! Frame and composite planning data structures.
//!
//! This module defines frame versions/synchronization state and planning models
//! used between dirty analysis, composite planning, and frame execution.

use std::collections::{HashMap, HashSet};

use render_protocol::{BlendMode, ImageSource, TransformMatrix4x4};

use super::{DirtyTileMask, RenderTreeNode, TileCoord};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FrameVersion {
    frame_id: u64,
    snapshot_revision: u64,
    state_epoch: u64,
}

#[derive(Debug, Default)]
pub(super) struct FrameSync {
    state_epoch: u64,
    pub(super) last_committed_frame_id: Option<u64>,
}

impl FrameSync {
    pub(super) fn note_state_change(&mut self) {
        self.state_epoch = self
            .state_epoch
            .checked_add(1)
            .expect("renderer state epoch overflow");
    }

    pub(super) fn version(&self, frame_id: u64, snapshot_revision: u64) -> FrameVersion {
        FrameVersion {
            frame_id,
            snapshot_revision,
            state_epoch: self.state_epoch,
        }
    }

    pub(super) fn can_commit(&self, version: FrameVersion, snapshot_revision: u64) -> bool {
        if version.state_epoch != self.state_epoch {
            return false;
        }
        if version.snapshot_revision != snapshot_revision {
            return false;
        }
        self.last_committed_frame_id
            .is_none_or(|last_frame_id| version.frame_id > last_frame_id)
    }

    pub(super) fn commit(&mut self, version: FrameVersion, snapshot_revision: u64) {
        assert!(
            self.can_commit(version, snapshot_revision),
            "frame commit must satisfy epoch, revision, and ordering checks"
        );
        self.last_committed_frame_id = Some(version.frame_id);
    }
}

#[derive(Debug)]
pub(super) struct FramePlan {
    pub(super) version: FrameVersion,
    pub(super) render_tree: Option<RenderTreeNode>,
    pub(super) composite_plan: Option<CompositeNodePlan>,
    pub(super) composite_matrix: TransformMatrix4x4,
}

#[derive(Debug)]
pub(super) struct FrameExecutionResult {
    pub(super) version: FrameVersion,
    pub(super) render_tree: Option<RenderTreeNode>,
}

#[derive(Debug)]
pub(super) struct DirtyExecutionPlan {
    pub(super) force_group_rerender: bool,
    pub(super) dirty_leaf_tiles: HashMap<u64, DirtyTileMask>,
    pub(super) dirty_group_tiles: HashMap<u64, DirtyTileMask>,
}

#[derive(Debug)]
pub(super) enum CompositeNodePlan {
    Leaf {
        layer_id: u64,
        blend: BlendMode,
        image_source: ImageSource,
        should_rebuild: bool,
        dirty_tiles: Option<DirtyTileMask>,
        visible_tiles: Option<HashSet<TileCoord>>,
    },
    Group {
        group_id: u64,
        blend: BlendMode,
        decision: GroupRenderDecision,
        emit_tiles: Option<HashSet<TileCoord>>,
        children: Vec<CompositeNodePlan>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GroupRerenderMode {
    UseCache,
    Rerender,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GroupRenderDecision {
    pub(super) mode: GroupRerenderMode,
    pub(super) rerender_tiles: Option<HashSet<TileCoord>>,
}

#[derive(Debug, Default)]
pub(super) struct GroupDecisionEngine;

impl GroupDecisionEngine {
    pub(super) fn decide(
        &self,
        force_group_rerender: bool,
        cache_missing: bool,
        group_dirty: Option<&DirtyTileMask>,
        active_tiles: Option<&HashSet<TileCoord>>,
    ) -> GroupRenderDecision {
        let mode = if force_group_rerender || cache_missing || group_dirty.is_some() {
            GroupRerenderMode::Rerender
        } else {
            GroupRerenderMode::UseCache
        };
        let rerender_tiles = match mode {
            GroupRerenderMode::UseCache => None,
            GroupRerenderMode::Rerender => rerender_tiles_for_group(
                force_group_rerender || cache_missing,
                group_dirty,
                active_tiles,
            ),
        };

        GroupRenderDecision {
            mode,
            rerender_tiles,
        }
    }
}

pub(super) fn rerender_tiles_for_group(
    force_active_region: bool,
    group_dirty: Option<&DirtyTileMask>,
    active_tiles: Option<&HashSet<TileCoord>>,
) -> Option<HashSet<TileCoord>> {
    if force_active_region {
        return active_tiles.cloned();
    }

    match group_dirty {
        Some(DirtyTileMask::Full) => active_tiles.cloned(),
        Some(DirtyTileMask::Partial(tiles)) => {
            if let Some(active) = active_tiles {
                Some(tiles.intersection(active).copied().collect())
            } else {
                Some(tiles.clone())
            }
        }
        None => active_tiles.cloned(),
    }
}
