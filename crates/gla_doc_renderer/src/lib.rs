use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasError, Backend as AtlasBackend};
use gla_document::{GlaDoc, GlaDocError, GlaNodeId, GlaNodeKind};
use gla_image::{
    GlaCachedImage, GlaCachedImageCreateError, GlaImage, GlaImageCreateError,
    GlaImageTileAccessError,
};
use glaphica_core::BlendMode;
use renderer::{
    CompositeTileCommand, RenderCommand, TileCompositeSource, TileRenderer, TileRendererError,
};

#[derive(Debug)]
pub struct GlaDocRenderer {
    render_backend: AtlasBackend,
    node_resources: Vec<NodeRenderResource>,
    active_plan: Option<ActiveRenderPlan>,
    brush_preview_image: Option<GlaImage>,
}

#[derive(Debug)]
pub struct NodeRenderResource {
    node_id: GlaNodeId,
    state: RenderImageState,
}

#[derive(Debug)]
pub enum RenderImageState {
    Active(GlaImage),
    Cached(GlaCachedImage),
    Empty,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActiveRenderPlan {
    active_layer_id: GlaNodeId,
    ancestor_chain: Vec<GlaNodeId>,
    resource_group_nodes: Vec<GlaNodeId>,
    prepare_steps: Vec<PrepareRenderStep>,
    passes: Vec<ActiveRenderPass>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActiveRenderPass {
    node_id: GlaNodeId,
    active_child_index: usize,
    inputs: Vec<RenderProgramInput>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PrepareRenderStep {
    node_id: GlaNodeId,
    inputs: Vec<RenderProgramInput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderProgramSourceKind {
    Truth,
    Result,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderProgramInput {
    node_id: GlaNodeId,
    source_kind: RenderProgramSourceKind,
    opacity: f32,
    blend_mode: BlendMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareExecutionError {
    message: String,
}

pub trait PrepareExecutor {
    fn compose_node(
        &mut self,
        target_node_id: GlaNodeId,
        inputs: &[RenderProgramInput],
    ) -> Result<(), PrepareExecutionError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderExecutionError {
    message: String,
}

pub trait RenderExecutor {
    fn composite_node_tiles(
        &mut self,
        target_node_id: GlaNodeId,
        tile_indices: &[usize],
        inputs: &[RenderProgramInput],
    ) -> Result<(), RenderExecutionError>;

    fn present_root_tiles(
        &mut self,
        root_node_id: GlaNodeId,
        tile_indices: &[usize],
    ) -> Result<(), RenderExecutionError>;
}

#[derive(Debug)]
pub enum GlaDocRendererError {
    Document(GlaDocError),
    Atlas(AtlasError),
    CachedImageCreate(GlaCachedImageCreateError),
    ImageCreate(GlaImageCreateError),
    ImageTileAccess(GlaImageTileAccessError),
    TileRenderer(TileRendererError),
    PrepareExecution(PrepareExecutionError),
    RenderExecution(RenderExecutionError),
    MissingActivePlan,
}

impl Display for GlaDocRendererError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Document(error) => Display::fmt(error, f),
            Self::Atlas(error) => Display::fmt(error, f),
            Self::CachedImageCreate(error) => Display::fmt(error, f),
            Self::ImageCreate(error) => Display::fmt(error, f),
            Self::ImageTileAccess(error) => Display::fmt(error, f),
            Self::TileRenderer(error) => Display::fmt(error, f),
            Self::PrepareExecution(error) => Display::fmt(error, f),
            Self::RenderExecution(error) => Display::fmt(error, f),
            Self::MissingActivePlan => f.write_str("missing active render plan"),
        }
    }
}

impl Error for GlaDocRendererError {}

impl From<GlaDocError> for GlaDocRendererError {
    fn from(error: GlaDocError) -> Self {
        Self::Document(error)
    }
}

impl From<AtlasError> for GlaDocRendererError {
    fn from(error: AtlasError) -> Self {
        Self::Atlas(error)
    }
}

impl From<GlaCachedImageCreateError> for GlaDocRendererError {
    fn from(error: GlaCachedImageCreateError) -> Self {
        Self::CachedImageCreate(error)
    }
}

impl From<GlaImageCreateError> for GlaDocRendererError {
    fn from(error: GlaImageCreateError) -> Self {
        Self::ImageCreate(error)
    }
}

impl From<GlaImageTileAccessError> for GlaDocRendererError {
    fn from(error: GlaImageTileAccessError) -> Self {
        Self::ImageTileAccess(error)
    }
}

impl From<TileRendererError> for GlaDocRendererError {
    fn from(error: TileRendererError) -> Self {
        Self::TileRenderer(error)
    }
}

impl From<PrepareExecutionError> for GlaDocRendererError {
    fn from(error: PrepareExecutionError) -> Self {
        Self::PrepareExecution(error)
    }
}

impl From<RenderExecutionError> for GlaDocRendererError {
    fn from(error: RenderExecutionError) -> Self {
        Self::RenderExecution(error)
    }
}

impl Display for PrepareExecutionError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for PrepareExecutionError {}

impl PrepareExecutionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for RenderExecutionError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for RenderExecutionError {}

impl RenderExecutionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl GlaDocRenderer {
    pub fn new(render_backend: AtlasBackend) -> Self {
        Self {
            render_backend,
            node_resources: Vec::new(),
            active_plan: None,
            brush_preview_image: None,
        }
    }

    pub fn render_backend(&self) -> &AtlasBackend {
        &self.render_backend
    }

    pub fn node_resources(&self) -> &[NodeRenderResource] {
        &self.node_resources
    }

    pub fn active_plan(&self) -> Option<&ActiveRenderPlan> {
        self.active_plan.as_ref()
    }

    pub fn root_active_image(&self) -> Option<&GlaImage> {
        let root_node_id = self.active_plan.as_ref()?.ancestor_chain.last().copied()?;
        self.node_resources
            .iter()
            .find(|entry| entry.node_id == root_node_id)
            .and_then(|entry| match entry.state() {
                RenderImageState::Active(image) => Some(image),
                RenderImageState::Cached(_) | RenderImageState::Empty => None,
            })
    }

    pub fn brush_preview_image(&self) -> Option<&GlaImage> {
        self.brush_preview_image.as_ref()
    }

    pub fn clear_brush_preview_image(&mut self) {
        self.brush_preview_image = None;
    }

    pub fn ensure_brush_preview_tile(
        &mut self,
        doc: &GlaDoc,
        tile_index: usize,
    ) -> Result<atlas::TileKey, GlaDocRendererError> {
        if self.brush_preview_image.is_none() {
            self.brush_preview_image = Some(GlaImage::new(doc.layout(), doc.render_backend())?);
        }
        let preview_image = self
            .brush_preview_image
            .as_mut()
            .ok_or(GlaDocRendererError::MissingActivePlan)?;
        ensure_active_target_tile(preview_image, tile_index, &self.render_backend)
    }

    pub fn ensure_brush_preview_merge_target(
        &mut self,
        doc: &GlaDoc,
        tile_index: usize,
    ) -> Result<(atlas::TileKey, atlas::TileKey), GlaDocRendererError> {
        let origin_tile_key = doc
            .active_layer_image()?
            .tile_key(tile_index)
            .ok_or(GlaImageTileAccessError::OutOfBounds)?;
        let preview_tile_key = self.ensure_brush_preview_tile(doc, tile_index)?;
        Ok((origin_tile_key, preview_tile_key))
    }

    pub fn render_active_tiles(
        &self,
        tile_indices: &[usize],
        executor: &mut impl RenderExecutor,
    ) -> Result<(), GlaDocRendererError> {
        let plan = self
            .active_plan
            .as_ref()
            .ok_or(GlaDocRendererError::MissingActivePlan)?;

        for pass in &plan.passes {
            executor.composite_node_tiles(pass.node_id, tile_indices, &pass.inputs)?;
        }

        let root_node_id = *plan
            .ancestor_chain
            .last()
            .ok_or(GlaDocRendererError::MissingActivePlan)?;
        executor.present_root_tiles(root_node_id, tile_indices)?;
        Ok(())
    }

    pub fn sync_document(&mut self, doc: &GlaDoc) -> Result<(), GlaDocRendererError> {
        let mut live_render_nodes = Vec::new();
        self.collect_render_nodes(doc, &mut live_render_nodes)?;

        self.node_resources
            .retain(|entry| live_render_nodes.contains(&entry.node_id));

        for node_id in live_render_nodes {
            if self
                .node_resources
                .iter()
                .any(|entry| entry.node_id == node_id)
            {
                continue;
            }

            self.node_resources.push(NodeRenderResource {
                node_id,
                state: RenderImageState::Empty,
            });
        }

        Ok(())
    }

    pub fn prepare_active_plan(
        &mut self,
        doc: &GlaDoc,
        executor: &mut impl PrepareExecutor,
    ) -> Result<&ActiveRenderPlan, GlaDocRendererError> {
        self.sync_document(doc)?;

        let ancestor_chain = doc.active_layer_ancestor_chain().to_vec();
        let mut resource_group_nodes = Vec::new();
        let mut passes = Vec::new();
        for (ancestor_index, &node_id) in ancestor_chain.iter().enumerate().skip(1) {
            let child_ids = doc.child_ids(node_id)?.to_vec();
            let active_child_id = ancestor_chain[ancestor_index - 1];
            let active_child_index = child_ids
                .iter()
                .position(|candidate| *candidate == active_child_id)
                .ok_or(GlaDocError::InvalidNodeId(doc.active_layer_id()))?;
            push_unique_node(&mut resource_group_nodes, node_id);
            for &child_id in &child_ids {
                if matches!(
                    doc.node(child_id)?.kind(),
                    GlaNodeKind::Root | GlaNodeKind::Branch
                ) {
                    push_unique_node(&mut resource_group_nodes, child_id);
                }
            }
            passes.push(ActiveRenderPass {
                node_id,
                active_child_index,
                inputs: build_render_program_inputs(doc, &child_ids)?,
            });
        }

        self.demote_inactive_nodes(&resource_group_nodes)?;
        let newly_allocated_nodes = self.promote_active_nodes(doc, &resource_group_nodes)?;
        let prepare_steps =
            self.build_prepare_steps(doc, &resource_group_nodes, &newly_allocated_nodes)?;
        execute_prepare_steps(&prepare_steps, executor)?;

        self.active_plan = Some(ActiveRenderPlan {
            active_layer_id: doc.active_layer_id(),
            ancestor_chain,
            resource_group_nodes,
            prepare_steps,
            passes,
        });

        Ok(self.active_plan.as_ref().expect("active plan should exist"))
    }

    pub fn prepare_active_plan_gpu(
        &mut self,
        doc: &GlaDoc,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tile_renderer: &mut TileRenderer,
    ) -> Result<&ActiveRenderPlan, GlaDocRendererError> {
        struct NoopPrepareExecutor;

        impl PrepareExecutor for NoopPrepareExecutor {
            fn compose_node(
                &mut self,
                _target_node_id: GlaNodeId,
                _inputs: &[RenderProgramInput],
            ) -> Result<(), PrepareExecutionError> {
                Ok(())
            }
        }

        tile_renderer.ensure_backend(device, &self.render_backend)?;
        let mut executor = NoopPrepareExecutor;
        self.prepare_active_plan(doc, &mut executor)?;

        let prepare_steps = self
            .active_plan
            .as_ref()
            .map(|plan| plan.prepare_steps.clone())
            .ok_or(GlaDocRendererError::MissingActivePlan)?;
        let tile_count = usize::try_from(doc.layout().total_tiles())
            .map_err(|_| GlaImageCreateError::TooManyTiles)?;
        let tile_indices = (0..tile_count).collect::<Vec<_>>();
        let active_layer_id = self
            .active_plan
            .as_ref()
            .map(|plan| plan.active_layer_id())
            .ok_or(GlaDocRendererError::MissingActivePlan)?;
        let mut commands = Vec::new();

        for step in &prepare_steps {
            self.compose_node_commands(
                doc,
                step.node_id,
                &tile_indices,
                &step.inputs,
                active_layer_id,
                &mut commands,
            )?;
        }
        let clear_batches = self.render_backend.take_pending_clear_batches()?;
        tile_renderer.execute_commands(
            device,
            queue,
            &[&self.render_backend],
            &clear_batches,
            &commands,
            None,
        )?;

        self.active_plan
            .as_ref()
            .ok_or(GlaDocRendererError::MissingActivePlan)
    }

    pub fn render_active_tiles_gpu(
        &mut self,
        doc: &GlaDoc,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tile_renderer: &mut TileRenderer,
        tile_indices: &[usize],
    ) -> Result<(), GlaDocRendererError> {
        tile_renderer.ensure_backend(device, &self.render_backend)?;
        let passes = self
            .active_plan
            .as_ref()
            .map(|plan| plan.passes.clone())
            .ok_or(GlaDocRendererError::MissingActivePlan)?;
        let active_layer_id = self
            .active_plan
            .as_ref()
            .map(|plan| plan.active_layer_id())
            .ok_or(GlaDocRendererError::MissingActivePlan)?;
        let mut commands = Vec::new();

        for pass in &passes {
            self.compose_node_commands(
                doc,
                pass.node_id,
                tile_indices,
                &pass.inputs,
                active_layer_id,
                &mut commands,
            )?;
        }
        let clear_batches = self.render_backend.take_pending_clear_batches()?;
        tile_renderer.execute_commands(
            device,
            queue,
            &[&self.render_backend],
            &clear_batches,
            &commands,
            None,
        )?;
        Ok(())
    }

    fn collect_render_nodes(
        &self,
        doc: &GlaDoc,
        output: &mut Vec<GlaNodeId>,
    ) -> Result<(), GlaDocRendererError> {
        output.clear();
        let mut preorder = Vec::new();
        doc.collect_subtree_preorder(doc.root_id(), &mut preorder)?;
        for node_id in preorder {
            let kind = doc.node(node_id)?.kind();
            if matches!(kind, GlaNodeKind::Root | GlaNodeKind::Branch) {
                output.push(node_id);
            }
        }
        Ok(())
    }

    fn demote_inactive_nodes(
        &mut self,
        resource_group_nodes: &[GlaNodeId],
    ) -> Result<(), GlaDocRendererError> {
        for entry in &mut self.node_resources {
            if resource_group_nodes.contains(&entry.node_id) {
                continue;
            }

            let active_image = match std::mem::replace(&mut entry.state, RenderImageState::Empty) {
                RenderImageState::Active(image) => image,
                state => {
                    entry.state = state;
                    continue;
                }
            };

            let mut active_keys = Vec::new();
            for tile_index in 0..active_image.tile_count() {
                let tile_key = active_image
                    .tile_key(tile_index)
                    .unwrap_or(atlas::TileKey::EMPTY);
                if tile_key != atlas::TileKey::EMPTY {
                    active_keys.push(tile_key);
                }
            }

            let cached_group = self.render_backend.cache_active_tiles(&active_keys)?;
            entry.state = RenderImageState::Cached(GlaCachedImage::from_active_image(
                &active_image,
                cached_group,
            )?);
        }

        Ok(())
    }

    fn promote_active_nodes(
        &mut self,
        doc: &GlaDoc,
        resource_group_nodes: &[GlaNodeId],
    ) -> Result<Vec<GlaNodeId>, GlaDocRendererError> {
        let mut newly_allocated_nodes = Vec::new();
        for &node_id in resource_group_nodes {
            let entry = self
                .node_resources
                .iter_mut()
                .find(|entry| entry.node_id == node_id)
                .ok_or(GlaDocError::InvalidNodeId(node_id))?;

            let state = std::mem::replace(&mut entry.state, RenderImageState::Empty);
            entry.state =
                match state {
                    RenderImageState::Active(image) => RenderImageState::Active(image),
                    RenderImageState::Cached(cached) => RenderImageState::Active(
                        restore_cached_image(doc, &self.render_backend, &cached)?,
                    ),
                    RenderImageState::Empty => {
                        newly_allocated_nodes.push(node_id);
                        RenderImageState::Active(GlaImage::new(doc.layout(), doc.render_backend())?)
                    }
                };
        }

        Ok(newly_allocated_nodes)
    }

    fn build_prepare_steps(
        &self,
        doc: &GlaDoc,
        resource_group_nodes: &[GlaNodeId],
        newly_allocated_nodes: &[GlaNodeId],
    ) -> Result<Vec<PrepareRenderStep>, GlaDocRendererError> {
        let mut prepare_steps = Vec::new();
        let mut visited = Vec::new();
        for &node_id in resource_group_nodes {
            self.collect_prepare_steps_for_node(
                doc,
                node_id,
                resource_group_nodes,
                newly_allocated_nodes,
                &mut visited,
                &mut prepare_steps,
            )?;
        }
        Ok(prepare_steps)
    }

    fn collect_prepare_steps_for_node(
        &self,
        doc: &GlaDoc,
        node_id: GlaNodeId,
        resource_group_nodes: &[GlaNodeId],
        newly_allocated_nodes: &[GlaNodeId],
        visited: &mut Vec<GlaNodeId>,
        prepare_steps: &mut Vec<PrepareRenderStep>,
    ) -> Result<(), GlaDocRendererError> {
        if !newly_allocated_nodes.contains(&node_id) || visited.contains(&node_id) {
            return Ok(());
        }

        let child_ids = doc.child_ids(node_id)?.to_vec();
        for &child_id in &child_ids {
            if !resource_group_nodes.contains(&child_id) {
                continue;
            }
            self.collect_prepare_steps_for_node(
                doc,
                child_id,
                resource_group_nodes,
                newly_allocated_nodes,
                visited,
                prepare_steps,
            )?;
        }

        visited.push(node_id);
        prepare_steps.push(PrepareRenderStep {
            node_id,
            inputs: build_render_program_inputs(doc, &child_ids)?,
        });
        Ok(())
    }

    fn compose_node_commands(
        &mut self,
        doc: &GlaDoc,
        target_node_id: GlaNodeId,
        tile_indices: &[usize],
        inputs: &[RenderProgramInput],
        active_layer_id: GlaNodeId,
        output: &mut Vec<RenderCommand>,
    ) -> Result<(), GlaDocRendererError> {
        let target_index = self
            .node_resources
            .iter()
            .position(|entry| entry.node_id == target_node_id)
            .ok_or(GlaDocError::InvalidNodeId(target_node_id))?;

        for &tile_index in tile_indices {
            let sources = self.collect_tile_sources(doc, inputs, tile_index, active_layer_id)?;
            let target_state = &mut self.node_resources[target_index].state;
            let target_image = match target_state {
                RenderImageState::Active(image) => image,
                RenderImageState::Cached(_) | RenderImageState::Empty => {
                    return Err(GlaDocError::InvalidNodeId(target_node_id).into());
                }
            };

            if sources.is_empty() {
                if target_image
                    .tile_key(tile_index)
                    .unwrap_or(atlas::TileKey::EMPTY)
                    != atlas::TileKey::EMPTY
                {
                    target_image.clear_tile(tile_index)?;
                }
                continue;
            }

            let target_key =
                ensure_active_target_tile(target_image, tile_index, &self.render_backend)?;
            output.push(RenderCommand::CompositeTile(CompositeTileCommand {
                target_tile_key: target_key,
                inputs: sources,
            }));
        }

        Ok(())
    }

    fn collect_tile_sources(
        &self,
        doc: &GlaDoc,
        inputs: &[RenderProgramInput],
        tile_index: usize,
        active_layer_id: GlaNodeId,
    ) -> Result<Vec<TileCompositeSource>, GlaDocRendererError> {
        let mut sources = Vec::with_capacity(inputs.len());
        for input in inputs {
            let tile_key = match input.source_kind {
                RenderProgramSourceKind::Truth if input.node_id == active_layer_id => self
                    .brush_preview_image
                    .as_ref()
                    .and_then(|preview_node| preview_node.tile_key(tile_index))
                    .unwrap_or(atlas::TileKey::EMPTY),
                RenderProgramSourceKind::Truth => doc
                    .node_image(input.node_id)?
                    .tile_key(tile_index)
                    .unwrap_or(atlas::TileKey::EMPTY),
                RenderProgramSourceKind::Result => self
                    .node_resources
                    .iter()
                    .find(|entry| entry.node_id == input.node_id)
                    .and_then(|entry| match &entry.state {
                        RenderImageState::Active(image) => image.tile_key(tile_index),
                        RenderImageState::Cached(cached) => cached.tile_key(tile_index),
                        RenderImageState::Empty => Some(atlas::TileKey::EMPTY),
                    })
                    .unwrap_or(atlas::TileKey::EMPTY),
            };
            if tile_key == atlas::TileKey::EMPTY {
                continue;
            }
            sources.push(TileCompositeSource {
                tile_key,
                opacity: input.opacity,
                blend_mode: input.blend_mode,
            });
        }
        Ok(sources)
    }
}

fn restore_cached_image(
    doc: &GlaDoc,
    render_backend: &AtlasBackend,
    cached: &GlaCachedImage,
) -> Result<GlaImage, GlaDocRendererError> {
    let mut image = GlaImage::new(doc.layout(), doc.render_backend())?;
    let activated = render_backend.activate_cached_group(cached.cache_group())?;
    let mut activated_iter = activated.into_iter();

    for tile_index in 0..cached.tile_count() {
        if cached.tile_key(tile_index).unwrap_or(atlas::TileKey::EMPTY) == atlas::TileKey::EMPTY {
            continue;
        }

        let tile_owner = activated_iter.next().ok_or(AtlasError::InvalidState)?;
        image.replace_tile_owner(tile_index, tile_owner)?;
    }

    if activated_iter.next().is_some() {
        return Err(AtlasError::InvalidState.into());
    }

    Ok(image)
}

fn ensure_active_target_tile(
    image: &mut GlaImage,
    tile_index: usize,
    render_backend: &AtlasBackend,
) -> Result<atlas::TileKey, GlaDocRendererError> {
    let tile_key = image
        .tile_key(tile_index)
        .ok_or(GlaImageTileAccessError::OutOfBounds)?;
    if tile_key != atlas::TileKey::EMPTY {
        return Ok(tile_key);
    }

    let tile_owner = render_backend.alloc_active()?;
    let previous = image.replace_tile_owner(tile_index, tile_owner)?;
    if previous.tile_key() != atlas::TileKey::EMPTY {
        return Err(AtlasError::InvalidState.into());
    }

    image
        .tile_key(tile_index)
        .ok_or(GlaImageTileAccessError::OutOfBounds.into())
}

impl NodeRenderResource {
    pub fn node_id(&self) -> GlaNodeId {
        self.node_id
    }

    pub fn state(&self) -> &RenderImageState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut RenderImageState {
        &mut self.state
    }
}

impl ActiveRenderPlan {
    pub fn active_layer_id(&self) -> GlaNodeId {
        self.active_layer_id
    }

    pub fn ancestor_chain(&self) -> &[GlaNodeId] {
        &self.ancestor_chain
    }

    pub fn resource_group_nodes(&self) -> &[GlaNodeId] {
        &self.resource_group_nodes
    }

    pub fn passes(&self) -> &[ActiveRenderPass] {
        &self.passes
    }

    pub fn prepare_steps(&self) -> &[PrepareRenderStep] {
        &self.prepare_steps
    }
}

impl ActiveRenderPass {
    pub fn node_id(&self) -> GlaNodeId {
        self.node_id
    }

    pub fn active_child_index(&self) -> usize {
        self.active_child_index
    }

    pub fn inputs(&self) -> &[RenderProgramInput] {
        &self.inputs
    }
}

impl PrepareRenderStep {
    pub fn node_id(&self) -> GlaNodeId {
        self.node_id
    }

    pub fn inputs(&self) -> &[RenderProgramInput] {
        &self.inputs
    }
}

fn push_unique_node(output: &mut Vec<GlaNodeId>, node_id: GlaNodeId) {
    if !output.contains(&node_id) {
        output.push(node_id);
    }
}

impl RenderProgramInput {
    pub fn node_id(&self) -> GlaNodeId {
        self.node_id
    }

    pub fn source_kind(&self) -> RenderProgramSourceKind {
        self.source_kind
    }

    pub fn opacity(&self) -> f32 {
        self.opacity
    }

    pub fn blend_mode(&self) -> BlendMode {
        self.blend_mode
    }
}

fn build_render_program_inputs(
    doc: &GlaDoc,
    child_ids: &[GlaNodeId],
) -> Result<Vec<RenderProgramInput>, GlaDocRendererError> {
    let mut inputs = Vec::with_capacity(child_ids.len());
    for &child_id in child_ids {
        let source_kind = match doc.node(child_id)?.kind() {
            GlaNodeKind::Leaf => RenderProgramSourceKind::Truth,
            GlaNodeKind::Root | GlaNodeKind::Branch => RenderProgramSourceKind::Result,
        };
        inputs.push(RenderProgramInput {
            node_id: child_id,
            source_kind,
            opacity: doc.node(child_id)?.opacity(),
            blend_mode: doc.node(child_id)?.blend_mode(),
        });
    }
    Ok(inputs)
}

fn execute_prepare_steps(
    prepare_steps: &[PrepareRenderStep],
    executor: &mut impl PrepareExecutor,
) -> Result<(), GlaDocRendererError> {
    for step in prepare_steps {
        executor.compose_node(step.node_id, &step.inputs)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use atlas::{AtlasLayout, Backend as AtlasBackend, TileKey};
    use gla_document::{BackendId, GlaDoc, GlaImageLayout};

    use crate::{
        GlaDocRenderer, PrepareExecutionError, PrepareExecutor, RenderExecutionError,
        RenderExecutor, RenderImageState, RenderProgramSourceKind,
    };

    #[derive(Default)]
    struct NoopPrepareExecutor;

    impl PrepareExecutor for NoopPrepareExecutor {
        fn compose_node(
            &mut self,
            _target_node_id: gla_document::GlaNodeId,
            _inputs: &[crate::RenderProgramInput],
        ) -> Result<(), PrepareExecutionError> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingPrepareExecutor {
        calls: Vec<gla_document::GlaNodeId>,
    }

    impl PrepareExecutor for RecordingPrepareExecutor {
        fn compose_node(
            &mut self,
            target_node_id: gla_document::GlaNodeId,
            _inputs: &[crate::RenderProgramInput],
        ) -> Result<(), PrepareExecutionError> {
            self.calls.push(target_node_id);
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingRenderExecutor {
        composites: Vec<(
            gla_document::GlaNodeId,
            Vec<usize>,
            Vec<crate::RenderProgramInput>,
        )>,
        presents: Vec<(gla_document::GlaNodeId, Vec<usize>)>,
    }

    impl RenderExecutor for RecordingRenderExecutor {
        fn composite_node_tiles(
            &mut self,
            target_node_id: gla_document::GlaNodeId,
            tile_indices: &[usize],
            inputs: &[crate::RenderProgramInput],
        ) -> Result<(), RenderExecutionError> {
            self.composites
                .push((target_node_id, tile_indices.to_vec(), inputs.to_vec()));
            Ok(())
        }

        fn present_root_tiles(
            &mut self,
            root_node_id: gla_document::GlaNodeId,
            tile_indices: &[usize],
        ) -> Result<(), RenderExecutionError> {
            self.presents.push((root_node_id, tile_indices.to_vec()));
            Ok(())
        }
    }

    fn new_doc() -> GlaDoc {
        GlaDoc::new(
            GlaImageLayout::new(64, 64),
            BackendId::new(3),
            BackendId::new(7),
            AtlasBackend::new(AtlasLayout::Tiny8, BackendId::new(11)),
        )
        .expect("document should build")
    }

    fn new_render_backend() -> AtlasBackend {
        AtlasBackend::new(AtlasLayout::Tiny8, BackendId::new(7))
    }

    #[test]
    fn ensure_brush_preview_merge_target_uses_active_layer_truth_and_render_cache() {
        let image_backend = AtlasBackend::new(AtlasLayout::Tiny8, BackendId::new(3));
        let mut doc = new_doc();
        let layer_id = doc
            .append_layer(doc.root_id())
            .expect("layer should append");
        doc.set_active_layer(layer_id)
            .expect("active layer should update");

        let active_tile = image_backend
            .alloc_active()
            .expect("active tile should allocate");
        let active_tile_key = active_tile.tile_key();
        doc.active_layer_image_mut()
            .expect("layer image should exist")
            .replace_tile_owner(0, active_tile)
            .expect("tile owner should install");

        let mut renderer = GlaDocRenderer::new(new_render_backend());
        let (origin_tile_key, preview_tile_key) = renderer
            .ensure_brush_preview_merge_target(&doc, 0)
            .expect("preview target should build");

        assert_eq!(origin_tile_key, active_tile_key);
        assert_ne!(preview_tile_key, TileKey::EMPTY);
        assert_eq!(
            renderer
                .brush_preview_image()
                .and_then(|image| image.tile_key(0)),
            Some(preview_tile_key)
        );
    }

    #[test]
    fn sync_document_tracks_only_root_and_branch_nodes() {
        let mut doc = new_doc();
        let group_id = doc
            .append_group(doc.root_id())
            .expect("group should append");
        let layer_id = doc.append_layer(group_id).expect("layer should append");
        let mut renderer = GlaDocRenderer::new(new_render_backend());

        renderer.sync_document(&doc).expect("sync should succeed");

        assert_eq!(renderer.node_resources().len(), 2);
        assert!(
            renderer
                .node_resources()
                .iter()
                .all(|entry| entry.node_id() != layer_id)
        );
        let branch_state = renderer
            .node_resources()
            .iter()
            .find(|entry| entry.node_id() == group_id)
            .expect("branch entry should exist");
        assert!(matches!(branch_state.state(), RenderImageState::Empty));
    }

    #[test]
    fn prepare_active_plan_promotes_ancestor_chain_to_active_images() {
        let mut doc = new_doc();
        let group_id = doc
            .append_group(doc.root_id())
            .expect("group should append");
        let layer_id = doc.append_layer(group_id).expect("layer should append");
        doc.set_active_layer(layer_id)
            .expect("active layer should update");
        let mut renderer = GlaDocRenderer::new(new_render_backend());
        let mut executor = NoopPrepareExecutor;

        let plan = renderer
            .prepare_active_plan(&doc, &mut executor)
            .expect("active plan should build");

        assert_eq!(plan.resource_group_nodes(), &[group_id, doc.root_id()]);
        assert!(
            renderer
                .node_resources()
                .iter()
                .all(|entry| { matches!(entry.state(), RenderImageState::Active(_)) })
        );
    }

    #[test]
    fn prepare_active_plan_demotes_inactive_nodes_to_cached_images() {
        let mut doc = new_doc();
        let left_group = doc
            .append_group(doc.root_id())
            .expect("left group should append");
        let left_nested = doc
            .append_group(left_group)
            .expect("left nested should append");
        let right_group = doc
            .append_group(doc.root_id())
            .expect("right group should append");
        let left_layer = doc
            .append_layer(left_nested)
            .expect("left layer should append");
        let right_layer = doc
            .append_layer(right_group)
            .expect("right layer should append");
        doc.set_active_layer(left_layer)
            .expect("active layer should update");
        let mut renderer = GlaDocRenderer::new(new_render_backend());
        let mut executor = NoopPrepareExecutor;

        renderer
            .prepare_active_plan(&doc, &mut executor)
            .expect("first active plan should build");
        doc.set_active_layer(right_layer)
            .expect("active layer should update");
        renderer
            .prepare_active_plan(&doc, &mut executor)
            .expect("second active plan should build");

        let left_state = renderer
            .node_resources()
            .iter()
            .find(|entry| entry.node_id() == left_nested)
            .expect("left nested should exist");
        let right_state = renderer
            .node_resources()
            .iter()
            .find(|entry| entry.node_id() == right_group)
            .expect("right group should exist");

        assert!(matches!(left_state.state(), RenderImageState::Cached(_)));
        assert!(matches!(right_state.state(), RenderImageState::Active(_)));
    }

    #[test]
    fn prepare_active_plan_includes_ancestor_children_in_resource_group() {
        let mut doc = new_doc();
        let left_group = doc
            .append_group(doc.root_id())
            .expect("left group should append");
        let right_group = doc
            .append_group(doc.root_id())
            .expect("right group should append");
        let nested_group = doc
            .append_group(left_group)
            .expect("nested group should append");
        let active_layer = doc
            .append_layer(nested_group)
            .expect("active layer should append");
        doc.append_layer(left_group)
            .expect("left sibling leaf should append");
        doc.set_active_layer(active_layer)
            .expect("active layer should update");
        let mut renderer = GlaDocRenderer::new(new_render_backend());
        let mut executor = NoopPrepareExecutor;

        let plan = renderer
            .prepare_active_plan(&doc, &mut executor)
            .expect("active plan should build");

        assert_eq!(
            plan.resource_group_nodes(),
            &[nested_group, left_group, doc.root_id(), right_group]
        );
        let right_state = renderer
            .node_resources()
            .iter()
            .find(|entry| entry.node_id() == right_group)
            .expect("right group should exist");
        assert!(matches!(right_state.state(), RenderImageState::Active(_)));
    }

    #[test]
    fn prepare_active_plan_records_bottom_up_render_pass_inputs() {
        let mut doc = new_doc();
        let group = doc
            .append_group(doc.root_id())
            .expect("group should append");
        let first = doc.append_layer(group).expect("first should append");
        let second = doc.append_layer(group).expect("second should append");
        doc.set_active_layer(second)
            .expect("active layer should update");
        let mut renderer = GlaDocRenderer::new(new_render_backend());
        let mut executor = NoopPrepareExecutor;

        let plan = renderer
            .prepare_active_plan(&doc, &mut executor)
            .expect("active plan should build");

        assert_eq!(plan.passes().len(), 2);
        assert_eq!(plan.passes()[0].node_id(), group);
        assert_eq!(plan.passes()[0].active_child_index(), 1);
        assert_eq!(plan.passes()[0].inputs()[0].node_id(), first);
        assert_eq!(
            plan.passes()[0].inputs()[0].source_kind(),
            RenderProgramSourceKind::Truth
        );
        assert_eq!(plan.passes()[0].inputs()[1].node_id(), second);
        assert_eq!(plan.passes()[1].node_id(), doc.root_id());
        assert_eq!(plan.passes()[1].active_child_index(), 0);
    }

    #[test]
    fn prepare_active_plan_records_recursive_prepare_steps_for_empty_results() {
        let mut doc = new_doc();
        let group = doc
            .append_group(doc.root_id())
            .expect("group should append");
        let nested = doc.append_group(group).expect("nested should append");
        let layer = doc.append_layer(nested).expect("layer should append");
        doc.set_active_layer(layer)
            .expect("active layer should update");
        let mut renderer = GlaDocRenderer::new(new_render_backend());
        let mut executor = NoopPrepareExecutor;

        let plan = renderer
            .prepare_active_plan(&doc, &mut executor)
            .expect("active plan should build");

        assert_eq!(plan.prepare_steps().len(), 3);
        assert_eq!(plan.prepare_steps()[0].node_id(), nested);
        assert_eq!(plan.prepare_steps()[0].inputs()[0].node_id(), layer);
        assert_eq!(
            plan.prepare_steps()[0].inputs()[0].source_kind(),
            RenderProgramSourceKind::Truth
        );
        assert_eq!(plan.prepare_steps()[1].node_id(), group);
        assert_eq!(plan.prepare_steps()[1].inputs()[0].node_id(), nested);
        assert_eq!(
            plan.prepare_steps()[1].inputs()[0].source_kind(),
            RenderProgramSourceKind::Result
        );
        assert_eq!(plan.prepare_steps()[2].node_id(), doc.root_id());
        assert_eq!(plan.prepare_steps()[2].inputs()[0].node_id(), group);
    }

    #[test]
    fn prepare_active_plan_executes_prepare_steps_in_linear_order() {
        let mut doc = new_doc();
        let group = doc
            .append_group(doc.root_id())
            .expect("group should append");
        let nested = doc.append_group(group).expect("nested should append");
        let layer = doc.append_layer(nested).expect("layer should append");
        doc.set_active_layer(layer)
            .expect("active layer should update");
        let mut renderer = GlaDocRenderer::new(new_render_backend());
        let mut executor = RecordingPrepareExecutor::default();

        renderer
            .prepare_active_plan(&doc, &mut executor)
            .expect("active plan should build");

        assert_eq!(executor.calls, vec![nested, group, doc.root_id()]);
    }

    #[test]
    fn prepare_active_plan_restores_cached_chain_node_without_recursing() {
        let mut doc = new_doc();
        let left_group = doc
            .append_group(doc.root_id())
            .expect("left group should append");
        let right_group = doc
            .append_group(doc.root_id())
            .expect("right group should append");
        let left_layer = doc
            .append_layer(left_group)
            .expect("left layer should append");
        let right_layer = doc
            .append_layer(right_group)
            .expect("right layer should append");
        let mut renderer = GlaDocRenderer::new(new_render_backend());
        let mut executor = NoopPrepareExecutor;

        doc.set_active_layer(left_layer)
            .expect("active layer should update");
        renderer
            .prepare_active_plan(&doc, &mut executor)
            .expect("left plan should build");
        doc.set_active_layer(right_layer)
            .expect("active layer should update");
        renderer
            .prepare_active_plan(&doc, &mut executor)
            .expect("right plan should build");
        doc.set_active_layer(left_layer)
            .expect("active layer should update");
        renderer
            .prepare_active_plan(&doc, &mut executor)
            .expect("left plan should rebuild");

        let left_state = renderer
            .node_resources()
            .iter()
            .find(|entry| entry.node_id() == left_group)
            .expect("left group should exist");

        assert!(matches!(left_state.state(), RenderImageState::Active(_)));
    }

    #[test]
    fn render_active_tiles_runs_linear_passes_then_presents_root_tiles() {
        let mut doc = new_doc();
        let group = doc
            .append_group(doc.root_id())
            .expect("group should append");
        let first = doc.append_layer(group).expect("first should append");
        let second = doc.append_layer(group).expect("second should append");
        doc.set_active_layer(second)
            .expect("active layer should update");
        let mut renderer = GlaDocRenderer::new(new_render_backend());
        let mut prepare_executor = NoopPrepareExecutor;
        let mut render_executor = RecordingRenderExecutor::default();

        renderer
            .prepare_active_plan(&doc, &mut prepare_executor)
            .expect("active plan should build");
        renderer
            .render_active_tiles(&[3], &mut render_executor)
            .expect("active tiles should render");

        assert_eq!(render_executor.composites.len(), 2);
        assert_eq!(render_executor.composites[0].0, group);
        assert_eq!(render_executor.composites[0].1, vec![3]);
        assert_eq!(render_executor.composites[0].2[0].node_id(), first);
        assert_eq!(render_executor.composites[0].2[1].node_id(), second);
        assert_eq!(render_executor.composites[1].0, doc.root_id());
        assert_eq!(render_executor.presents, vec![(doc.root_id(), vec![3])]);
    }
}
