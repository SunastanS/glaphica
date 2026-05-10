use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasError, Backend as AtlasBackend, TileCredential};
use gla_document::{GlaDoc, GlaDocError, GlaNodeId, GlaNodeKind};
use gla_image::{
    GlaCachedImage, GlaCachedImageActivateError, GlaCachedImageCreateError, GlaImage,
    GlaImageCacheTileError, GlaImageCreateError, GlaImageEnsureActiveTileError,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetTileAction {
    Noop,
    Composite(atlas::TileKey),
}

#[derive(Debug, Clone)]
struct RenderCommandBuildStep {
    node_id: GlaNodeId,
    tile_indices: Vec<usize>,
    inputs: Vec<RenderProgramInput>,
}

#[derive(Debug)]
pub enum GlaDocRendererError {
    Document(GlaDocError),
    Atlas(AtlasError),
    CachedImageCreate(GlaCachedImageCreateError),
    CachedImageActivate(GlaCachedImageActivateError),
    ImageCreate(GlaImageCreateError),
    ImageTileAccess(GlaImageTileAccessError),
    TileRenderer(TileRendererError),
    MissingActivePlan,
}

impl Display for GlaDocRendererError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Document(error) => Display::fmt(error, f),
            Self::Atlas(error) => Display::fmt(error, f),
            Self::CachedImageCreate(error) => Display::fmt(error, f),
            Self::CachedImageActivate(error) => Display::fmt(error, f),
            Self::ImageCreate(error) => Display::fmt(error, f),
            Self::ImageTileAccess(error) => Display::fmt(error, f),
            Self::TileRenderer(error) => Display::fmt(error, f),
            Self::MissingActivePlan => f.write_str("missing active render plan"),
        }
    }
}

impl From<GlaCachedImageActivateError> for GlaDocRendererError {
    fn from(error: GlaCachedImageActivateError) -> Self {
        Self::CachedImageActivate(error)
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

impl From<GlaImageEnsureActiveTileError> for GlaDocRendererError {
    fn from(error: GlaImageEnsureActiveTileError) -> Self {
        match error {
            GlaImageEnsureActiveTileError::Atlas(e) => Self::Atlas(e),
            GlaImageEnsureActiveTileError::TileAccess(e) => Self::ImageTileAccess(e),
        }
    }
}

impl From<GlaImageCacheTileError> for GlaDocRendererError {
    fn from(error: GlaImageCacheTileError) -> Self {
        match error {
            GlaImageCacheTileError::Atlas(e) => Self::Atlas(e),
            GlaImageCacheTileError::TileAccess(e) => Self::ImageTileAccess(e),
        }
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
    ) -> Result<TileCredential, GlaDocRendererError> {
        if self.brush_preview_image.is_none() {
            self.brush_preview_image = Some(GlaImage::new(
                doc.layout(),
                doc.render_backend_ref().clone(),
            )?);
        }
        let preview_image = self
            .brush_preview_image
            .as_mut()
            .ok_or(GlaDocRendererError::MissingActivePlan)?;
        preview_image.ensure_active_tile_key(tile_index)?;
        Ok(preview_image.tile_credential(tile_index)?)
    }

    pub fn ensure_brush_preview_merge_target(
        &mut self,
        doc: &GlaDoc,
        tile_index: usize,
    ) -> Result<(TileCredential, TileCredential), GlaDocRendererError> {
        let origin_credential = doc.active_layer_image()?.tile_credential(tile_index)?;
        let preview_credential = self.ensure_brush_preview_tile(doc, tile_index)?;
        Ok((origin_credential, preview_credential))
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

    pub(crate) fn build_active_plan(
        &mut self,
        doc: &GlaDoc,
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
        tile_renderer.ensure_backend(device, &self.render_backend)?;
        self.build_active_plan(doc)?;

        let slot_count = usize::try_from(doc.layout().total_slots())
            .map_err(|_| GlaImageCreateError::TooManyTiles)?;
        let tile_indices = (0..slot_count).collect::<Vec<_>>();
        let active_layer_id = self
            .active_plan
            .as_ref()
            .map(|plan| plan.active_layer_id())
            .ok_or(GlaDocRendererError::MissingActivePlan)?;
        let prepare_steps: Vec<_> = self
            .active_plan
            .as_ref()
            .map(|plan| {
                plan.prepare_steps
                    .iter()
                    .map(|step| RenderCommandBuildStep {
                        node_id: step.node_id,
                        tile_indices: tile_indices.clone(),
                        inputs: step.inputs.clone(),
                    })
                    .collect()
            })
            .ok_or(GlaDocRendererError::MissingActivePlan)?;

        let commands = self.build_render_commands(doc, &prepare_steps, active_layer_id)?;
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
        let active_layer_id = self
            .active_plan
            .as_ref()
            .map(|plan| plan.active_layer_id())
            .ok_or(GlaDocRendererError::MissingActivePlan)?;
        let passes: Vec<_> = self
            .active_plan
            .as_ref()
            .map(|plan| {
                plan.passes
                    .iter()
                    .map(|pass| RenderCommandBuildStep {
                        node_id: pass.node_id,
                        tile_indices: tile_indices.to_vec(),
                        inputs: pass.inputs.clone(),
                    })
                    .collect()
            })
            .ok_or(GlaDocRendererError::MissingActivePlan)?;

        let commands = self.build_render_commands(doc, &passes, active_layer_id)?;
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

    fn build_render_commands(
        &mut self,
        doc: &GlaDoc,
        steps: &[RenderCommandBuildStep],
        active_layer_id: GlaNodeId,
    ) -> Result<Vec<RenderCommand>, GlaDocRendererError> {
        let mut commands = Vec::new();
        for step in steps {
            self.compose_node_commands(
                doc,
                step.node_id,
                &step.tile_indices,
                &step.inputs,
                active_layer_id,
                &mut commands,
            )?;
        }
        Ok(commands)
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

            let (layout, tile_owners) = active_image.into_tile_owners();
            let mut tile_keys = Vec::with_capacity(tile_owners.len());
            let mut non_empty_owners = Vec::new();
            for tile_owner in tile_owners {
                let tile_key = tile_owner.physical_tile_key();
                tile_keys.push(tile_key);
                if tile_key.is_some() {
                    non_empty_owners.push(tile_owner);
                }
            }

            let cached_group = self.render_backend.cache_active_owners(non_empty_owners)?;
            entry.state =
                RenderImageState::Cached(GlaCachedImage::new(layout, cached_group, tile_keys)?);
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
            entry.state = match state {
                RenderImageState::Active(image) => RenderImageState::Active(image),
                RenderImageState::Cached(cached) => {
                    RenderImageState::Active(cached.activate(&self.render_backend)?)
                }
                RenderImageState::Empty => {
                    newly_allocated_nodes.push(node_id);
                    RenderImageState::Active(GlaImage::new(
                        doc.layout(),
                        doc.render_backend_ref().clone(),
                    )?)
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

            match prepare_target_tile(target_image, tile_index, &sources)? {
                TargetTileAction::Noop => {}
                TargetTileAction::Composite(target_key) => output.push(
                    RenderCommand::CompositeTile(build_composite_command(target_key, &sources)),
                ),
            }
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
            let Some(tile_key) =
                self.source_tile_key(doc, input, tile_index, active_layer_id)?
            else {
                continue;
            };
            sources.push(TileCompositeSource {
                tile_key,
                opacity: input.opacity,
                blend_mode: input.blend_mode,
            });
        }
        Ok(sources)
    }

    fn source_tile_key(
        &self,
        doc: &GlaDoc,
        input: &RenderProgramInput,
        tile_index: usize,
        active_layer_id: GlaNodeId,
    ) -> Result<Option<atlas::TileKey>, GlaDocRendererError> {
        match input.source_kind {
            RenderProgramSourceKind::Truth if input.node_id == active_layer_id => {
                if let Some(preview_tile_key) = self
                    .brush_preview_image
                    .as_ref()
                    .map(|preview_node| preview_node.physical_tile_key(tile_index))
                    .transpose()?
                    .flatten()
                {
                    return Ok(Some(preview_tile_key));
                }
                Ok(doc.node_image(input.node_id)?.physical_tile_key(tile_index)?)
            }
            RenderProgramSourceKind::Truth => {
                Ok(doc.node_image(input.node_id)?.physical_tile_key(tile_index)?)
            }
            RenderProgramSourceKind::Result => {
                let entry = self
                    .node_resources
                    .iter()
                    .find(|entry| entry.node_id == input.node_id)
                    .ok_or(GlaDocError::InvalidNodeId(input.node_id))?;
                match &entry.state {
                    RenderImageState::Active(image) => Ok(image.physical_tile_key(tile_index)?),
                    RenderImageState::Cached(cached) => {
                        if tile_index >= cached.slot_count() {
                            return Err(GlaDocError::InvalidSlotIndex {
                                slot_index: tile_index,
                                slot_count: cached.slot_count(),
                            }
                            .into());
                        }
                        Ok(cached.physical_tile_key(tile_index))
                    }
                    RenderImageState::Empty => Ok(None),
                }
            }
        }
    }
}

fn prepare_target_tile(
    target_image: &mut GlaImage,
    tile_index: usize,
    sources: &[TileCompositeSource],
) -> Result<TargetTileAction, GlaDocRendererError> {
    if sources.is_empty() {
        if target_image.physical_tile_key(tile_index)?.is_some() {
            target_image.cache_tile(tile_index)?;
        }
        return Ok(TargetTileAction::Noop);
    }

    Ok(TargetTileAction::Composite(
        target_image.ensure_active_tile_key(tile_index)?,
    ))
}

fn build_composite_command(
    target_tile_key: atlas::TileKey,
    sources: &[TileCompositeSource],
) -> CompositeTileCommand {
    CompositeTileCommand {
        target_tile_key,
        inputs: sources.to_vec(),
    }
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

#[cfg(test)]
mod tests {
    use atlas::{AtlasLayout, Backend as AtlasBackend};
    use gla_document::{BackendId, GlaDoc, GlaImageLayout};
    use gla_image::GlaImage;
    use glaphica_core::BlendMode;
    use renderer::{RenderCommand, TileCompositeSource};

    use crate::{
        GlaDocRenderer, RenderCommandBuildStep, RenderImageState, RenderProgramSourceKind,
        TargetTileAction, build_composite_command, prepare_target_tile,
    };

    fn new_doc() -> GlaDoc {
        GlaDoc::new(
            GlaImageLayout::new(64, 64),
            AtlasBackend::new(AtlasLayout::Tiny8, BackendId::new(3)),
            AtlasBackend::new(AtlasLayout::Tiny8, BackendId::new(7)),
            AtlasBackend::new(AtlasLayout::Tiny8, BackendId::new(11)),
        )
        .expect("document should build")
    }

    fn new_render_backend() -> AtlasBackend {
        AtlasBackend::new(AtlasLayout::Tiny8, BackendId::new(7))
    }

    #[test]
    fn prepare_target_tile_caches_existing_target_when_sources_are_empty() {
        let backend = new_render_backend();
        let mut image = GlaImage::new(GlaImageLayout::new(64, 64), backend.clone())
            .expect("image should build");
        let tile_key = image
            .ensure_active_tile_key(0)
            .expect("target tile should allocate");

        let action = prepare_target_tile(&mut image, 0, &[]).expect("target should prepare");

        assert_eq!(action, TargetTileAction::Noop);
        assert!(image.tile_key(0).is_ok_and(|key| key.is_empty()));
        assert_eq!(backend.tile_state(tile_key), Ok(atlas::TileState::Cached));
    }

    #[test]
    fn prepare_target_tile_allocates_target_when_sources_exist() {
        let backend = new_render_backend();
        let source = backend.alloc_active().expect("source tile should allocate");
        let mut image =
            GlaImage::new(GlaImageLayout::new(64, 64), backend).expect("image should build");
        let sources = [TileCompositeSource {
            tile_key: source.tile_key(),
            opacity: 0.5,
            blend_mode: BlendMode::Normal,
        }];

        let action = prepare_target_tile(&mut image, 0, &sources).expect("target should prepare");

        assert!(matches!(action, TargetTileAction::Composite(key) if !key.is_empty()));
    }

    #[test]
    fn build_composite_command_keeps_physical_target_and_inputs() {
        let backend = new_render_backend();
        let target = backend
            .alloc_active()
            .expect("target tile should allocate")
            .tile_key();
        let source = backend
            .alloc_active()
            .expect("source tile should allocate")
            .tile_key();
        let sources = [TileCompositeSource {
            tile_key: source,
            opacity: 0.5,
            blend_mode: BlendMode::Normal,
        }];

        let command = build_composite_command(target, &sources);

        assert_eq!(command.target_tile_key, target);
        assert_eq!(command.inputs, sources);
    }

    #[test]
    fn ensure_brush_preview_merge_target_uses_active_layer_truth_and_render_cache() {
        let mut doc = new_doc();
        let layer_id = doc
            .append_layer(doc.root_id())
            .expect("layer should append");
        doc.set_active_layer(layer_id)
            .expect("active layer should update");

        let active_tile = doc
            .image_backend_ref()
            .alloc_active()
            .expect("active tile should allocate");
        let active_tile_key = active_tile.tile_key();
        doc.active_layer_image_mut()
            .expect("layer image should exist")
            .replace_tile_owner(0, active_tile)
            .expect("tile owner should install");

        let mut renderer = GlaDocRenderer::new(new_render_backend());
        let (origin_credential, preview_credential) = renderer
            .ensure_brush_preview_merge_target(&doc, 0)
            .expect("preview target should build");

        assert_eq!(
            doc.active_layer_image()
                .expect("layer image should exist")
                .tile_manager()
                .resolve_active_key(origin_credential),
            Ok(active_tile_key)
        );
        assert_eq!(
            renderer
                .brush_preview_image()
                .map(|image| image.tile_credential(0)),
            Some(Ok(preview_credential))
        );
        assert!(
            renderer
                .brush_preview_image()
                .and_then(|image| image.physical_tile_key(0).ok())
                .flatten()
                .is_some()
        );
    }

    #[test]
    fn active_layer_truth_falls_back_when_preview_tile_is_empty() {
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
        let inputs = vec![crate::RenderProgramInput {
            node_id: layer_id,
            source_kind: crate::RenderProgramSourceKind::Truth,
            opacity: 1.0,
            blend_mode: glaphica_core::BlendMode::Normal,
        }];

        let without_preview = renderer
            .collect_tile_sources(&doc, &inputs, 0, layer_id)
            .expect("source collection should succeed");
        assert_eq!(without_preview.len(), 1);
        assert_eq!(without_preview[0].tile_key, active_tile_key);

        renderer.brush_preview_image = Some(
            GlaImage::new(doc.layout(), doc.render_backend_ref().clone())
                .expect("preview image should build"),
        );
        let with_empty_preview = renderer
            .collect_tile_sources(&doc, &inputs, 0, layer_id)
            .expect("source collection should succeed");
        assert_eq!(with_empty_preview.len(), 1);
        assert_eq!(with_empty_preview[0].tile_key, active_tile_key);
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

        let plan = renderer
            .build_active_plan(&doc)
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

        renderer
            .build_active_plan(&doc)
            .expect("first active plan should build");
        doc.set_active_layer(right_layer)
            .expect("active layer should update");
        renderer
            .build_active_plan(&doc)
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

        let plan = renderer
            .build_active_plan(&doc)
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

        let plan = renderer
            .build_active_plan(&doc)
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

        let plan = renderer
            .build_active_plan(&doc)
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

        let plan = renderer
            .build_active_plan(&doc)
            .expect("active plan should build");

        let step_ids: Vec<_> = plan
            .prepare_steps()
            .iter()
            .map(|step| step.node_id())
            .collect();
        assert_eq!(step_ids, vec![nested, group, doc.root_id()]);
    }

    #[test]
    fn prepare_active_plan_restores_cached_chain_node_without_recursing() {
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
        let mut renderer = GlaDocRenderer::new(new_render_backend());

        doc.set_active_layer(left_layer)
            .expect("active layer should update");
        renderer
            .build_active_plan(&doc)
            .expect("left plan should build");
        let cached_tile = renderer
            .render_backend
            .alloc_active()
            .expect("cached tile should allocate");
        let cached_tile_key = cached_tile.tile_key();
        let left_entry = renderer
            .node_resources
            .iter_mut()
            .find(|entry| entry.node_id == left_nested)
            .expect("left nested should exist");
        let RenderImageState::Active(left_image) = &mut left_entry.state else {
            panic!("left group should be active");
        };
        left_image
            .replace_tile_owner(0, cached_tile)
            .expect("tile owner should install");
        doc.set_active_layer(right_layer)
            .expect("active layer should update");
        renderer
            .build_active_plan(&doc)
            .expect("right plan should build");
        assert_eq!(
            renderer.render_backend.tile_state(cached_tile_key),
            Ok(atlas::TileState::Cached)
        );
        doc.set_active_layer(left_layer)
            .expect("active layer should update");
        renderer
            .build_active_plan(&doc)
            .expect("left plan should rebuild");

        let left_state = renderer
            .node_resources()
            .iter()
            .find(|entry| entry.node_id() == left_nested)
            .expect("left nested should exist");

        let RenderImageState::Active(left_image) = left_state.state() else {
            panic!("left group should be active");
        };
        assert_eq!(left_image.tile_key(0), Ok(cached_tile_key));
        assert_eq!(
            renderer.render_backend.tile_state(cached_tile_key),
            Ok(atlas::TileState::Active)
        );
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

        let plan = renderer
            .build_active_plan(&doc)
            .expect("active plan should build");

        assert_eq!(plan.passes().len(), 2);
        assert_eq!(plan.passes()[0].node_id(), group);
        assert_eq!(plan.passes()[0].active_child_index(), 1);
        assert_eq!(plan.passes()[0].inputs()[0].node_id(), first);
        assert_eq!(plan.passes()[0].inputs()[1].node_id(), second);
        assert_eq!(plan.passes()[1].node_id(), doc.root_id());
        assert_eq!(plan.passes()[1].active_child_index(), 0);
        let last_ancestor = plan.ancestor_chain().last().copied();
        assert_eq!(last_ancestor, Some(doc.root_id()));
    }

    #[test]
    fn build_render_commands_composes_all_passes_with_active_layer_truth_fallback() {
        let image_backend = AtlasBackend::new(AtlasLayout::Tiny8, BackendId::new(3));
        let mut doc = new_doc();
        let group = doc
            .append_group(doc.root_id())
            .expect("group should append");
        let first = doc.append_layer(group).expect("first should append");
        let second = doc.append_layer(group).expect("second should append");
        doc.node_image_mut(first)
            .expect("first image should exist")
            .replace_tile_owner(0, image_backend.alloc_active().expect("first tile"))
            .expect("tile owner should install");
        let second_tile = image_backend
            .alloc_active()
            .expect("second tile should allocate");
        let second_tile_key = second_tile.tile_key();
        doc.node_image_mut(second)
            .expect("second image should exist")
            .replace_tile_owner(0, second_tile)
            .expect("tile owner should install");
        doc.set_active_layer(second)
            .expect("active layer should update");
        let mut renderer = GlaDocRenderer::new(new_render_backend());

        let plan = renderer
            .build_active_plan(&doc)
            .expect("active plan should build");
        let active_layer_id = plan.active_layer_id();
        let passes: Vec<_> = plan
            .passes()
            .iter()
            .map(|pass| RenderCommandBuildStep {
                node_id: pass.node_id(),
                tile_indices: vec![0],
                inputs: pass.inputs().to_vec(),
            })
            .collect();

        let commands = renderer
            .build_render_commands(&doc, &passes, active_layer_id)
            .expect("render commands should build");

        assert_eq!(commands.len(), 2);
        for command in &commands {
            let RenderCommand::CompositeTile(composite) = command else {
                panic!("expected CompositeTile command");
            };
            assert!(!composite.target_tile_key.is_empty());
            assert!(!composite.inputs.is_empty());
        }
        assert!(
            commands.iter().any(|cmd| match cmd {
                RenderCommand::CompositeTile(c) => c
                    .inputs
                    .iter()
                    .any(|input| input.tile_key == second_tile_key),
                _ => false,
            }),
            "composite command should contain active layer tile key through truth fallback"
        );
    }
}
