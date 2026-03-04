use std::sync::Arc;

use document::{FlatRenderTree, SharedRenderTree};
use glaphica_core::{AtlasLayout, BackendKind, ImageDirtyTracker, RenderTreeGeneration};
use gpu_runtime::{
    atlas_runtime::AtlasStorageRuntime, GpuContext, GpuContextInitDescriptor, RenderContext,
    RenderExecutor,
};

pub struct MainThreadState {
    gpu_context: Arc<GpuContext>,
    atlas_storage: AtlasStorageRuntime,
    render_executor: RenderExecutor,
    shared_tree: SharedRenderTree,
    dirty_tracker: ImageDirtyTracker,
}

impl MainThreadState {
    pub async fn init() -> Result<Self, InitError> {
        let gpu_context = Arc::new(
            GpuContext::init(&GpuContextInitDescriptor::default())
                .await
                .map_err(InitError::GpuContext)?,
        );

        let mut atlas_storage = AtlasStorageRuntime::with_capacity(2);
        atlas_storage
            .create_backend(
                &gpu_context.device,
                0,
                BackendKind::Leaf,
                AtlasLayout::Small11,
                Default::default(),
            )
            .map_err(InitError::Atlas)?;
        atlas_storage
            .create_backend(
                &gpu_context.device,
                1,
                BackendKind::BranchCache,
                AtlasLayout::Small11,
                Default::default(),
            )
            .map_err(InitError::Atlas)?;

        Ok(Self {
            gpu_context,
            atlas_storage,
            render_executor: RenderExecutor::new(),
            shared_tree: SharedRenderTree::new(FlatRenderTree {
                generation: RenderTreeGeneration(0),
                nodes: Arc::new(std::collections::HashMap::new()),
                root_id: None,
            }),
            dirty_tracker: ImageDirtyTracker::default(),
        })
    }

    pub fn process_render(&mut self) {
        let tree = self.shared_tree.read();
        let cmds = tree.build_render_cmds(&self.dirty_tracker);
        if cmds.is_empty() {
            return;
        }

        let mut context = RenderContext {
            gpu_context: &self.gpu_context,
            atlas_storage: &self.atlas_storage,
        };

        if let Err(e) = self.render_executor.execute(&mut context, &cmds) {
            eprintln!("render error: {e}");
        }
    }
}

#[derive(Debug)]
pub enum InitError {
    GpuContext(gpu_runtime::GpuContextInitError),
    Atlas(gpu_runtime::atlas_runtime::AtlasStorageRuntimeRegisterError),
}