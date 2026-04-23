use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasError, Backend, BackendId, CachedTileGroup, TileKey, TileOwner};
use gla_document::DocumentBackupStore;
use gla_image::{GlaImage, GlaImageEnsureActiveTileError, GlaImageTileAccessError};
pub use glaphica_core::BrushId;
pub use glaphica_core::CanvasInput;
use glaphica_core::CanvasVec2;
use renderer::{
    ApplyDabCommand, BrushShaderSpec, CopyTileCommand, MergeTileCommand, RenderCommand,
};
use smoother::BrushLatencyTraceState;

pub mod round;
pub mod sampler;
pub mod smoother;

pub use crate::sampler::{
    EquidistantCurveSampler, EquidistantSamplerCursor, EquidistantStrokeSampler, StrokeSampler,
};
pub use crate::smoother::{
    CommittedCanvasSample, CommittedCanvasSpan, CommittedCanvasSpanBuffer, CurveKnot,
    DistanceOrTimeStrokeSmoother, PassthroughStrokeSmoother, StrokeCurveBuffer, StrokeSmoother,
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
        tile_canvas_origin: CanvasVec2,
    ) -> Result<Vec<u8>, BrushInputError>;

    fn encode_merge_payload(&self, input: &BrushInput) -> Result<Vec<u8>, BrushInputError>;
}

pub trait BrushStrokeInputProcessor: Send {
    fn reset(&mut self);

    fn push_canvas_inputs(&mut self, canvas_input: &[CanvasInput]) -> Result<(), BrushInputError>;

    fn finish_stroke(&mut self) -> Result<(), BrushInputError>;

    fn current_drawing_sample(&self) -> Option<CommittedCanvasSample>;

    fn drain_brush_input(&mut self) -> Result<Option<BrushInput>, BrushInputError>;
}

pub(crate) trait BrushStrokeSampler: Send {
    fn reset(&mut self);

    fn sample_brush_input(
        &mut self,
        spans: &CommittedCanvasSpanBuffer,
    ) -> Result<Option<BrushInput>, BrushInputError>;
}

pub(crate) struct SmoothedBrushStrokeInputProcessor {
    smoother: Box<dyn StrokeSmoother>,
    brush_sampler: Box<dyn BrushStrokeSampler>,
    committed_spans: CommittedCanvasSpanBuffer,
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
            committed_spans: CommittedCanvasSpanBuffer::new(),
            latency_trace: BrushLatencyTraceState::default(),
        }
    }
}

impl BrushStrokeInputProcessor for SmoothedBrushStrokeInputProcessor {
    fn reset(&mut self) {
        self.smoother.clear();
        self.brush_sampler.reset();
        self.committed_spans.clear();
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

    fn current_drawing_sample(&self) -> Option<CommittedCanvasSample> {
        self.smoother.current_drawing_sample()
    }

    fn drain_brush_input(&mut self) -> Result<Option<BrushInput>, BrushInputError> {
        self.committed_spans.clear();
        self.smoother
            .pop_committed_spans(&mut self.committed_spans)?;
        if self.committed_spans.is_empty() {
            self.latency_trace.trace_drain(0, 0);
            return Ok(None);
        }

        if let Some(sample) = self.current_drawing_sample() {
            self.latency_trace.record_current_draw(sample);
        }

        let encoded = self
            .brush_sampler
            .sample_brush_input(&self.committed_spans)?;
        let emitted_blocks = encoded
            .as_ref()
            .map(|input| input.blocks.blocks().len())
            .unwrap_or(0);
        self.latency_trace
            .trace_drain(self.committed_spans.len(), emitted_blocks);
        Ok(encoded)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrushIntermediateTile {
    pub tile_index: usize,
    pub tile_key: TileKey,
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

    fn tile_key(&self, tile_index: usize) -> Option<TileKey> {
        let sparse_index = self
            .tile_indices
            .iter()
            .position(|&stored_tile_index| stored_tile_index == tile_index)?;
        Some(self.tile_owners.get(sparse_index)?.tile_key())
    }

    fn tiles(&self) -> impl Iterator<Item = (usize, TileKey)> + '_ {
        self.tile_indices
            .iter()
            .copied()
            .zip(self.tile_owners.iter().map(TileOwner::tile_key))
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
pub struct BrushIntermediate {
    tiles: SparseTileOwners,
}

impl BrushIntermediate {
    pub fn backend_id(&self) -> BackendId {
        self.tiles.backend_id()
    }

    pub fn tile_key(&self, tile_index: usize) -> Option<TileKey> {
        self.tiles.tile_key(tile_index)
    }

    pub fn tiles(&self) -> impl Iterator<Item = BrushIntermediateTile> + '_ {
        self.tiles
            .tiles()
            .map(|(tile_index, tile_key)| BrushIntermediateTile {
                tile_index,
                tile_key,
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
pub struct StrokeTileRecord {
    pub tile_index: usize,
    pub intermediate_tile_key: TileKey,
    pub backup_tile_key: Option<TileKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrushStrokeError {
    Atlas(AtlasError),
    Image(GlaImageTileAccessError),
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

impl From<GlaImageEnsureActiveTileError> for BrushStrokeError {
    fn from(error: GlaImageEnsureActiveTileError) -> Self {
        match error {
            GlaImageEnsureActiveTileError::Atlas(error) => Self::Atlas(error),
            GlaImageEnsureActiveTileError::TileAccess(error) => Self::Image(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StrokeCommitBatch {
    pub backup_group: CachedTileGroup,
    pub backup_tile_indices: Vec<usize>,
    pub backup_tile_keys: Vec<TileKey>,
    pub tile_records: Vec<StrokeTileRecord>,
    pub commands: Vec<RenderCommand>,
}

#[derive(Debug)]
pub struct BrushBackend {
    brush_id: BrushId,
    intermediate_backend: Backend,
    intermediate_backend_id: BackendId,
    stroke_history_groups: Vec<CachedTileGroup>,
}

impl BrushBackend {
    pub fn new(brush_id: BrushId, intermediate_backend: Backend) -> Result<Self, BrushStrokeError> {
        let intermediate_backend_id = intermediate_backend.backend_id()?;
        Ok(Self {
            brush_id,
            intermediate_backend,
            intermediate_backend_id,
            stroke_history_groups: Vec::new(),
        })
    }

    pub fn brush_id(&self) -> BrushId {
        self.brush_id
    }

    pub fn intermediate_backend(&self) -> &Backend {
        &self.intermediate_backend
    }

    pub fn intermediate_backend_id(&self) -> BackendId {
        self.intermediate_backend_id
    }

    pub fn stroke_history_groups(&self) -> &[CachedTileGroup] {
        &self.stroke_history_groups
    }

    pub fn begin_stroke(&self) -> BrushStrokeState {
        BrushStrokeState {
            brush_id: self.brush_id,
            intermediate_backend: self.intermediate_backend.clone(),
            intermediate_backend_id: self.intermediate_backend_id,
            intermediate: BrushIntermediate {
                tiles: SparseTileOwners::new(self.intermediate_backend_id),
            },
            touched_tiles: Vec::new(),
        }
    }

    pub fn archive_stroke(
        &mut self,
        stroke: BrushStrokeState,
    ) -> Result<CachedTileGroup, BrushStrokeError> {
        if stroke.brush_id != self.brush_id
            || stroke.intermediate_backend_id != self.intermediate_backend_id
        {
            return Err(AtlasError::WrongBackend.into());
        }

        let cached_group = self
            .intermediate_backend
            .cache_active_owners(stroke.into_intermediate().into_tile_owners())?;
        self.stroke_history_groups.push(cached_group.clone());
        Ok(cached_group)
    }
}

#[derive(Debug)]
pub struct BrushStrokeState {
    brush_id: BrushId,
    intermediate_backend: Backend,
    intermediate_backend_id: BackendId,
    intermediate: BrushIntermediate,
    touched_tiles: Vec<StrokeTileRecord>,
}

impl BrushStrokeState {
    pub fn new(brush_id: BrushId, intermediate_backend: Backend) -> Result<Self, BrushStrokeError> {
        let intermediate_backend_id = intermediate_backend.backend_id()?;
        Ok(Self {
            brush_id,
            intermediate_backend,
            intermediate_backend_id,
            intermediate: BrushIntermediate {
                tiles: SparseTileOwners::new(intermediate_backend_id),
            },
            touched_tiles: Vec::new(),
        })
    }

    pub fn brush_id(&self) -> BrushId {
        self.brush_id
    }

    pub fn intermediate_backend_id(&self) -> BackendId {
        self.intermediate_backend_id
    }

    pub fn intermediate(&self) -> &BrushIntermediate {
        &self.intermediate
    }

    pub fn into_intermediate(self) -> BrushIntermediate {
        self.intermediate
    }

    pub fn touched_tiles(&self) -> &[StrokeTileRecord] {
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
    ) -> Result<&mut StrokeTileRecord, BrushStrokeError> {
        if let Some(index) = self.touched_tile_index(tile_index) {
            return self
                .touched_tiles
                .get_mut(index)
                .ok_or(AtlasError::InvalidState.into());
        }

        let intermediate_tile_key = self
            .intermediate
            .tiles
            .ensure_tile(tile_index, &self.intermediate_backend)?;
        self.touched_tiles.push(StrokeTileRecord {
            tile_index,
            intermediate_tile_key,
            backup_tile_key: None,
        });
        self.touched_tiles
            .last_mut()
            .ok_or(AtlasError::InvalidState.into())
    }

    pub fn push_apply_dab(
        &mut self,
        tile_index: usize,
        source_tile_key: Option<TileKey>,
        brush_payload: Vec<u8>,
        output: &mut Vec<RenderCommand>,
    ) -> Result<TileKey, BrushStrokeError> {
        let destination_tile_key = self.ensure_touched_tile(tile_index)?.intermediate_tile_key;
        output.push(RenderCommand::ApplyDab(ApplyDabCommand {
            brush_id: self.brush_id,
            destination_tile_key,
            source_tile_key,
            brush_payload,
        }));
        Ok(destination_tile_key)
    }

    pub fn push_preview_merge(
        &self,
        tile_index: usize,
        origin_tile_key: TileKey,
        preview_tile_key: TileKey,
        brush_payload: Vec<u8>,
        output: &mut Vec<RenderCommand>,
    ) -> Option<TileKey> {
        let Some(record_index) = self.touched_tile_index(tile_index) else {
            return None;
        };
        let intermediate_tile_key = self.touched_tiles.get(record_index)?.intermediate_tile_key;
        output.push(RenderCommand::MergeTile(MergeTileCommand {
            brush_id: self.brush_id,
            origin_tile_key,
            intermediate_tile_key,
            destination_tile_key: preview_tile_key,
            brush_payload,
        }));
        Some(preview_tile_key)
    }

    pub fn build_commit_batch(
        &mut self,
        image: &mut GlaImage,
        image_backend: &Backend,
        backup_store: &mut DocumentBackupStore,
        tile_indices: &[usize],
        brush_payload: Vec<u8>,
    ) -> Result<StrokeCommitBatch, BrushStrokeError> {
        let image_backend_id = image_backend.backend_id()?;
        if image.backend() != image_backend_id {
            return Err(BrushStrokeError::WrongImageBackend {
                expected: image_backend_id,
                actual: image.backend(),
            });
        }

        let mut affected_tiles = tile_indices.to_vec();
        affected_tiles.sort_unstable();
        affected_tiles.dedup();

        let mut touched_tile_indexes = Vec::new();
        let mut backup_tile_indices = Vec::new();
        for &tile_index in &affected_tiles {
            let Some(record_index) = self.touched_tile_index(tile_index) else {
                continue;
            };
            touched_tile_indexes.push(record_index);
            let active_tile_key = image
                .tile_key(tile_index)
                .ok_or(GlaImageTileAccessError::OutOfBounds)?;
            if active_tile_key != TileKey::EMPTY {
                backup_tile_indices.push(tile_index);
            }
        }

        let backup_group = backup_store.retain_cached_group(backup_tile_indices.len())?;
        let backup_tile_keys = backup_group.keys().to_vec();
        let mut commands = Vec::new();
        let mut backup_key_cursor = 0usize;
        for &record_index in &touched_tile_indexes {
            let record = self
                .touched_tiles
                .get_mut(record_index)
                .ok_or(AtlasError::InvalidState)?;
            let tile_index = record.tile_index;
            let origin_tile_key = image
                .tile_key(tile_index)
                .ok_or(GlaImageTileAccessError::OutOfBounds)?;
            let destination_tile_key = image.ensure_active_tile_key(tile_index, image_backend)?;
            let backup_tile_key = if origin_tile_key != TileKey::EMPTY {
                let backup_tile_key = backup_tile_keys
                    .get(backup_key_cursor)
                    .copied()
                    .ok_or(AtlasError::InvalidState)?;
                commands.push(RenderCommand::CopyTile(CopyTileCommand {
                    source_tile_key: origin_tile_key,
                    destination_tile_key: backup_tile_key,
                }));
                backup_key_cursor += 1;
                record.backup_tile_key = Some(backup_tile_key);
                Some(backup_tile_key)
            } else {
                record.backup_tile_key = None;
                None
            };

            commands.push(RenderCommand::MergeTile(MergeTileCommand {
                brush_id: self.brush_id,
                origin_tile_key: backup_tile_key.unwrap_or(TileKey::EMPTY),
                intermediate_tile_key: record.intermediate_tile_key,
                destination_tile_key,
                brush_payload: brush_payload.clone(),
            }));
        }

        if backup_key_cursor != backup_tile_keys.len() {
            return Err(AtlasError::InvalidState.into());
        }

        Ok(StrokeCommitBatch {
            backup_group,
            backup_tile_indices,
            backup_tile_keys,
            tile_records: touched_tile_indexes
                .into_iter()
                .map(|record_index| self.touched_tiles[record_index])
                .collect(),
            commands,
        })
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

    pub fn register(
        &mut self,
        shader_registration: BrushShaderRegistration,
        intermediate_backend: Backend,
        input_processor: Box<dyn BrushInputProcessor>,
    ) -> Result<(), BrushStrokeError> {
        let registration = BrushRegistration {
            brush_id: shader_registration.brush_id,
            shader_spec: shader_registration.shader_spec,
            backend: BrushBackend::new(shader_registration.brush_id, intermediate_backend)?,
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
        tile_canvas_origin: CanvasVec2,
    ) -> Result<Vec<u8>, BrushInputError> {
        let processor =
            self.input_processor(input.brush_id)
                .ok_or(BrushInputError::WrongBrush {
                    expected: input.brush_id,
                    actual: input.brush_id,
                })?;
        processor.encode_apply_dab_payload(input, block_index, tile_canvas_origin)
    }

    pub fn encode_merge_payload(&self, input: &BrushInput) -> Result<Vec<u8>, BrushInputError> {
        let processor =
            self.input_processor(input.brush_id)
                .ok_or(BrushInputError::WrongBrush {
                    expected: input.brush_id,
                    actual: input.brush_id,
                })?;
        processor.encode_merge_payload(input)
    }
}

#[cfg(test)]
mod tests {
    use atlas::{AtlasLayout, BackendId, TileState};
    use glaphica_core::CanvasVec2;
    use glaphica_core::IMAGE_TILE_SIZE;
    use renderer::{ApplyDabCommand, CopyTileCommand, MergeTileCommand, RenderCommand};

    use crate::round::{
        ROUND_SHADER_SPEC, RoundBrushInputProcessor, encode_round_apply_payload,
        encode_round_merge_payload,
    };
    use crate::{
        BrushBackend, BrushId, BrushRegistry, BrushShaderRegistration, BrushStrokeState,
        CanvasInput,
    };
    use atlas::{Backend, TileKey};
    use gla_document::DocumentBackupStore;
    use gla_image::{GlaImage, GlaImageLayout};

    #[test]
    fn apply_dab_allocates_intermediate_tile_once() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(7));
        let mut state =
            BrushStrokeState::new(BrushId::new(5), backend.clone()).expect("state should build");
        let mut commands = Vec::new();

        let first_payload = encode_round_apply_payload([8.0, 9.0], 10.0, 0.5, 0.75);
        let second_payload = encode_round_apply_payload([3.0, 4.0], 5.0, 0.6, 0.25);
        let first = state
            .push_apply_dab(4, None, first_payload.clone(), &mut commands)
            .expect("dab should build");
        let second = state
            .push_apply_dab(4, Some(first), second_payload.clone(), &mut commands)
            .expect("dab should build");

        assert_eq!(first, second);
        assert_eq!(state.intermediate().tile_key(4), Some(first));
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
        let active_tile = backend.alloc_active().expect("active tile");
        let active_tile_key = active_tile.tile_key();

        let mut state =
            BrushStrokeState::new(BrushId::new(9), backend).expect("state should build");
        let mut commands = Vec::new();
        let intermediate_tile_key = state
            .push_apply_dab(
                0,
                None,
                encode_round_apply_payload([1.0, 2.0], 3.0, 0.5, 1.0),
                &mut commands,
            )
            .expect("dab");
        commands.clear();
        let preview_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(13));
        let preview_tile_key = preview_backend
            .alloc_active()
            .expect("preview tile")
            .tile_key();
        let merge_payload = encode_round_merge_payload([0.2, 0.3, 0.4]);

        let returned_tile_key = state
            .push_preview_merge(
                0,
                active_tile_key,
                preview_tile_key,
                merge_payload.clone(),
                &mut commands,
            )
            .expect("preview merge should allocate");

        assert_eq!(returned_tile_key, preview_tile_key);
        assert_eq!(
            commands,
            vec![RenderCommand::MergeTile(MergeTileCommand {
                brush_id: BrushId::new(9),
                origin_tile_key: active_tile_key,
                intermediate_tile_key,
                destination_tile_key: preview_tile_key,
                brush_payload: merge_payload,
            })]
        );
    }

    #[test]
    fn commit_batch_copies_non_empty_active_tiles_before_merge() {
        let brush_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(7));
        let image_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(3));
        let backup_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(5));
        let mut image = GlaImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE),
            BackendId::new(3),
        )
        .expect("image should create");
        let first_active = image_backend.alloc_active().expect("first active");
        let first_active_key = first_active.tile_key();
        image
            .replace_tile_owner(0, first_active)
            .expect("install tile");

        let mut state =
            BrushStrokeState::new(BrushId::new(11), brush_backend).expect("state should build");
        let mut backup_store =
            DocumentBackupStore::new(backup_backend).expect("backup store should build");
        let mut draw_commands = Vec::new();
        let first_intermediate = state
            .push_apply_dab(
                0,
                None,
                encode_round_apply_payload([4.0, 4.0], 6.0, 0.5, 1.0),
                &mut draw_commands,
            )
            .expect("dab");
        let second_intermediate = state
            .push_apply_dab(
                1,
                None,
                encode_round_apply_payload([2.0, 2.0], 5.0, 0.3, 0.9),
                &mut draw_commands,
            )
            .expect("dab");
        let merge_payload = encode_round_merge_payload([0.1, 0.2, 0.3]);

        let batch = state
            .build_commit_batch(
                &mut image,
                &image_backend,
                &mut backup_store,
                &[1, 0],
                merge_payload.clone(),
            )
            .expect("commit batch should build");

        let second_active_key = image.tile_key(1).expect("tile key should exist");
        assert_ne!(second_active_key, TileKey::EMPTY);
        assert_eq!(batch.backup_tile_indices, vec![0]);
        assert_eq!(batch.backup_tile_keys.len(), 1);
        assert_eq!(
            batch.commands,
            vec![
                RenderCommand::CopyTile(CopyTileCommand {
                    source_tile_key: first_active_key,
                    destination_tile_key: batch.backup_tile_keys[0],
                }),
                RenderCommand::MergeTile(MergeTileCommand {
                    brush_id: BrushId::new(11),
                    origin_tile_key: batch.backup_tile_keys[0],
                    intermediate_tile_key: first_intermediate,
                    destination_tile_key: first_active_key,
                    brush_payload: merge_payload.clone(),
                }),
                RenderCommand::MergeTile(MergeTileCommand {
                    brush_id: BrushId::new(11),
                    origin_tile_key: TileKey::EMPTY,
                    intermediate_tile_key: second_intermediate,
                    destination_tile_key: second_active_key,
                    brush_payload: merge_payload.clone(),
                }),
            ]
        );
        assert_eq!(
            state.touched_tiles()[0].backup_tile_key,
            Some(batch.backup_tile_keys[0])
        );
        assert_eq!(state.touched_tiles()[1].backup_tile_key, None);
    }

    #[test]
    fn preview_merge_uses_explicit_origin_tile_key() {
        let brush_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(7));
        let origin_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(3));
        let active_tile = origin_backend.alloc_active().expect("active tile");
        let active_tile_key = active_tile.tile_key();
        let mut state =
            BrushStrokeState::new(BrushId::new(11), brush_backend).expect("state should build");
        let mut commands = Vec::new();
        let intermediate_tile_key = state
            .push_apply_dab(
                0,
                None,
                encode_round_apply_payload([4.0, 4.0], 8.0, 1.0, 0.4),
                &mut commands,
            )
            .expect("dab");
        commands.clear();
        let preview_backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(13));
        let preview_tile_key = preview_backend
            .alloc_active()
            .expect("preview tile")
            .tile_key();
        let merge_payload = encode_round_merge_payload([0.9, 0.8, 0.7]);

        let returned_tile_key = state
            .push_preview_merge(
                0,
                active_tile_key,
                preview_tile_key,
                merge_payload.clone(),
                &mut commands,
            )
            .expect("preview merge should allocate");
        assert_eq!(returned_tile_key, preview_tile_key);

        assert_eq!(
            commands,
            vec![RenderCommand::MergeTile(MergeTileCommand {
                brush_id: BrushId::new(11),
                origin_tile_key: active_tile_key,
                intermediate_tile_key,
                destination_tile_key: preview_tile_key,
                brush_payload: merge_payload,
            })]
        );
    }

    #[test]
    fn atlas_backend_retires_stroke_intermediate_as_cached_group() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(7));
        let mut brush_backend =
            BrushBackend::new(BrushId::new(17), backend.clone()).expect("backend should build");
        let mut state = brush_backend.begin_stroke();
        let mut commands = Vec::new();
        let active_tile_key = state
            .push_apply_dab(
                0,
                None,
                encode_round_apply_payload([4.0, 4.0], 8.0, 0.8, 0.7),
                &mut commands,
            )
            .expect("dab");

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
        let next_tile_key = next_state
            .push_apply_dab(
                0,
                None,
                encode_round_apply_payload([1.0, 1.0], 2.0, 0.2, 0.5),
                &mut Vec::new(),
            )
            .expect("next dab");
        assert_ne!(next_tile_key, active_tile_key);
        assert_eq!(backend.tile_state(next_tile_key), Ok(TileState::Active));
    }

    #[test]
    fn registry_registers_shader_and_backend_together() {
        let brush_id = BrushId::new(23);
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(9));
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
        let tile_key = stroke
            .push_apply_dab(
                0,
                None,
                encode_round_apply_payload([2.0, 3.0], 4.0, 0.4, 0.6),
                &mut Vec::new(),
            )
            .expect("dab");
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
