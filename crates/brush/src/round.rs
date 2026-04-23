use crate::{
    BrushId, BrushInput, BrushInputBlockList, BrushInputError, BrushInputProcessor,
    BrushShaderRegistration, BrushStrokeInputProcessor, BrushStrokeSampler, CommittedCanvasSample,
    CommittedCanvasSpanBuffer, DistanceOrTimeStrokeSmoother, EquidistantStrokeSampler,
    SmoothedBrushStrokeInputProcessor, StrokeSampler, StrokeSmoother,
};
use bytemuck::{Pod, Zeroable};
use glaphica_core::CanvasVec2;
use renderer::{BrushShaderSource, BrushShaderSpec};

pub const ROUND_BRUSH_ID: BrushId = BrushId::new(1);

pub const ROUND_APPLY_DAB_WGSL: &str = include_str!("round_apply_dab.wgsl");
pub const ROUND_MERGE_TILE_WGSL: &str = include_str!("round_merge_tile.wgsl");

pub const ROUND_SHADER_SPEC: BrushShaderSpec = BrushShaderSpec {
    apply_dab: BrushShaderSource {
        wgsl: ROUND_APPLY_DAB_WGSL,
        entry_point: "fs_apply_dab",
    },
    merge_tile: BrushShaderSource {
        wgsl: ROUND_MERGE_TILE_WGSL,
        entry_point: "fs_merge_tile",
    },
};

pub const ROUND_SHADER_REGISTRATION: BrushShaderRegistration = BrushShaderRegistration {
    brush_id: ROUND_BRUSH_ID,
    shader_spec: ROUND_SHADER_SPEC,
};

pub const ROUND_INPUT_BLOCK_LEN: usize = 9;

#[derive(Debug, Clone, PartialEq)]
pub struct RoundBrushInputProcessor {
    base_radius_px: f32,
    spacing_ratio: f32,
    base_hardness: f32,
    base_flow: f32,
    base_opacity: f32,
    tint: [f32; 3],
    smoother_factory: fn() -> Box<dyn StrokeSmoother>,
}

struct RoundBrushStrokeSampler {
    sampler: EquidistantStrokeSampler,
    base_radius_px: f32,
    spacing_ratio: f32,
    base_hardness: f32,
    base_flow: f32,
    base_opacity: f32,
    tint: [f32; 3],
    last_emitted_position: Option<CanvasVec2>,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
struct RoundApplyPayload {
    center_local_x: f32,
    center_local_y: f32,
    radius_px: f32,
    hardness: f32,
    flow: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
struct RoundMergePayload {
    tint: [f32; 3],
    opacity: f32,
}

pub fn encode_round_apply_payload(
    center_local: [f32; 2],
    radius_px: f32,
    hardness: f32,
    flow: f32,
) -> Vec<u8> {
    bytemuck::bytes_of(&RoundApplyPayload {
        center_local_x: center_local[0],
        center_local_y: center_local[1],
        radius_px,
        hardness,
        flow,
    })
    .to_vec()
}

pub fn encode_round_merge_payload(tint: [f32; 3], opacity: f32) -> Vec<u8> {
    bytemuck::bytes_of(&RoundMergePayload { tint, opacity }).to_vec()
}

impl Default for RoundBrushInputProcessor {
    fn default() -> Self {
        Self {
            base_radius_px: 5.0,
            spacing_ratio: 1.0,
            base_hardness: 0.7,
            base_flow: 1.0,
            base_opacity: 1.0,
            tint: [0.0, 0.0, 1.0],
            smoother_factory: default_smoother_factory,
        }
    }
}

fn default_smoother_factory() -> Box<dyn StrokeSmoother> {
    Box::new(DistanceOrTimeStrokeSmoother::default())
}

impl RoundBrushInputProcessor {
    fn dab_spacing_px(&self) -> f32 {
        (self.base_radius_px * self.spacing_ratio).max(f32::EPSILON)
    }

    pub fn with_smoother_factory(
        mut self,
        smoother_factory: fn() -> Box<dyn StrokeSmoother>,
    ) -> Self {
        self.smoother_factory = smoother_factory;
        self
    }
}

impl BrushInputProcessor for RoundBrushInputProcessor {
    fn begin_stroke(&self) -> Box<dyn BrushStrokeInputProcessor> {
        Box::new(SmoothedBrushStrokeInputProcessor::new(
            (self.smoother_factory)(),
            Box::new(RoundBrushStrokeSampler {
                sampler: EquidistantStrokeSampler::new(self.dab_spacing_px()),
                base_radius_px: self.base_radius_px,
                spacing_ratio: self.spacing_ratio,
                base_hardness: self.base_hardness,
                base_flow: self.base_flow,
                base_opacity: self.base_opacity,
                tint: self.tint,
                last_emitted_position: None,
            }),
        ))
    }

    fn max_affected_radius_px(&self) -> u32 {
        self.base_radius_px.ceil().max(1.0) as u32
    }

    fn block_center(
        &self,
        input: &BrushInput,
        block_index: usize,
    ) -> Result<CanvasVec2, BrushInputError> {
        if input.brush_id != ROUND_BRUSH_ID {
            return Err(BrushInputError::WrongBrush {
                expected: ROUND_BRUSH_ID,
                actual: input.brush_id,
            });
        }
        let values = input
            .blocks
            .blocks()
            .get(block_index)
            .ok_or(BrushInputError::InvalidBlockLength {
                brush_id: ROUND_BRUSH_ID,
                expected: block_index + 1,
                actual: input.blocks.blocks().len(),
            })?
            .values();
        if values.len() != ROUND_INPUT_BLOCK_LEN {
            return Err(BrushInputError::InvalidBlockLength {
                brush_id: ROUND_BRUSH_ID,
                expected: ROUND_INPUT_BLOCK_LEN,
                actual: values.len(),
            });
        }
        Ok(CanvasVec2::new(values[0], values[1]))
    }

    fn encode_apply_dab_payload(
        &self,
        input: &BrushInput,
        block_index: usize,
        tile_canvas_origin: CanvasVec2,
    ) -> Result<Vec<u8>, BrushInputError> {
        if input.brush_id != ROUND_BRUSH_ID {
            return Err(BrushInputError::WrongBrush {
                expected: ROUND_BRUSH_ID,
                actual: input.brush_id,
            });
        }
        let values = input
            .blocks
            .blocks()
            .get(block_index)
            .ok_or(BrushInputError::InvalidBlockLength {
                brush_id: ROUND_BRUSH_ID,
                expected: block_index + 1,
                actual: input.blocks.blocks().len(),
            })?
            .values();
        if values.len() != ROUND_INPUT_BLOCK_LEN {
            return Err(BrushInputError::InvalidBlockLength {
                brush_id: ROUND_BRUSH_ID,
                expected: ROUND_INPUT_BLOCK_LEN,
                actual: values.len(),
            });
        }
        for (value_index, value) in values.iter().copied().enumerate() {
            if !value.is_finite() {
                return Err(BrushInputError::InvalidBlockValue {
                    brush_id: ROUND_BRUSH_ID,
                    block_index,
                    value_index,
                });
            }
        }
        let center = CanvasVec2::new(values[0], values[1]);
        let local_center = [
            center.x - tile_canvas_origin.x,
            center.y - tile_canvas_origin.y,
        ];
        Ok(encode_round_apply_payload(
            local_center,
            values[2].max(0.0),
            values[3],
            values[4],
        ))
    }

    fn encode_merge_payload(&self, input: &BrushInput) -> Result<Vec<u8>, BrushInputError> {
        if input.brush_id != ROUND_BRUSH_ID {
            return Err(BrushInputError::WrongBrush {
                expected: ROUND_BRUSH_ID,
                actual: input.brush_id,
            });
        }
        let last = input
            .blocks
            .blocks()
            .last()
            .ok_or(BrushInputError::InvalidBlockLength {
                brush_id: ROUND_BRUSH_ID,
                expected: 1,
                actual: 0,
            })?
            .values();
        if last.len() != ROUND_INPUT_BLOCK_LEN {
            return Err(BrushInputError::InvalidBlockLength {
                brush_id: ROUND_BRUSH_ID,
                expected: ROUND_INPUT_BLOCK_LEN,
                actual: last.len(),
            });
        }
        Ok(encode_round_merge_payload(
            [last[6], last[7], last[8]],
            last[5].clamp(0.0, 1.0),
        ))
    }
}

impl BrushStrokeSampler for RoundBrushStrokeSampler {
    fn reset(&mut self) {
        self.sampler.reset();
        self.last_emitted_position = None;
    }

    fn sample_brush_input(
        &mut self,
        spans: &CommittedCanvasSpanBuffer,
    ) -> Result<Option<BrushInput>, BrushInputError> {
        self.sampler.set_spacing(self.dab_spacing_px());
        let mut samples = Vec::new();
        self.sampler.sample_committed_spans(spans, &mut samples);
        let mut blocks = BrushInputBlockList::new(ROUND_BRUSH_ID);
        for (block_index, sample) in samples.iter().copied().enumerate() {
            if self
                .last_emitted_position
                .is_some_and(|position| same_canvas_position(position, sample.position))
            {
                continue;
            }
            push_round_block(
                &mut blocks,
                block_index,
                sample,
                self.base_radius_px,
                self.base_hardness,
                self.base_flow,
                self.base_opacity,
                self.tint,
            )?;
            self.last_emitted_position = Some(sample.position);
        }
        if blocks.blocks().is_empty() {
            return Ok(None);
        }

        Ok(Some(BrushInput {
            brush_id: ROUND_BRUSH_ID,
            blocks,
        }))
    }
}

impl RoundBrushStrokeSampler {
    fn dab_spacing_px(&self) -> f32 {
        (self.base_radius_px * self.spacing_ratio).max(f32::EPSILON)
    }
}

fn same_canvas_position(lhs: CanvasVec2, rhs: CanvasVec2) -> bool {
    const EPSILON: f32 = 1e-5;
    (lhs.x - rhs.x).abs() <= EPSILON && (lhs.y - rhs.y).abs() <= EPSILON
}

fn push_round_block(
    blocks: &mut BrushInputBlockList,
    block_index: usize,
    sample: CommittedCanvasSample,
    base_radius_px: f32,
    base_hardness: f32,
    base_flow: f32,
    base_opacity: f32,
    tint: [f32; 3],
) -> Result<(), BrushInputError> {
    if !sample.position.x.is_finite() {
        return Err(BrushInputError::InvalidBlockValue {
            brush_id: ROUND_BRUSH_ID,
            block_index,
            value_index: 0,
        });
    }
    if !sample.position.y.is_finite() {
        return Err(BrushInputError::InvalidBlockValue {
            brush_id: ROUND_BRUSH_ID,
            block_index,
            value_index: 1,
        });
    }
    if !sample.pressure.is_finite() {
        return Err(BrushInputError::InvalidBlockValue {
            brush_id: ROUND_BRUSH_ID,
            block_index,
            value_index: 2,
        });
    }

    let pressure = sample.pressure.clamp(0.0, 1.0);
    let dab_flow = base_flow * pressure;
    blocks.push_block(vec![
        sample.position.x,
        sample.position.y,
        base_radius_px,
        base_hardness,
        dab_flow,
        base_opacity,
        tint[0],
        tint[1],
        tint[2],
    ]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        BrushInput, BrushInputBlockList, BrushInputProcessor, CanvasInput,
        PassthroughStrokeSmoother, StrokeSmoother,
        round::{ROUND_BRUSH_ID, RoundBrushInputProcessor, encode_round_merge_payload},
    };
    use glaphica_core::CanvasVec2;

    fn passthrough_smoother_factory() -> Box<dyn StrokeSmoother> {
        Box::new(PassthroughStrokeSmoother::default())
    }

    #[test]
    fn round_processor_encodes_payloads_from_blocks() {
        let mut input = BrushInputBlockList::new(ROUND_BRUSH_ID);
        input.push_block(vec![10.0, 8.0, 6.0, 0.4, 0.7, 0.8, 0.2, 0.3, 0.4]);
        let input = BrushInput {
            brush_id: ROUND_BRUSH_ID,
            blocks: input,
        };

        let result = RoundBrushInputProcessor::default()
            .encode_apply_dab_payload(&input, 0, CanvasVec2::new(0.0, 0.0))
            .expect("processing should succeed");

        assert!(!result.is_empty());
        assert_eq!(
            RoundBrushInputProcessor::default()
                .encode_merge_payload(&input)
                .expect("merge payload"),
            encode_round_merge_payload([0.2, 0.3, 0.4], 0.8)
        );
    }

    #[test]
    fn round_processor_produces_blocks_from_canvas_input() {
        let processor = RoundBrushInputProcessor::default();
        let mut stroke = processor.begin_stroke();
        let input = [
            CanvasInput {
                time_ns: 1,
                position: glaphica_core::CanvasVec2::new(11.0, 13.0),
                pressure: 0.5,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            },
            CanvasInput {
                time_ns: 2,
                position: glaphica_core::CanvasVec2::new(17.0, 13.0),
                pressure: 0.5,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            },
        ];

        stroke
            .push_canvas_inputs(&input)
            .expect("push should succeed");
        let result = stroke
            .drain_brush_input()
            .expect("drain should succeed")
            .expect("brush input should exist");

        assert_eq!(result.brush_id, ROUND_BRUSH_ID);
        assert_eq!(result.blocks.blocks().len(), 1);
        assert_eq!(result.blocks.blocks()[0].values()[0], 11.0);
        assert_eq!(result.blocks.blocks()[0].values()[1], 13.0);
        assert_eq!(result.blocks.blocks()[0].values()[2], 5.0);
        assert_eq!(result.blocks.blocks()[0].values()[3], 0.7);
        assert_eq!(result.blocks.blocks()[0].values()[4], 0.5);
        assert_eq!(result.blocks.blocks()[0].values()[5], 1.0);
    }

    #[test]
    fn round_processor_uses_uniform_sampling_for_first_point_with_passthrough_smoother() {
        let processor =
            RoundBrushInputProcessor::default().with_smoother_factory(passthrough_smoother_factory);
        let mut stroke = processor.begin_stroke();

        stroke
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 1,
                    position: glaphica_core::CanvasVec2::new(0.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 2,
                    position: glaphica_core::CanvasVec2::new(12.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("push should succeed");

        let result = stroke
            .drain_brush_input()
            .expect("drain should succeed")
            .expect("brush input should exist");
        let positions = result
            .blocks
            .blocks()
            .iter()
            .map(|block| (block.values()[0], block.values()[1]))
            .collect::<Vec<_>>();

        assert_eq!(positions, vec![(0.0, 0.0), (5.0, 0.0), (10.0, 0.0)]);
    }

    #[test]
    fn round_processor_does_not_repeat_first_center_after_small_initial_motion() {
        let processor = RoundBrushInputProcessor::default();
        let mut stroke = processor.begin_stroke();
        let mut emitted_positions = Vec::new();

        stroke
            .push_canvas_inputs(&[CanvasInput {
                time_ns: 0,
                position: glaphica_core::CanvasVec2::new(0.0, 0.0),
                pressure: 0.5,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            }])
            .expect("press input");
        if let Some(input) = stroke.drain_brush_input().expect("press drain") {
            emitted_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        stroke
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 1,
                    position: glaphica_core::CanvasVec2::new(0.5, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 2,
                    position: glaphica_core::CanvasVec2::new(1.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 3,
                    position: glaphica_core::CanvasVec2::new(8.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("motion inputs");
        if let Some(input) = stroke.drain_brush_input().expect("motion drain") {
            emitted_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        stroke.finish_stroke().expect("finish stroke");
        if let Some(input) = stroke.drain_brush_input().expect("finish drain") {
            emitted_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        let first_point_count = emitted_positions
            .iter()
            .filter(|&&(x, y)| x.abs() <= 1e-5 && y.abs() <= 1e-5)
            .count();
        assert_eq!(first_point_count, 1, "{emitted_positions:?}");
    }

    #[test]
    fn default_smoother_delays_second_center_after_small_initial_motion() {
        let processor = RoundBrushInputProcessor::default();
        let mut stroke = processor.begin_stroke();
        let mut emitted_positions = Vec::new();

        stroke
            .push_canvas_inputs(&[CanvasInput {
                time_ns: 0,
                position: glaphica_core::CanvasVec2::new(0.0, 0.0),
                pressure: 0.5,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            }])
            .expect("press input");
        if let Some(input) = stroke.drain_brush_input().expect("press drain") {
            emitted_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        stroke
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 1,
                    position: glaphica_core::CanvasVec2::new(0.5, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 2,
                    position: glaphica_core::CanvasVec2::new(1.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 3,
                    position: glaphica_core::CanvasVec2::new(8.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("motion inputs");
        if let Some(input) = stroke.drain_brush_input().expect("motion drain") {
            emitted_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        stroke.finish_stroke().expect("finish stroke");
        if let Some(input) = stroke.drain_brush_input().expect("finish drain") {
            emitted_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        assert_eq!(emitted_positions, vec![(0.0, 0.0)]);
    }

    #[test]
    fn passthrough_smoother_keeps_second_center_one_spacing_from_origin_after_small_initial_motion()
    {
        let processor =
            RoundBrushInputProcessor::default().with_smoother_factory(passthrough_smoother_factory);
        let mut stroke = processor.begin_stroke();
        let mut emitted_positions = Vec::new();

        stroke
            .push_canvas_inputs(&[CanvasInput {
                time_ns: 0,
                position: glaphica_core::CanvasVec2::new(0.0, 0.0),
                pressure: 0.5,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            }])
            .expect("press input");
        if let Some(input) = stroke.drain_brush_input().expect("press drain") {
            emitted_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        stroke
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 1,
                    position: glaphica_core::CanvasVec2::new(0.5, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 2,
                    position: glaphica_core::CanvasVec2::new(1.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 3,
                    position: glaphica_core::CanvasVec2::new(8.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("motion inputs");
        if let Some(input) = stroke.drain_brush_input().expect("motion drain") {
            emitted_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        stroke.finish_stroke().expect("finish stroke");
        if let Some(input) = stroke.drain_brush_input().expect("finish drain") {
            emitted_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        assert!(emitted_positions.len() >= 2, "{emitted_positions:?}");
        let second = emitted_positions[1];
        let distance_from_origin = (second.0 * second.0 + second.1 * second.1).sqrt();
        assert!(distance_from_origin >= 4.9, "{emitted_positions:?}");
    }

    #[test]
    fn round_processor_keeps_uniform_arclength_across_drains() {
        let processor = RoundBrushInputProcessor::default();
        let mut stroke = processor.begin_stroke();

        stroke
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 1,
                    position: glaphica_core::CanvasVec2::new(0.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 2,
                    position: glaphica_core::CanvasVec2::new(6.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("first push");
        let first = stroke
            .drain_brush_input()
            .expect("first drain")
            .expect("first input should exist");

        stroke
            .push_canvas_inputs(&[CanvasInput {
                time_ns: 3,
                position: glaphica_core::CanvasVec2::new(12.0, 0.0),
                pressure: 0.5,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            }])
            .expect("second push");
        stroke
            .push_canvas_inputs(&[CanvasInput {
                time_ns: 4,
                position: glaphica_core::CanvasVec2::new(18.0, 0.0),
                pressure: 0.5,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            }])
            .expect("third push");
        let second = stroke
            .drain_brush_input()
            .expect("second drain")
            .expect("second input should exist");
        stroke.finish_stroke().expect("finish stroke");
        let third = stroke
            .drain_brush_input()
            .expect("third drain")
            .expect("third input should exist");

        let mut positions = first
            .blocks
            .blocks()
            .iter()
            .chain(second.blocks.blocks().iter())
            .chain(third.blocks.blocks().iter())
            .map(|block| (block.values()[0], block.values()[1]))
            .collect::<Vec<_>>();
        positions.sort_by(|lhs, rhs| lhs.partial_cmp(rhs).expect("finite x compare"));

        assert_eq!(positions, vec![(0.0, 0.0), (5.0, 0.0), (10.0, 0.0)]);
    }

    #[test]
    fn round_processor_streaming_drains_match_finished_stroke_sampling_prefix() {
        let processor = RoundBrushInputProcessor::default();
        let inputs = (0..16)
            .map(|index| {
                let x = index as f32 * 12.0;
                let y = if index < 6 {
                    index as f32 * 6.0
                } else if index < 11 {
                    36.0 - (index as f32 - 6.0) * 4.0
                } else {
                    16.0 - (index as f32 - 11.0) * 3.0
                };
                CanvasInput {
                    time_ns: index as u64 * 1_000_000,
                    position: glaphica_core::CanvasVec2::new(x, y),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                }
            })
            .collect::<Vec<_>>();

        let mut streamed = processor.begin_stroke();
        let mut streamed_positions = Vec::new();
        for chunk in inputs.chunks(2) {
            streamed
                .push_canvas_inputs(chunk)
                .expect("streaming push should succeed");
            if let Some(input) = streamed.drain_brush_input().expect("streaming drain") {
                streamed_positions.extend(
                    input
                        .blocks
                        .blocks()
                        .iter()
                        .map(|block| (block.values()[0], block.values()[1])),
                );
            }
        }
        streamed.finish_stroke().expect("finish streaming stroke");
        if let Some(input) = streamed.drain_brush_input().expect("final streaming drain") {
            streamed_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        let mut finished = processor.begin_stroke();
        finished
            .push_canvas_inputs(&inputs)
            .expect("finished push should succeed");
        finished.finish_stroke().expect("finish complete stroke");
        let final_input = finished
            .drain_brush_input()
            .expect("finished drain")
            .expect("finished stroke should emit input");
        let finished_positions = final_input
            .blocks
            .blocks()
            .iter()
            .map(|block| (block.values()[0], block.values()[1]))
            .collect::<Vec<_>>();

        let comparable_len = streamed_positions
            .len()
            .min(finished_positions.len())
            .saturating_sub(4);
        assert!(comparable_len > 0);
        assert_eq!(
            &streamed_positions[..comparable_len],
            &finished_positions[..comparable_len]
        );
    }
}
