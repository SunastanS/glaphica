use brush::{BrushId, BrushInputError, BrushRegistry, BrushStrokeInputProcessor, round::RoundBrushSettings};
use std::error::Error;
use std::fmt::{Display, Formatter};

use crate::input::BrushThreadBrushInputProducer;
use crate::CanvasInput;

pub struct BrushWorker {
    brushes: BrushRegistry,
    active_brush_id: BrushId,
    active_input_stroke: Box<dyn BrushStrokeInputProcessor>,
}

#[derive(Debug)]
pub enum BrushWorkerError {
    BrushInput(BrushInputError),
    BrushInit(BrushInputError),
    BrushNotRegistered(BrushId),
}

impl Display for BrushWorkerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BrushInput(error) => Display::fmt(error, f),
            Self::BrushInit(error) => Display::fmt(error, f),
            Self::BrushNotRegistered(brush_id) => {
                write!(f, "brush {} is not registered", brush_id.raw())
            }
        }
    }
}

impl Error for BrushWorkerError {}

impl From<BrushInputError> for BrushWorkerError {
    fn from(error: BrushInputError) -> Self {
        Self::BrushInput(error)
    }
}

impl BrushWorker {
    pub fn new(
        brushes: BrushRegistry,
        active_brush_id: BrushId,
        _batch_capacity: usize,
    ) -> Result<Self, BrushWorkerError> {
        let active_input_stroke =
            begin_input_stroke(&brushes, active_brush_id).map_err(map_begin_input_stroke_error)?;
        Ok(Self {
            brushes,
            active_brush_id,
            active_input_stroke,
        })
    }

    pub fn brushes(&self) -> &BrushRegistry {
        &self.brushes
    }

    pub fn brushes_mut(&mut self) -> &mut BrushRegistry {
        &mut self.brushes
    }

    pub fn active_brush_id(&self) -> BrushId {
        self.active_brush_id
    }

    pub fn set_active_brush(&mut self, brush_id: BrushId) -> Result<(), BrushWorkerError> {
        if self.active_brush_id == brush_id {
            return Ok(());
        }
        self.active_input_stroke =
            begin_input_stroke(&self.brushes, brush_id).map_err(map_begin_input_stroke_error)?;
        self.active_brush_id = brush_id;
        Ok(())
    }

    pub fn reset_active_stroke(&mut self) {
        self.active_input_stroke.reset();
    }

    pub fn update_round_brush_settings(
        &mut self,
        settings: RoundBrushSettings,
    ) {
        self.brushes.update_round_brush_settings(settings);
        self.active_input_stroke = begin_input_stroke(&self.brushes, self.active_brush_id)
            .map_err(map_begin_input_stroke_error)
            .expect("existing brush should be re-creatable");
    }

    pub fn process_canvas_inputs(
        &mut self,
        canvas_inputs: &[CanvasInput],
        brush_input_producer: &BrushThreadBrushInputProducer,
    ) -> Result<usize, BrushWorkerError> {
        if canvas_inputs.is_empty() {
            return Ok(0);
        }

        self.active_input_stroke.push_canvas_inputs(canvas_inputs)?;
        let Some(brush_input) = self.active_input_stroke.drain_brush_input()? else {
            return Ok(0);
        };
        let produced_blocks = brush_input.blocks.blocks().len();

        brush_input_producer.push(brush_input);
        Ok(produced_blocks)
    }

    pub fn finish_active_stroke(
        &mut self,
        brush_input_producer: &BrushThreadBrushInputProducer,
    ) -> Result<usize, BrushWorkerError> {
        self.active_input_stroke.finish_stroke()?;
        let Some(brush_input) = self.active_input_stroke.drain_brush_input()? else {
            return Ok(0);
        };
        let produced_blocks = brush_input.blocks.blocks().len();
        if produced_blocks == 0 {
            return Ok(0);
        }

        brush_input_producer.push(brush_input);
        Ok(produced_blocks)
    }
}

fn begin_input_stroke(
    brushes: &BrushRegistry,
    brush_id: BrushId,
) -> Result<Box<dyn BrushStrokeInputProcessor>, BrushInputError> {
    brushes.begin_input_stroke(brush_id)
}

fn map_begin_input_stroke_error(error: BrushInputError) -> BrushWorkerError {
    match error {
        BrushInputError::WrongBrush { expected, actual } if expected == actual => {
            BrushWorkerError::BrushNotRegistered(expected)
        }
        error => BrushWorkerError::BrushInit(error),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use atlas::{AtlasLayout, Backend, BackendId};
    use brush::round::ROUND_BRUSH_ID;
    use brush::{BrushId, BrushInputError, BrushStrokeError};
    use glaphica_core::{CanvasVec2, RadianVec2};

    use crate::{BrushWorker, create_brush_input_channels};

    #[test]
    fn worker_process_and_finish_emit_distinct_brush_input_batches() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(41));
        let brushes = brush::BrushRegistry::with_builtin_round(backend);
        let (brush_producer, brush_consumer) = create_brush_input_channels(8);
        let mut worker = BrushWorker::new(brushes, ROUND_BRUSH_ID, 16).expect("worker");
        let mut processed_brush_inputs = Vec::new();
        let mut finished_brush_inputs = Vec::new();

        let canvas_inputs = [
            crate::CanvasInput {
                time_ns: 1,
                position: CanvasVec2::new(10.0, 20.0),
                pressure: 0.25,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            },
            crate::CanvasInput {
                time_ns: 2,
                position: CanvasVec2::new(30.0, 40.0),
                pressure: 0.75,
                tilt: RadianVec2::new(0.1, 0.2),
                twist: 0.3,
            },
            crate::CanvasInput {
                time_ns: 3,
                position: CanvasVec2::new(60.0, 80.0),
                pressure: 0.75,
                tilt: RadianVec2::new(0.1, 0.2),
                twist: 0.3,
            },
        ];
        let produced = worker
            .process_canvas_inputs(&canvas_inputs, &brush_producer)
            .expect("process canvas input");
        brush_consumer.drain_batch_with_wait(&mut processed_brush_inputs, 1, Duration::ZERO);

        let finished = worker
            .finish_active_stroke(&brush_producer)
            .expect("finish stroke");
        brush_consumer.drain_batch_with_wait(&mut finished_brush_inputs, 1, Duration::ZERO);

        assert_eq!(produced, 1);
        assert_eq!(processed_brush_inputs.len(), 1);
        assert_eq!(processed_brush_inputs[0].brush_id, ROUND_BRUSH_ID);
        assert_eq!(processed_brush_inputs[0].blocks.blocks().len(), 1);
        assert_eq!(
            processed_brush_inputs[0].blocks.blocks()[0].values()[0],
            10.0
        );
        assert_eq!(
            processed_brush_inputs[0].blocks.blocks()[0].values()[1],
            20.0
        );

        assert_eq!(finished_brush_inputs.len(), 1);
        assert_eq!(finished_brush_inputs[0].brush_id, ROUND_BRUSH_ID);
        assert_eq!(finished, finished_brush_inputs[0].blocks.blocks().len());
        assert!(finished > 0);

        let processed_origin = (
            processed_brush_inputs[0].blocks.blocks()[0].values()[0],
            processed_brush_inputs[0].blocks.blocks()[0].values()[1],
        );
        let finished_positions = finished_brush_inputs[0]
            .blocks
            .blocks()
            .iter()
            .map(|block| (block.values()[0], block.values()[1]))
            .collect::<Vec<_>>();

        assert!(!finished_positions.contains(&processed_origin));
        assert!(
            finished_positions
                .windows(2)
                .all(|pair| pair[0].0 < pair[1].0)
        );
        assert!(
            finished_positions
                .windows(2)
                .all(|pair| pair[0].1 < pair[1].1)
        );
    }

    #[test]
    fn worker_rejects_unknown_active_brush() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(42));
        let brushes = brush::BrushRegistry::with_builtin_round(backend);

        let error = match BrushWorker::new(brushes, BrushId::new(999), 4) {
            Ok(_) => panic!("expected unknown brush to be rejected"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            crate::BrushWorkerError::BrushNotRegistered(brush_id) if brush_id == BrushId::new(999)
        ));
    }

    #[test]
    fn begin_input_stroke_preserves_non_registration_failures() {
        let error = super::map_begin_input_stroke_error(BrushInputError::Stroke(
            BrushStrokeError::WrongImageBackend {
                expected: BackendId::new(7),
                actual: BackendId::new(8),
            },
        ));

        assert!(matches!(
            error,
            crate::BrushWorkerError::BrushInit(BrushInputError::Stroke(
                BrushStrokeError::WrongImageBackend { expected, actual }
            )) if expected == BackendId::new(7) && actual == BackendId::new(8)
        ));
    }
}
