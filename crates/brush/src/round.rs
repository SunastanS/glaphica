use crate::{
    BrushId, BrushInput, BrushInputBlockList, BrushInputError, BrushInputProcessor,
    BrushShaderRegistration, CanvasInput,
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

pub const ROUND_INPUT_BLOCK_LEN: usize = 8;

#[derive(Debug, Clone, PartialEq)]
pub struct RoundBrushInputProcessor {
    base_radius_px: f32,
    base_hardness: f32,
    base_opacity: f32,
    tint: [f32; 3],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
struct RoundApplyPayload {
    center_local: [f32; 2],
    radius_px: f32,
    hardness: f32,
    opacity: f32,
    _pad1: [u32; 3],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
struct RoundMergePayload {
    tint: [f32; 3],
    _pad2: f32,
}

pub fn encode_round_apply_payload(
    center_local: [f32; 2],
    radius_px: f32,
    hardness: f32,
    opacity: f32,
) -> Vec<u8> {
    bytemuck::bytes_of(&RoundApplyPayload {
        center_local,
        radius_px,
        hardness,
        opacity,
        _pad1: [0; 3],
    })
    .to_vec()
}

pub fn encode_round_merge_payload(tint: [f32; 3]) -> Vec<u8> {
    bytemuck::bytes_of(&RoundMergePayload { tint, _pad2: 0.0 }).to_vec()
}

impl Default for RoundBrushInputProcessor {
    fn default() -> Self {
        Self {
            base_radius_px: 5.0,
            base_hardness: 0.7,
            base_opacity: 1.0,
            tint: [0.0, 0.0, 0.0],
        }
    }
}

impl BrushInputProcessor for RoundBrushInputProcessor {
    fn produce_input(
        &self,
        canvas_input: &[CanvasInput],
    ) -> Result<BrushInput, BrushInputError> {
        let mut blocks = BrushInputBlockList::new(ROUND_BRUSH_ID);
        for (block_index, sample) in canvas_input.iter().copied().enumerate() {
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
            blocks.push_block(vec![
                sample.position.x,
                sample.position.y,
                self.base_radius_px,
                self.base_hardness,
                self.base_opacity * pressure,
                self.tint[0],
                self.tint[1],
                self.tint[2],
            ]);
        }

        Ok(BrushInput {
            brush_id: ROUND_BRUSH_ID,
            blocks,
        })
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
        let local_center = [center.x - tile_canvas_origin.x, center.y - tile_canvas_origin.y];
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
        Ok(encode_round_merge_payload([last[5], last[6], last[7]]))
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        BrushInput, BrushInputBlockList, BrushInputProcessor, CanvasInput,
        round::{ROUND_BRUSH_ID, RoundBrushInputProcessor, encode_round_merge_payload},
    };
    use glaphica_core::CanvasVec2;

    #[test]
    fn round_processor_encodes_payloads_from_blocks() {
        let mut input = BrushInputBlockList::new(ROUND_BRUSH_ID);
        input.push_block(vec![10.0, 8.0, 6.0, 0.4, 0.7, 0.2, 0.3, 0.4]);
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
            encode_round_merge_payload([0.2, 0.3, 0.4])
        );
    }

    #[test]
    fn round_processor_produces_blocks_from_canvas_input() {
        let processor = RoundBrushInputProcessor::default();
        let input = [CanvasInput {
            time_ns: 1,
            position: glaphica_core::CanvasVec2::new(11.0, 13.0),
            pressure: 0.5,
            tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
            twist: 0.0,
        }];

        let result = processor
            .produce_input(&input)
            .expect("production should succeed");

        assert_eq!(result.brush_id, ROUND_BRUSH_ID);
        assert_eq!(result.blocks.blocks().len(), 1);
        assert_eq!(result.blocks.blocks()[0].values()[0], 11.0);
        assert_eq!(result.blocks.blocks()[0].values()[1], 13.0);
        assert_eq!(result.blocks.blocks()[0].values()[2], 5.0);
        assert_eq!(result.blocks.blocks()[0].values()[3], 0.7);
        assert_eq!(result.blocks.blocks()[0].values()[4], 0.5);
    }
}
