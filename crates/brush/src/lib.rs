use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasError, Backend, BackendId, CachedTileGroup, TileCredential, TileKey, TileOwner};
use gla_image::{GlaImageEnsureActiveTileError, GlaImageTileAccessError};
use gla_undo::GlaImageUndoError;
pub use glaphica_core::BrushId;
pub use glaphica_core::CanvasInput;
use glaphica_core::CanvasVec2;
use renderer::{BrushShaderProvider, BrushShaderSpec, BrushTileFormat, MergeTileCommand};
use smoother::BrushLatencyTraceState;

pub mod round;
pub mod sampler;
pub mod smoother;

pub use crate::sampler::{
    EquidistantCurveSampler, EquidistantSamplerCursor, EquidistantStrokeSampler, StrokeSampler,
};
pub use crate::smoother::{
    CurveKnot, DistanceOrTimeStrokeSmoother, FrozenCanvasSample, FrozenCanvasSpan,
    FrozenCanvasSpanBuffer, PassthroughStrokeSmoother, StrokeCurveBuffer, StrokeSmoother,
    StrokeSmootherError,
};

#[derive(Debug, Clone, PartialEq)]
pub struct BrushInputBlock {
    values: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrushInputBlockList {
    brush_id: BrushId,
    blocks: Vec<BrushInputBlock>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrushInput {
    pub brush_id: BrushId,
    pub blocks: BrushInputBlockList,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrushInputError {
    WrongBrush {
        expected: BrushId,
        actual: BrushId,
    },
    InvalidBlockLength {
        brush_id: BrushId,
        expected: usize,
        actual: usize,
    },
    InvalidBlockValue {
        brush_id: BrushId,
        block_index: usize,
        value_index: usize,
    },
    Smoother(StrokeSmootherError),
    Stroke(BrushStrokeError),
}

pub trait BrushInputProcessor: Send + Sync {
    fn begin_stroke(&self) -> Box<dyn BrushStrokeInputProcessor>;

    fn max_affected_radius_px(&self) -> u32;

    fn block_center(
        &self,
        input: &BrushInput,
        block_index: usize,
    ) -> Result<CanvasVec2, BrushInputError>;

    fn encode_apply_dab_payload(
        &self,
        input: &BrushInput,
        block_index: usize,
        slot_canvas_origin: CanvasVec2,
    ) -> Result<Vec<u8>, BrushInputError>;

    fn merge_payload(&self) -> Vec<u8>;
}

pub trait BrushStrokeInputProcessor: Send {
    fn reset(&mut self);

    fn push_canvas_inputs(&mut self, canvas_input: &[CanvasInput]) -> Result<(), BrushInputError>;

    fn finish_stroke(&mut self) -> Result<(), BrushInputError>;

    fn current_drawing_sample(&self) -> Option<FrozenCanvasSample>;

    fn drain_brush_input(&mut self) -> Result<Option<BrushInput>, BrushInputError>;
}

pub(crate) trait BrushStrokeSampler: Send {
    fn reset(&mut self);

    fn sample_brush_input(
        &mut self,
        spans: &FrozenCanvasSpanBuffer,
    ) -> Result<Option<BrushInput>, BrushInputError>;
}

pub(crate) struct SmoothedBrushStrokeInputProcessor {
    smoother: Box<dyn StrokeSmoother>,
    brush_sampler: Box<dyn BrushStrokeSampler>,
    frozen_spans: FrozenCanvasSpanBuffer,
    latency_trace: BrushLatencyTraceState,
}

impl SmoothedBrushStrokeInputProcessor {
    pub(crate) fn new(
        smoother: Box<dyn StrokeSmoother>,
        brush_sampler: Box<dyn BrushStrokeSampler>,
    ) -> Self {
        Self {
            smoother,
            brush_sampler,
            frozen_spans: FrozenCanvasSpanBuffer::new(),
            latency_trace: BrushLatencyTraceState::default(),
        }
    }
}

impl BrushStrokeInputProcessor for SmoothedBrushStrokeInputProcessor {
    fn reset(&mut self) {
        self.smoother.clear();
        self.brush_sampler.reset();
        self.frozen_spans.clear();
        self.latency_trace.clear();
    }

    fn push_canvas_inputs(&mut self, canvas_input: &[CanvasInput]) -> Result<(), BrushInputError> {
        for &input in canvas_input {
            self.latency_trace.record_input(input);
        }
        self.smoother.push_canvas_inputs(canvas_input)?;
        Ok(())
    }

    fn finish_stroke(&mut self) -> Result<(), BrushInputError> {
        self.smoother.finish_stroke();
        Ok(())
    }

    fn current_drawing_sample(&self) -> Option<FrozenCanvasSample> {
        self.smoother.current_drawing_sample()
    }

    fn drain_brush_input(&mut self) -> Result<Option<BrushInput>, BrushInputError> {
        self.frozen_spans.clear();
        self.smoother.pop_frozen_spans(&mut self.frozen_spans)?;
        if self.frozen_spans.is_empty() {
            self.latency_trace.trace_drain(0, 0);
            return Ok(None);
        }

        if let Some(sample) = self.current_drawing_sample() {
            self.latency_trace.record_current_draw(sample);
        }

        let encoded = self.brush_sampler.sample_brush_input(&self.frozen_spans)?;
        let emitted_blocks = encoded
            .as_ref()
            .map(|input| input.blocks.blocks().len())
            .unwrap_or(0);
        self.latency_trace
            .trace_drain(self.frozen_spans.knot_count(), emitted_blocks);
        Ok(encoded)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrushTile {
    pub tile_index: usize,
    pub brush_credential: TileCredential,
}

#[derive(Debug)]
struct SparseTileOwners {
    backend_id: BackendId,
    tile_indices: Vec<usize>,
    tile_owners: Vec<TileOwner>,
}

impl SparseTileOwners {
    fn new(backend_id: BackendId) -> Self {
        Self {
            backend_id,
            tile_indices: Vec::new(),
            tile_owners: Vec::new(),
        }
    }

    fn backend_id(&self) -> BackendId {
        self.backend_id
    }

    fn credential(&self, tile_index: usize) -> Option<TileCredential> {
        let sparse_index = self
            .tile_indices
            .iter()
            .position(|&stored_tile_index| stored_tile_index == tile_index)?;
        Some(self.tile_owners.get(sparse_index)?.credential())
    }

    fn tile_key(&self, tile_index: usize) -> Option<TileKey> {
        let sparse_index = self
            .tile_indices
            .iter()
            .position(|&stored_tile_index| stored_tile_index == tile_index)?;
        Some(self.tile_owners.get(sparse_index)?.tile_key())
    }

    fn credentials(&self) -> impl Iterator<Item = (usize, TileCredential)> + '_ {
        self.tile_indices
            .iter()
            .copied()
            .zip(self.tile_owners.iter().map(TileOwner::credential))
    }

    fn ensure_tile(&mut self, tile_index: usize, backend: &Backend) -> Result<TileKey, AtlasError> {
        if let Some(tile_key) = self.tile_key(tile_index) {
            return Ok(tile_key);
        }

        let tile_owner = backend.alloc_active()?;
        let tile_key = tile_owner.tile_key();
        self.tile_indices.push(tile_index);
        self.tile_owners.push(tile_owner);
        Ok(tile_key)
    }

    fn into_tile_owners(self) -> Vec<TileOwner> {
        self.tile_owners
    }
}

#[derive(Debug)]
pub struct BrushTileSet {
    tiles: SparseTileOwners,
}

impl BrushTileSet {
    pub fn backend_id(&self) -> BackendId {
        self.tiles.backend_id()
    }

    pub fn credential(&self, tile_index: usize) -> Option<TileCredential> {
        self.tiles.credential(tile_index)
    }

    pub fn tiles(&self) -> impl Iterator<Item = BrushTile> + '_ {
        self.tiles
            .credentials()
            .map(|(tile_index, brush_credential)| BrushTile {
                tile_index,
                brush_credential,
            })
    }

    pub fn into_tile_owners(self) -> Vec<TileOwner> {
        self.tiles.into_tile_owners()
    }
}

impl BrushInputBlock {
    pub fn new(values: Vec<f32>) -> Self {
        Self { values }
    }

    pub fn values(&self) -> &[f32] {
        &self.values
    }
}

impl BrushInputBlockList {
    pub fn new(brush_id: BrushId) -> Self {
        Self {
            brush_id,
            blocks: Vec::new(),
        }
    }

    pub fn brush_id(&self) -> BrushId {
        self.brush_id
    }

    pub fn blocks(&self) -> &[BrushInputBlock] {
        &self.blocks
    }

    pub fn push_block(&mut self, values: Vec<f32>) {
        self.blocks.push(BrushInputBlock::new(values));
    }
}

impl Display for BrushInputError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongBrush { expected, actual } => write!(
                f,
                "brush input is for brush {}, expected brush {}",
                actual.raw(),
                expected.raw()
            ),
            Self::InvalidBlockLength {
                brush_id,
                expected,
                actual,
            } => write!(
                f,
                "brush {} input block length mismatch: expected {}, got {}",
                brush_id.raw(),
                expected,
                actual
            ),
            Self::InvalidBlockValue {
                brush_id,
                block_index,
                value_index,
            } => write!(
                f,
                "brush {} input block {} contains invalid value at index {}",
                brush_id.raw(),
                block_index,
                value_index
            ),
            Self::Smoother(error) => Display::fmt(error, f),
            Self::Stroke(error) => Display::fmt(error, f),
        }
    }
}

impl Error for BrushInputError {}

impl From<StrokeSmootherError> for BrushInputError {
    fn from(error: StrokeSmootherError) -> Self {
        Self::Smoother(error)
    }
}

impl From<BrushStrokeError> for BrushInputError {
    fn from(error: BrushStrokeError) -> Self {
        Self::Stroke(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrokeSlotRecord {
    pub tile_index: usize,
    pub brush_credential: TileCredential,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrushStrokeError {
    Atlas(AtlasError),
    Image(GlaImageTileAccessError),
    ImageUndo(GlaImageUndoError),
    WrongImageBackend {
        expected: BackendId,
        actual: BackendId,
    },
}

impl Display for BrushStrokeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Atlas(error) => Display::fmt(error, f),
            Self::Image(error) => Display::fmt(error, f),
            Self::ImageUndo(error) => Display::fmt(error, f),
            Self::WrongImageBackend { expected, actual } => write!(
                f,
                "commit image backend is {}, but provided backend is {}",
                actual.raw(),
                expected.raw()
            ),
        }
    }
}

impl Error for BrushStrokeError {}

impl From<AtlasError> for BrushStrokeError {
    fn from(error: AtlasError) -> Self {
        Self::Atlas(error)
    }
}

impl From<GlaImageTileAccessError> for BrushStrokeError {
    fn from(error: GlaImageTileAccessError) -> Self {
        Self::Image(error)
    }
}

impl From<GlaImageUndoError> for BrushStrokeError {
    fn from(error: GlaImageUndoError) -> Self {
        Self::ImageUndo(error)
    }
}

impl From<GlaImageEnsureActiveTileError> for BrushStrokeError {
    fn from(error: GlaImageEnsureActiveTileError) -> Self {
        match error {
            GlaImageEnsureActiveTileError::Atlas(error) => Self::Atlas(error),
            GlaImageEnsureActiveTileError::TileAccess(error) => Self::Image(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StrokeCommitPlan {
    pub brush_id: BrushId,
    pub brush_payload: Vec<u8>,
    pub entries: Vec<StrokeCommitPlanEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrokeCommitPlanEntry {
    pub tile_index: usize,
    pub brush_credential: TileCredential,
}

#[must_use]
pub fn build_merge_command(
    brush_id: BrushId,
    brush_payload: &[u8],
    brush_tile_key: TileKey,
    origin_tile_key: TileKey,
    destination_tile_key: TileKey,
) -> MergeTileCommand {
    MergeTileCommand {
        brush_id,
        origin_tile_key,
        brush_tile_key,
        destination_tile_key,
        brush_payload: brush_payload.to_vec(),
    }
}

#[derive(Debug)]
pub struct BrushBackend {
    brush_id: BrushId,
    brush_backend: Backend,
    brush_backend_id: BackendId,
    brush_format: BrushTileFormat,
    stroke_history_groups: Vec<CachedTileGroup>,
}

impl BrushBackend {
    pub fn new(
        brush_id: BrushId,
        brush_backend: Backend,
        brush_format: BrushTileFormat,
    ) -> Result<Self, BrushStrokeError> {
        let brush_backend_id = brush_backend.backend_id();
        Ok(Self {
            brush_id,
            brush_backend,
            brush_backend_id,
            brush_format,
            stroke_history_groups: Vec::new(),
        })
    }

    pub fn brush_id(&self) -> BrushId {
        self.brush_id
    }

    pub fn brush_backend(&self) -> &Backend {
        &self.brush_backend
    }

    pub fn brush_backend_id(&self) -> BackendId {
        self.brush_backend_id
    }

    pub fn brush_tile_format(&self) -> BrushTileFormat {
        self.brush_format
    }

    pub fn stroke_history_groups(&self) -> &[CachedTileGroup] {
        &self.stroke_history_groups
    }

    pub fn begin_stroke(&self) -> BrushStrokeState {
        BrushStrokeState {
            brush_id: self.brush_id,
            brush_backend: self.brush_backend.clone(),
            brush_backend_id: self.brush_backend_id,
            brush_tile_set: BrushTileSet {
                tiles: SparseTileOwners::new(self.brush_backend_id),
            },
            touched_tiles: Vec::new(),
        }
    }

    pub fn archive_stroke(
        &mut self,
        stroke: BrushStrokeState,
    ) -> Result<CachedTileGroup, BrushStrokeError> {
        if stroke.brush_id != self.brush_id || stroke.brush_backend_id != self.brush_backend_id {
            return Err(AtlasError::WrongBackend.into());
        }

        let cached_group = self
            .brush_backend
            .cache_active_owners(stroke.into_brush_tiles().into_tile_owners())?;
        self.stroke_history_groups.push(cached_group.clone());
        Ok(cached_group)
    }
}

#[derive(Debug)]
pub struct BrushStrokeState {
    brush_id: BrushId,
    brush_backend: Backend,
    brush_backend_id: BackendId,
    brush_tile_set: BrushTileSet,
    touched_tiles: Vec<StrokeSlotRecord>,
}

impl BrushStrokeState {
    pub fn new(brush_id: BrushId, brush_backend: Backend) -> Result<Self, BrushStrokeError> {
        let brush_backend_id = brush_backend.backend_id();
        Ok(Self {
            brush_id,
            brush_backend,
            brush_backend_id,
            brush_tile_set: BrushTileSet {
                tiles: SparseTileOwners::new(brush_backend_id),
            },
            touched_tiles: Vec::new(),
        })
    }

    pub fn brush_id(&self) -> BrushId {
        self.brush_id
    }

    pub fn brush_backend_id(&self) -> BackendId {
        self.brush_backend_id
    }

    pub fn brush_tiles(&self) -> &BrushTileSet {
        &self.brush_tile_set
    }

    pub fn into_brush_tiles(self) -> BrushTileSet {
        self.brush_tile_set
    }

    pub fn touched_tiles(&self) -> &[StrokeSlotRecord] {
        &self.touched_tiles
    }

    fn touched_tile_index(&self, tile_index: usize) -> Option<usize> {
        self.touched_tiles
            .iter()
            .position(|record| record.tile_index == tile_index)
    }

    fn ensure_touched_tile(
        &mut self,
        tile_index: usize,
    ) -> Result<&mut StrokeSlotRecord, BrushStrokeError> {
        if let Some(index) = self.touched_tile_index(tile_index) {
            return self
                .touched_tiles
                .get_mut(index)
                .ok_or(AtlasError::InvalidState.into());
        }

        self.brush_tile_set
            .tiles
            .ensure_tile(tile_index, &self.brush_backend)?;
        let brush_credential = self
            .brush_tile_set
            .credential(tile_index)
            .ok_or(AtlasError::InvalidState)?;
        self.touched_tiles.push(StrokeSlotRecord {
            tile_index,
            brush_credential,
        });
        self.touched_tiles
            .last_mut()
            .ok_or(AtlasError::InvalidState.into())
    }

    pub fn push_apply_dab(
        &mut self,
        tile_index: usize,
    ) -> Result<TileCredential, BrushStrokeError> {
        let record = self.ensure_touched_tile(tile_index)?;
        Ok(record.brush_credential)
    }

    pub fn preview_brush_tile_credential(&self, tile_index: usize) -> Option<TileCredential> {
        let record_index = self.touched_tile_index(tile_index)?;
        Some(self.touched_tiles.get(record_index)?.brush_credential)
    }

    pub fn build_commit_plan(&self, brush_payload: Vec<u8>) -> StrokeCommitPlan {
        let entries = self
            .touched_tiles
            .iter()
            .map(|record| StrokeCommitPlanEntry {
                tile_index: record.tile_index,
                brush_credential: record.brush_credential,
            })
            .collect();
        StrokeCommitPlan {
            brush_id: self.brush_id,
            brush_payload,
            entries,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrushShaderRegistration {
    pub brush_id: BrushId,
    pub shader_spec: BrushShaderSpec,
}

pub struct BrushRegistration {
    pub brush_id: BrushId,
    pub shader_spec: BrushShaderSpec,
    pub backend: BrushBackend,
    pub input_processor: Box<dyn BrushInputProcessor>,
}

#[derive(Default)]
pub struct BrushRegistry {
    registrations: Vec<BrushRegistration>,
}

impl BrushRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_builtin_round(brush_backend: Backend) -> Self {
        Self::with_builtin_round_processor(
            brush_backend,
            round::RoundBrushInputProcessor::default(),
        )
    }

    pub fn with_builtin_round_settings(
        brush_backend: Backend,
        settings: round::RoundBrushSettings,
    ) -> Self {
        Self::with_builtin_round_processor(
            brush_backend,
            round::RoundBrushInputProcessor::from(settings),
        )
    }

    pub fn with_builtin_round_processor(
        brush_backend: Backend,
        input_processor: round::RoundBrushInputProcessor,
    ) -> Self {
        let mut registry = Self::new();
        registry
            .register(
                round::ROUND_SHADER_REGISTRATION,
                brush_backend,
                Box::new(input_processor),
            )
            .expect("builtin round backend should register");
        registry
    }

    pub fn update_round_brush_settings(&mut self, settings: round::RoundBrushSettings) -> bool {
        self.replace_input_processor(
            round::ROUND_BRUSH_ID,
            Box::new(round::RoundBrushInputProcessor::from(settings)),
        )
    }

    pub fn register(
        &mut self,
        shader_registration: BrushShaderRegistration,
        brush_backend: Backend,
        input_processor: Box<dyn BrushInputProcessor>,
    ) -> Result<(), BrushStrokeError> {
        let registration = BrushRegistration {
            brush_id: shader_registration.brush_id,
            shader_spec: shader_registration.shader_spec,
            backend: BrushBackend::new(
                shader_registration.brush_id,
                brush_backend,
                shader_registration.shader_spec.brush_tile_format,
            )?,
            input_processor,
        };
        if let Some(index) = self
            .registrations
            .iter()
            .position(|candidate| candidate.brush_id == registration.brush_id)
        {
            self.registrations[index] = registration;
            return Ok(());
        }
        self.registrations.push(registration);
        Ok(())
    }

    pub fn shader_spec(&self, brush_id: BrushId) -> Option<BrushShaderSpec> {
        self.registrations
            .iter()
            .find(|registration| registration.brush_id == brush_id)
            .map(|registration| registration.shader_spec)
    }

    pub fn registration(&self, brush_id: BrushId) -> Option<&BrushRegistration> {
        self.registrations
            .iter()
            .find(|registration| registration.brush_id == brush_id)
    }

    pub fn backend(&self, brush_id: BrushId) -> Option<&BrushBackend> {
        self.registration(brush_id)
            .map(|registration| &registration.backend)
    }

    pub fn input_processor(&self, brush_id: BrushId) -> Option<&dyn BrushInputProcessor> {
        self.registration(brush_id)
            .map(|registration| registration.input_processor.as_ref())
    }

    pub fn replace_input_processor(
        &mut self,
        brush_id: BrushId,
        input_processor: Box<dyn BrushInputProcessor>,
    ) -> bool {
        let Some(registration) = self
            .registrations
            .iter_mut()
            .find(|registration| registration.brush_id == brush_id)
        else {
            return false;
        };
        registration.input_processor = input_processor;
        true
    }

    pub fn backend_mut(&mut self, brush_id: BrushId) -> Option<&mut BrushBackend> {
        self.registrations
            .iter_mut()
            .find(|registration| registration.brush_id == brush_id)
            .map(|registration| &mut registration.backend)
    }

    pub fn begin_stroke(&self, brush_id: BrushId) -> Option<BrushStrokeState> {
        self.backend(brush_id).map(BrushBackend::begin_stroke)
    }

    pub fn begin_input_stroke(
        &self,
        brush_id: BrushId,
    ) -> Result<Box<dyn BrushStrokeInputProcessor>, BrushInputError> {
        let processor = self
            .input_processor(brush_id)
            .ok_or(BrushInputError::WrongBrush {
                expected: brush_id,
                actual: brush_id,
            })?;
        Ok(processor.begin_stroke())
    }

    pub fn max_affected_radius_px(&self, brush_id: BrushId) -> Option<u32> {
        self.input_processor(brush_id)
            .map(BrushInputProcessor::max_affected_radius_px)
    }

    pub fn block_center(
        &self,
        input: &BrushInput,
        block_index: usize,
    ) -> Result<CanvasVec2, BrushInputError> {
        let processor =
            self.input_processor(input.brush_id)
                .ok_or(BrushInputError::WrongBrush {
                    expected: input.brush_id,
                    actual: input.brush_id,
                })?;
        processor.block_center(input, block_index)
    }

    pub fn encode_apply_dab_payload(
        &self,
        input: &BrushInput,
        block_index: usize,
        slot_canvas_origin: CanvasVec2,
    ) -> Result<Vec<u8>, BrushInputError> {
        let processor =
            self.input_processor(input.brush_id)
                .ok_or(BrushInputError::WrongBrush {
                    expected: input.brush_id,
                    actual: input.brush_id,
                })?;
        processor.encode_apply_dab_payload(input, block_index, slot_canvas_origin)
    }

    pub fn merge_payload(&self, brush_id: BrushId) -> Option<Vec<u8>> {
        let processor = self.input_processor(brush_id)?;
        Some(processor.merge_payload())
    }
}

impl BrushShaderProvider for BrushRegistry {
    fn shader_spec(&self, brush_id: BrushId) -> Option<BrushShaderSpec> {
        self.shader_spec(brush_id)
    }
}

#[cfg(test)]
mod tests {
    use atlas::{AtlasLayout, BackendId, TileCredential, TileManager, TileState};
    use gla_image::GlaImageTileAccessError;
    use glaphica_core::CanvasVec2;
    use glaphica_core::IMAGE_TILE_SIZE;
    use renderer::{ApplyDabCommand, CopyTileCommand, MergeTileCommand, RenderCommand};

    use crate::build_merge_command;
    use crate::round::{
        ROUND_SHADER_SPEC, RoundBrushInputProcessor, RoundMergeSettings,
        encode_round_apply_payload, encode_round_merge_payload,
    };
    use crate::{
        BrushBackend, BrushId, BrushRegistry, BrushShaderRegistration, BrushStrokeState,
        CanvasInput,
    };
    use atlas::Backend;
    use gla_image::{GlaImage, GlaImageLayout};
    use gla_undo::GlaImageUndo;

    fn resolve_tile(tile_manager: &TileManager, credential: TileCredential) -> atlas::TileKey {
        tile_manager
            .resolve(credential)
            .expect("credential should resolve")
            .expect("brush tile should be active")
    }

    #[test]
    fn apply_dab_allocates_brush_tile_once() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(7));
        let tile_manager = TileManager::from(backend.clone());
        let mut state =
            BrushStrokeState::new(BrushId::new(5), backend.clone()).expect("state should build");
        let mut commands = Vec::new();

        let first_payload = encode_round_apply_payload([8.0, 9.0], 10.0, 0.75);
        let second_payload = encode_round_apply_payload([3.0, 4.0], 5.0, 0.25);
        let first_credential = state.push_apply_dab(4).expect("dab should build");
        let first = resolve_tile(&tile_manager, first_credential);
        commands.push(RenderCommand::ApplyDab(ApplyDabCommand {
            brush_id: state.brush_id(),
            destination_tile_key: first,
            source_tile_key: None,
            brush_payload: first_payload.clone(),
        }));
        let second_credential = state.push_apply_dab(4).expect("dab should build");
        let second = resolve_tile(&tile_manager, second_credential);
        commands.push(RenderCommand::ApplyDab(ApplyDabCommand {
            brush_id: state.brush_id(),
            destination_tile_key: second,
            source_tile_key: Some(first),
            brush_payload: second_payload.clone(),
        }));

        assert_eq!(first, second);
        assert_eq!(first_credential, second_credential);
        assert_eq!(state.brush_tiles().credential(4), Some(first_credential));
        assert_eq!(
            commands,
            vec![
                RenderCommand::ApplyDab(ApplyDabCommand {
                    brush_id: BrushId::new(5),
                    destination_tile_key: first,
                    source_tile_key: None,
                    brush_payload: first_payload,
                }),
                RenderCommand::ApplyDab(ApplyDabCommand {
                    brush_id: BrushId::new(5),
                    destination_tile_key: first,
                    source_tile_key: Some(first),
                    brush_payload: second_payload,
                }),
            ]
        );
        assert_eq!(backend.tile_state(first), Ok(TileState::Active));
    }

    #[test]
    fn preview_merge_uses_virtual_preview_node_tile() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(7));
        let tile_manager = TileManager::from(backend.clone());
        let active_tile = backend.alloc_active().expect("active tile");
        let active_tile_key = active_tile.tile_key();

        let mut state =
            BrushStrokeState::new(BrushId::new(9), backend).expect("state should build");
        let mut commands = Vec::new();
        let brush_credential = state.push_apply_dab(0).expect("dab");
        let brush_tile_key = resolve_tile(&tile_manager, brush_credential);
        commands.clear();
        let preview_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(13));
        let preview_tile_key = preview_backend
            .alloc_active()
            .expect("preview tile")
            .tile_key();
        let merge_payload = encode_round_merge_payload(RoundMergeSettings {
            tint: [0.2, 0.3, 0.4],
            opacity: 1.0,
            stroke_flow: 1.0,
            spacing_ratio: 1.0,
            hardness: 0.7,
        });

        let returned_brush_credential = state
            .preview_brush_tile_credential(0)
            .expect("preview merge should find brush tile");
        let returned_brush_tile_key = resolve_tile(&tile_manager, returned_brush_credential);
        commands.push(RenderCommand::MergeTile(MergeTileCommand {
            brush_id: state.brush_id(),
            origin_tile_key: active_tile_key,
            brush_tile_key: returned_brush_tile_key,
            destination_tile_key: preview_tile_key,
            brush_payload: merge_payload.clone(),
        }));

        assert_eq!(returned_brush_tile_key, brush_tile_key);
        assert_eq!(
            commands,
            vec![RenderCommand::MergeTile(MergeTileCommand {
                brush_id: BrushId::new(9),
                origin_tile_key: active_tile_key,
                brush_tile_key,
                destination_tile_key: preview_tile_key,
                brush_payload: merge_payload,
            })]
        );
    }

    #[test]
    fn commit_plan_executes_backup_then_merge() {
        let brush_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(7));
        let image_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(3));
        let backup_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(5));
        let mut image = GlaImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE),
            image_backend.clone(),
        )
        .expect("image should create");
        let first_active = image_backend.alloc_active().expect("first active");
        let first_active_key = first_active.tile_key();
        image
            .replace_tile_owner(0, first_active)
            .expect("install tile");

        let brush_tile_manager = TileManager::from(brush_backend.clone());
        let mut state =
            BrushStrokeState::new(BrushId::new(11), brush_backend).expect("state should build");
        let image_undo = GlaImageUndo::new(image_backend.clone(), backup_backend);
        let first_brush_credential = state.push_apply_dab(0).expect("dab");
        let second_brush_credential = state.push_apply_dab(1).expect("dab");
        let first_brush_tile = resolve_tile(&brush_tile_manager, first_brush_credential);
        let second_brush_tile = resolve_tile(&brush_tile_manager, second_brush_credential);
        let merge_payload = encode_round_merge_payload(RoundMergeSettings {
            tint: [0.1, 0.2, 0.3],
            opacity: 1.0,
            stroke_flow: 1.0,
            spacing_ratio: 1.0,
            hardness: 0.7,
        });

        let plan = state.build_commit_plan(merge_payload.clone());
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.brush_id, BrushId::new(11));

        let source_credentials: Vec<(usize, TileCredential)> = plan
            .entries
            .iter()
            .map(|e| Ok((e.tile_index, image.tile_credential(e.tile_index)?)))
            .collect::<Result<_, GlaImageTileAccessError>>()
            .expect("credentials should collect");
        let backup_result = image_undo
            .execute_backup(&source_credentials)
            .expect("backup should succeed");

        let mut commands: Vec<RenderCommand> = backup_result.commands;
        for entry in &plan.entries {
            let destination_tile_key = image
                .ensure_active_tile_key(entry.tile_index)
                .expect("tile should activate");
            let brush_tile_key = brush_tile_manager
                .resolve(entry.brush_credential)
                .expect("brush credential should resolve")
                .expect("brush tile should be active");
            let origin_tile_key = backup_result
                .origin_keys
                .iter()
                .find(|(idx, _)| *idx == entry.tile_index)
                .map(|(_, key)| *key)
                .expect("origin key should exist");
            commands.push(RenderCommand::MergeTile(build_merge_command(
                plan.brush_id,
                &plan.brush_payload,
                brush_tile_key,
                origin_tile_key,
                destination_tile_key,
            )));
        }

        let second_active_key = image.tile_key(1).expect("tile key should exist");
        assert!(!second_active_key.is_empty());
        let tile_0_backup_key = backup_result
            .origin_keys
            .iter()
            .find(|(idx, _)| *idx == 0)
            .map(|(_, key)| *key)
            .expect("tile 0 should have a backup key");
        assert_eq!(
            commands,
            vec![
                RenderCommand::CopyTile(CopyTileCommand {
                    source_tile_key: first_active_key,
                    destination_tile_key: tile_0_backup_key,
                }),
                RenderCommand::MergeTile(build_merge_command(
                    BrushId::new(11),
                    &merge_payload,
                    first_brush_tile,
                    tile_0_backup_key,
                    first_active_key,
                )),
                RenderCommand::MergeTile(build_merge_command(
                    BrushId::new(11),
                    &merge_payload,
                    second_brush_tile,
                    image_undo.backup_backend().empty_tile_key(),
                    second_active_key,
                )),
            ]
        );
        let tile_1_backup_key = backup_result
            .origin_keys
            .iter()
            .find(|(idx, _)| *idx == 1)
            .map(|(_, key)| *key)
            .expect("tile 1 should be in origin_keys");
        assert!(
            tile_1_backup_key.is_empty(),
            "tile 1 was empty in image, should have empty backup key"
        );
    }

    #[test]
    fn commit_plan_builds_pure_entries_from_touched_tiles() {
        let brush_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(7));
        let brush_tile_manager = TileManager::from(brush_backend.clone());
        let mut state =
            BrushStrokeState::new(BrushId::new(13), brush_backend).expect("state should build");
        let first_brush_credential = state.push_apply_dab(0).expect("dab");
        let second_brush_credential = state.push_apply_dab(3).expect("dab");
        let first_brush_tile = resolve_tile(&brush_tile_manager, first_brush_credential);
        let second_brush_tile = resolve_tile(&brush_tile_manager, second_brush_credential);

        let plan = state.build_commit_plan(vec![0xAB, 0xCD]);

        assert_eq!(plan.brush_id, BrushId::new(13));
        assert_eq!(plan.brush_payload, vec![0xAB, 0xCD]);
        assert_eq!(plan.entries.len(), 2);

        assert_eq!(plan.entries[0].tile_index, 0);
        assert_eq!(plan.entries[1].tile_index, 3);

        assert_ne!(
            plan.entries[0].brush_credential,
            plan.entries[1].brush_credential
        );

        let record_0 = state
            .touched_tiles()
            .iter()
            .find(|r| r.tile_index == 0)
            .expect("tile 0 should be touched");
        let record_3 = state
            .touched_tiles()
            .iter()
            .find(|r| r.tile_index == 3)
            .expect("tile 3 should be touched");
        assert_eq!(plan.entries[0].brush_credential, record_0.brush_credential);
        assert_eq!(plan.entries[1].brush_credential, record_3.brush_credential);
        assert_eq!(
            brush_tile_manager
                .resolve(record_0.brush_credential)
                .expect("credential should resolve"),
            Some(first_brush_tile)
        );
        assert_eq!(
            brush_tile_manager
                .resolve(record_3.brush_credential)
                .expect("credential should resolve"),
            Some(second_brush_tile)
        );
    }

    #[test]
    fn preview_merge_uses_explicit_origin_tile_key() {
        let brush_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(7));
        let brush_tile_manager = TileManager::from(brush_backend.clone());
        let origin_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(3));
        let active_tile = origin_backend.alloc_active().expect("active tile");
        let active_tile_key = active_tile.tile_key();
        let mut state =
            BrushStrokeState::new(BrushId::new(11), brush_backend).expect("state should build");
        let mut commands = Vec::new();
        let brush_credential = state.push_apply_dab(0).expect("dab");
        let brush_tile_key = resolve_tile(&brush_tile_manager, brush_credential);
        commands.clear();
        let preview_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(13));
        let preview_tile_key = preview_backend
            .alloc_active()
            .expect("preview tile")
            .tile_key();
        let merge_payload = encode_round_merge_payload(RoundMergeSettings {
            tint: [0.9, 0.8, 0.7],
            opacity: 1.0,
            stroke_flow: 1.0,
            spacing_ratio: 1.0,
            hardness: 0.7,
        });

        let returned_brush_credential = state
            .preview_brush_tile_credential(0)
            .expect("preview merge should find brush tile");
        let returned_brush_tile_key = resolve_tile(&brush_tile_manager, returned_brush_credential);
        commands.push(RenderCommand::MergeTile(MergeTileCommand {
            brush_id: state.brush_id(),
            origin_tile_key: active_tile_key,
            brush_tile_key: returned_brush_tile_key,
            destination_tile_key: preview_tile_key,
            brush_payload: merge_payload.clone(),
        }));
        assert_eq!(returned_brush_tile_key, brush_tile_key);

        assert_eq!(
            commands,
            vec![RenderCommand::MergeTile(MergeTileCommand {
                brush_id: BrushId::new(11),
                origin_tile_key: active_tile_key,
                brush_tile_key,
                destination_tile_key: preview_tile_key,
                brush_payload: merge_payload,
            })]
        );
    }

    #[test]
    fn atlas_backend_retires_stroke_brush_tiles_as_cached_group() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(7));
        let tile_manager = TileManager::from(backend.clone());
        let mut brush_backend = BrushBackend::new(
            BrushId::new(17),
            backend.clone(),
            renderer::BrushTileFormat::Rgba8Unorm,
        )
        .expect("backend should build");
        let mut state = brush_backend.begin_stroke();
        let active_credential = state.push_apply_dab(0).expect("dab");
        let active_tile_key = resolve_tile(&tile_manager, active_credential);

        let cached_group = brush_backend
            .archive_stroke(state)
            .expect("stroke should retire");

        assert_eq!(cached_group.keys(), &[active_tile_key]);
        assert_eq!(
            brush_backend.stroke_history_groups(),
            &[cached_group.clone()]
        );
        assert_eq!(backend.tile_state(active_tile_key), Ok(TileState::Cached));

        let mut next_state = brush_backend.begin_stroke();
        let next_credential = next_state.push_apply_dab(0).expect("next dab");
        let next_tile_key = resolve_tile(&tile_manager, next_credential);
        assert_ne!(next_tile_key, active_tile_key);
        assert_eq!(backend.tile_state(next_tile_key), Ok(TileState::Active));
    }

    #[test]
    fn registry_registers_shader_and_backend_together() {
        let brush_id = BrushId::new(23);
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(9));
        let tile_manager = TileManager::from(backend.clone());
        let mut registry = BrushRegistry::new();

        registry
            .register(
                BrushShaderRegistration {
                    brush_id,
                    shader_spec: ROUND_SHADER_SPEC,
                },
                backend,
                Box::new(RoundBrushInputProcessor::default()),
            )
            .expect("registration should succeed");

        assert_eq!(registry.shader_spec(brush_id), Some(ROUND_SHADER_SPEC));
        let mut stroke = registry
            .begin_stroke(brush_id)
            .expect("stroke should build");
        let credential = stroke.push_apply_dab(0).expect("dab");
        let tile_key = resolve_tile(&tile_manager, credential);
        let backend = registry
            .backend_mut(brush_id)
            .expect("backend should exist");
        let cached_group = backend.archive_stroke(stroke).expect("cache");

        assert_eq!(cached_group.keys(), &[tile_key]);
        assert_eq!(backend.stroke_history_groups(), &[cached_group]);
    }

    #[test]
    fn registry_begins_stateful_input_stroke() {
        let brush_id = crate::round::ROUND_BRUSH_ID;
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(11));
        let mut registry = BrushRegistry::new();
        registry
            .register(
                BrushShaderRegistration {
                    brush_id,
                    shader_spec: ROUND_SHADER_SPEC,
                },
                backend,
                Box::new(RoundBrushInputProcessor::default()),
            )
            .expect("registration should succeed");

        let mut stroke = registry
            .begin_input_stroke(brush_id)
            .expect("input stroke should build");

        stroke
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 1,
                    position: CanvasVec2::new(2.0, 3.0),
                    pressure: 0.6,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 2,
                    position: CanvasVec2::new(8.0, 3.0),
                    pressure: 0.6,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("push canvas input");
        let input = stroke
            .drain_brush_input()
            .expect("drain brush input")
            .expect("brush input should exist");

        assert_eq!(input.brush_id, brush_id);
        assert!(!input.blocks.blocks().is_empty());
    }
}
