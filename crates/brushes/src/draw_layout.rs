#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushDrawKind {
    PixelRect,
    Round,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushDrawInputShape {
    F32,
    Vec2F32,
    Vec3F32,
    Vec4F32,
}

impl BrushDrawInputShape {
    pub const fn lane_count(self) -> usize {
        match self {
            Self::F32 => 1,
            Self::Vec2F32 => 2,
            Self::Vec3F32 => 3,
            Self::Vec4F32 => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrushDrawInputLayout {
    kind: BrushDrawKind,
    shape: &'static [BrushDrawInputShape],
}

impl BrushDrawInputLayout {
    pub const fn new(kind: BrushDrawKind, shape: &'static [BrushDrawInputShape]) -> Self {
        Self { kind, shape }
    }

    pub const fn kind(self) -> BrushDrawKind {
        self.kind
    }

    pub const fn shape(self) -> &'static [BrushDrawInputShape] {
        self.shape
    }

    pub const fn total_f32_count(self) -> usize {
        let mut total = 0usize;
        let mut index = 0usize;
        while index < self.shape.len() {
            total += self.shape[index].lane_count();
            index += 1;
        }
        total
    }

    pub fn validate_input(self, input: &[f32]) -> bool {
        input.len() == self.total_f32_count()
    }

    pub fn slot_range(self, slot_index: usize) -> Option<std::ops::Range<usize>> {
        let mut start = 0usize;
        for (index, shape) in self.shape.iter().copied().enumerate() {
            let end = start + shape.lane_count();
            if index == slot_index {
                return Some(start..end);
            }
            start = end;
        }
        None
    }

    pub fn slot_slice<'a>(self, input: &'a [f32], slot_index: usize) -> Option<&'a [f32]> {
        if !self.validate_input(input) {
            return None;
        }
        let range = self.slot_range(slot_index)?;
        input.get(range)
    }
}

#[cfg(test)]
mod tests {
    use super::{BrushDrawInputLayout, BrushDrawInputShape, BrushDrawKind};

    #[test]
    fn total_f32_count_sums_all_slot_shapes() {
        let layout = BrushDrawInputLayout::new(
            BrushDrawKind::PixelRect,
            &[BrushDrawInputShape::Vec2F32, BrushDrawInputShape::F32],
        );
        assert_eq!(layout.total_f32_count(), 3);
    }

    #[test]
    fn slot_slice_uses_shape_offsets() {
        let layout = BrushDrawInputLayout::new(
            BrushDrawKind::PixelRect,
            &[BrushDrawInputShape::Vec2F32, BrushDrawInputShape::F32],
        );
        let slot = layout.slot_slice(&[11.0, 12.0, 3.0], 1);
        assert_eq!(slot, Some(&[3.0][..]));
    }

    #[test]
    fn slot_slice_rejects_input_len_mismatch() {
        let layout =
            BrushDrawInputLayout::new(BrushDrawKind::PixelRect, &[BrushDrawInputShape::Vec2F32]);
        assert_eq!(layout.slot_slice(&[1.0], 0), None);
    }
}
