use std::error::Error;
use std::fmt::{Display, Formatter};
use std::time::Duration;

use brush::{BrushId, BrushInputError};

use crate::input::{BrushThreadBrushInputProducer, BrushThreadCanvasInputConsumer};
use crate::{AppBrushRegistry, CanvasInput};

pub struct BrushWorker {
    brushes: AppBrushRegistry,
    active_brush_id: BrushId,
    canvas_batch: Vec<CanvasInput>,
}

#[derive(Debug)]
pub enum BrushWorkerError {
    BrushInput(BrushInputError),
    BrushNotRegistered(BrushId),
}

impl Display for BrushWorkerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BrushInput(error) => Display::fmt(error, f),
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
        brushes: AppBrushRegistry,
        active_brush_id: BrushId,
        batch_capacity: usize,
    ) -> Result<Self, BrushWorkerError> {
        if brushes.shader_spec(active_brush_id).is_none() {
            return Err(BrushWorkerError::BrushNotRegistered(active_brush_id));
        }
        Ok(Self {
            brushes,
            active_brush_id,
            canvas_batch: Vec::with_capacity(batch_capacity),
        })
    }

    pub fn brushes(&self) -> &AppBrushRegistry {
        &self.brushes
    }

    pub fn brushes_mut(&mut self) -> &mut AppBrushRegistry {
        &mut self.brushes
    }

    pub fn active_brush_id(&self) -> BrushId {
        self.active_brush_id
    }

    pub fn set_active_brush(&mut self, brush_id: BrushId) -> Result<(), BrushWorkerError> {
        if self.brushes.shader_spec(brush_id).is_none() {
            return Err(BrushWorkerError::BrushNotRegistered(brush_id));
        }
        self.active_brush_id = brush_id;
        Ok(())
    }

    pub fn process_canvas_input(
        &mut self,
        canvas_input_consumer: &BrushThreadCanvasInputConsumer,
        brush_input_producer: &BrushThreadBrushInputProducer,
        max_batch_size: usize,
        wait_timeout: Duration,
    ) -> Result<usize, BrushWorkerError> {
        self.canvas_batch.clear();
        canvas_input_consumer.drain_batch_with_wait(
            &mut self.canvas_batch,
            max_batch_size,
            wait_timeout,
        );
        if self.canvas_batch.is_empty() {
            return Ok(0);
        }

        let brush_input = self
            .brushes
            .produce_input(self.active_brush_id, &self.canvas_batch)?;
        let produced_blocks = brush_input.blocks.blocks().len();
        if produced_blocks == 0 {
            return Ok(0);
        }

        brush_input_producer.push(brush_input);
        Ok(produced_blocks)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use atlas::{AtlasLayout, Backend, BackendId};
    use brush::BrushId;
    use brush::round::ROUND_BRUSH_ID;
    use glaphica_core::{CanvasVec2, RadianVec2};

    use crate::{BrushWorker, create_brush_input_channels};

    #[test]
    fn worker_turns_canvas_batch_into_brush_input_batch() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(41));
        let brushes = crate::AppBrushRegistry::with_builtin_round(backend);
        let (canvas_producer, canvas_consumer, brush_producer, brush_consumer) =
            create_brush_input_channels(8, 8);
        let mut worker = BrushWorker::new(brushes, ROUND_BRUSH_ID, 16).expect("worker");
        let mut brush_inputs = Vec::new();

        canvas_producer.push(crate::CanvasInput {
            time_ns: 1,
            position: CanvasVec2::new(10.0, 20.0),
            pressure: 0.25,
            tilt: RadianVec2::new(0.0, 0.0),
            twist: 0.0,
        });
        canvas_producer.push(crate::CanvasInput {
            time_ns: 2,
            position: CanvasVec2::new(30.0, 40.0),
            pressure: 0.75,
            tilt: RadianVec2::new(0.1, 0.2),
            twist: 0.3,
        });

        let produced = worker
            .process_canvas_input(&canvas_consumer, &brush_producer, 16, Duration::ZERO)
            .expect("process canvas input");
        brush_consumer.drain_batch_with_wait(&mut brush_inputs, 1, Duration::ZERO);

        assert_eq!(produced, 2);
        assert_eq!(brush_inputs.len(), 1);
        assert_eq!(brush_inputs[0].brush_id, ROUND_BRUSH_ID);
        assert_eq!(brush_inputs[0].blocks.blocks().len(), 2);
        assert_eq!(brush_inputs[0].blocks.blocks()[0].values()[0], 10.0);
        assert_eq!(brush_inputs[0].blocks.blocks()[1].values()[1], 40.0);
    }

    #[test]
    fn worker_rejects_unknown_active_brush() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(42));
        let brushes = crate::AppBrushRegistry::with_builtin_round(backend);

        let error = match BrushWorker::new(brushes, BrushId::new(999), 4) {
            Ok(_) => panic!("expected unknown brush to be rejected"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            crate::BrushWorkerError::BrushNotRegistered(brush_id) if brush_id == BrushId::new(999)
        ));
    }
}
