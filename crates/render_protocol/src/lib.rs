use std::sync::Arc;

slotmap::new_key_type! {
    pub struct ImageHandle;
}

pub type TransformMatrix4x4 = [f32; 16];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub origin_x: u32,
    pub origin_y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderTreeSnapshot {
    pub revision: u64,
    pub root: Arc<RenderNodeSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderNodeSnapshot {
    Leaf {
        layer_id: u64,
        blend: BlendMode,
        image_handle: ImageHandle,
    },
    Group {
        group_id: u64,
        blend: BlendMode,
        children: Arc<[RenderNodeSnapshot]>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendMode {
    Normal,
    Multiply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendModePipelineStrategy {
    SurfaceAlphaBlend,
    SurfaceMultiplyBlend,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupPassStrategy {
    IsolatedOffscreenComposite,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderStepSupportMatrix {
    normal_blend_strategy: BlendModePipelineStrategy,
    multiply_blend_strategy: BlendModePipelineStrategy,
    group_strategy: GroupPassStrategy,
}

impl RenderStepSupportMatrix {
    pub const fn current_executable_semantics() -> Self {
        Self {
            normal_blend_strategy: BlendModePipelineStrategy::SurfaceAlphaBlend,
            multiply_blend_strategy: BlendModePipelineStrategy::SurfaceMultiplyBlend,
            group_strategy: GroupPassStrategy::IsolatedOffscreenComposite,
        }
    }

    pub const fn blend_strategy(&self, blend_mode: BlendMode) -> BlendModePipelineStrategy {
        match blend_mode {
            BlendMode::Normal => self.normal_blend_strategy,
            BlendMode::Multiply => self.multiply_blend_strategy,
        }
    }

    pub const fn group_strategy(&self) -> GroupPassStrategy {
        self.group_strategy
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderStepUnsupportedReason {
    BlendModeUnsupported { blend_mode: BlendMode },
    GroupCompositingUnsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderStepValidationError {
    pub step_index: usize,
    pub reason: RenderStepUnsupportedReason,
}

impl RenderTreeSnapshot {
    pub fn validate_executable(
        &self,
        support: &RenderStepSupportMatrix,
    ) -> Result<(), RenderStepValidationError> {
        let mut stack = vec![self.root.as_ref()];
        let mut node_index = 0usize;
        while let Some(node) = stack.pop() {
            match node {
                RenderNodeSnapshot::Leaf { blend, .. } => {
                    if matches!(
                        support.blend_strategy(*blend),
                        BlendModePipelineStrategy::Unsupported
                    ) {
                        return Err(RenderStepValidationError {
                            step_index: node_index,
                            reason: RenderStepUnsupportedReason::BlendModeUnsupported {
                                blend_mode: *blend,
                            },
                        });
                    }
                }
                RenderNodeSnapshot::Group {
                    blend, children, ..
                } => {
                    if matches!(
                        support.blend_strategy(*blend),
                        BlendModePipelineStrategy::Unsupported
                    ) {
                        return Err(RenderStepValidationError {
                            step_index: node_index,
                            reason: RenderStepUnsupportedReason::BlendModeUnsupported {
                                blend_mode: *blend,
                            },
                        });
                    }
                    if matches!(support.group_strategy(), GroupPassStrategy::Unsupported) {
                        return Err(RenderStepValidationError {
                            step_index: node_index,
                            reason: RenderStepUnsupportedReason::GroupCompositingUnsupported,
                        });
                    }
                    for child in children.iter().rev() {
                        stack.push(child);
                    }
                }
            }
            node_index = node_index
                .checked_add(1)
                .expect("render tree node index overflow");
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RenderOp {
    SetViewTransform { matrix: TransformMatrix4x4 },
    SetViewport(Viewport),
    BindRenderTree(RenderTreeSnapshot),
    MarkLayerDirty { layer_id: u64 },
    SetFrameBudgetMicros { budget_micros: u32 },
    DropStaleWorkBeforeRevision { revision: u64 },
    PresentToSurface,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(root: RenderNodeSnapshot) -> RenderTreeSnapshot {
        RenderTreeSnapshot {
            revision: 7,
            root: Arc::new(root),
        }
    }

    #[test]
    fn current_matrix_accepts_multiply_leaf() {
        let root = RenderNodeSnapshot::Leaf {
            layer_id: 11,
            blend: BlendMode::Multiply,
            image_handle: ImageHandle::default(),
        };
        let snapshot = snapshot(root);

        snapshot
            .validate_executable(&RenderStepSupportMatrix::current_executable_semantics())
            .expect("multiply should be supported by current executable semantics");
    }

    #[test]
    fn current_matrix_accepts_group_boundaries() {
        let root = RenderNodeSnapshot::Group {
            group_id: 0,
            blend: BlendMode::Normal,
            children: Arc::from(
                vec![RenderNodeSnapshot::Leaf {
                    layer_id: 5,
                    blend: BlendMode::Normal,
                    image_handle: ImageHandle::default(),
                }]
                .into_boxed_slice(),
            ),
        };
        let snapshot = snapshot(root);

        snapshot
            .validate_executable(&RenderStepSupportMatrix::current_executable_semantics())
            .expect("group boundary should be valid as isolated compositing in current semantics");
    }

    #[test]
    fn group_blend_reports_unsupported_mode() {
        let root = RenderNodeSnapshot::Group {
            group_id: 7,
            blend: BlendMode::Multiply,
            children: Arc::from(Vec::<RenderNodeSnapshot>::new().into_boxed_slice()),
        };
        let snapshot = snapshot(root);

        let support = RenderStepSupportMatrix {
            normal_blend_strategy: BlendModePipelineStrategy::SurfaceAlphaBlend,
            multiply_blend_strategy: BlendModePipelineStrategy::Unsupported,
            group_strategy: GroupPassStrategy::IsolatedOffscreenComposite,
        };

        let error = snapshot
            .validate_executable(&support)
            .expect_err("group multiply blend should be rejected when unsupported");
        assert_eq!(error.step_index, 0);
        assert_eq!(
            error.reason,
            RenderStepUnsupportedReason::BlendModeUnsupported {
                blend_mode: BlendMode::Multiply,
            }
        );
    }
}
