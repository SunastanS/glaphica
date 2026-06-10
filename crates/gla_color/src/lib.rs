#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelCount {
    D1,
    D2,
    D4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RgbaBlendMode {
    Overlay,
    Multiply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueToRgbaBlendMode {
    MaskAlpha,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendMode {
    Overlay,
    Multiply,
    MaskAlpha,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelType {
    U8,
    U32,
    F32,
    F64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlaFormat {
    pub channel_count: ChannelCount,
    pub channel_type: ChannelType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorSpace {
    LinearSrgb,
    Srgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlphaMode {
    None,
    Straight,
    Premultiplied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueSemantic {
    Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelInterpretation {
    Rgba {
        color_space: ColorSpace,
        alpha: AlphaMode,
    },
    Value {
        semantic: ValueSemantic,
    },
}

impl GlaFormat {
    pub fn default_interpretation(self) -> Option<PixelInterpretation> {
        match self.channel_count {
            ChannelCount::D1 => Some(PixelInterpretation::Value {
                semantic: ValueSemantic::Value,
            }),
            ChannelCount::D2 => None,
            ChannelCount::D4 => Some(PixelInterpretation::Rgba {
                color_space: ColorSpace::LinearSrgb,
                alpha: AlphaMode::Premultiplied,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositeKind {
    Rgba(RgbaBlendMode),
    ValueToRgba(ValueToRgbaBlendMode),
}

pub fn composite_kind(
    src_format: GlaFormat,
    dst_format: GlaFormat,
    blend_mode: BlendMode,
) -> Option<CompositeKind> {
    match (
        src_format.default_interpretation(),
        dst_format.default_interpretation(),
        blend_mode,
    ) {
        (
            Some(PixelInterpretation::Rgba {
                color_space: ColorSpace::LinearSrgb,
                alpha: AlphaMode::Premultiplied,
            }),
            Some(PixelInterpretation::Rgba {
                color_space: ColorSpace::LinearSrgb,
                alpha: AlphaMode::Premultiplied,
            }),
            BlendMode::Overlay,
        ) => Some(CompositeKind::Rgba(RgbaBlendMode::Overlay)),
        (
            Some(PixelInterpretation::Rgba {
                color_space: ColorSpace::LinearSrgb,
                alpha: AlphaMode::Premultiplied,
            }),
            Some(PixelInterpretation::Rgba {
                color_space: ColorSpace::LinearSrgb,
                alpha: AlphaMode::Premultiplied,
            }),
            BlendMode::Multiply,
        ) => Some(CompositeKind::Rgba(RgbaBlendMode::Multiply)),
        (
            Some(PixelInterpretation::Value {
                semantic: ValueSemantic::Value,
            }),
            Some(PixelInterpretation::Rgba {
                color_space: ColorSpace::LinearSrgb,
                alpha: AlphaMode::Premultiplied,
            }),
            BlendMode::MaskAlpha,
        ) => Some(CompositeKind::ValueToRgba(ValueToRgbaBlendMode::MaskAlpha)),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PremultipliedRgbaF32 {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl PremultipliedRgbaF32 {
    pub const TRANSPARENT: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    pub fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }
}

pub fn composite_premultiplied_rgba(
    backdrop: PremultipliedRgbaF32,
    source: PremultipliedRgbaF32,
    mode: RgbaBlendMode,
    opacity: f32,
) -> PremultipliedRgbaF32 {
    let opacity = opacity.clamp(0.0, 1.0);
    let source = PremultipliedRgbaF32 {
        r: source.r * opacity,
        g: source.g * opacity,
        b: source.b * opacity,
        a: source.a * opacity,
    };
    let backdrop_alpha = backdrop.a.clamp(0.0, 1.0);
    let source_alpha = source.a.clamp(0.0, 1.0);
    let backdrop_rgb = unpremultiply(backdrop);
    let source_rgb = unpremultiply(source);
    let blended_rgb = blend_rgb(backdrop_rgb, source_rgb, mode);
    let out_alpha = source_alpha + backdrop_alpha * (1.0 - source_alpha);
    let out_rgb = [
        (1.0 - source_alpha) * backdrop_alpha * backdrop_rgb[0]
            + (1.0 - backdrop_alpha) * source_alpha * source_rgb[0]
            + backdrop_alpha * source_alpha * blended_rgb[0],
        (1.0 - source_alpha) * backdrop_alpha * backdrop_rgb[1]
            + (1.0 - backdrop_alpha) * source_alpha * source_rgb[1]
            + backdrop_alpha * source_alpha * blended_rgb[1],
        (1.0 - source_alpha) * backdrop_alpha * backdrop_rgb[2]
            + (1.0 - backdrop_alpha) * source_alpha * source_rgb[2]
            + backdrop_alpha * source_alpha * blended_rgb[2],
    ];
    PremultipliedRgbaF32 {
        r: out_rgb[0],
        g: out_rgb[1],
        b: out_rgb[2],
        a: out_alpha,
    }
}

pub fn apply_value_mask_to_premultiplied_rgba(
    color: PremultipliedRgbaF32,
    value: f32,
    opacity: f32,
) -> PremultipliedRgbaF32 {
    let factor = (value * opacity).clamp(0.0, 1.0);
    PremultipliedRgbaF32 {
        r: color.r * factor,
        g: color.g * factor,
        b: color.b * factor,
        a: color.a * factor,
    }
}

fn unpremultiply(color: PremultipliedRgbaF32) -> [f32; 3] {
    if color.a <= 0.0 {
        return [0.0, 0.0, 0.0];
    }
    [color.r / color.a, color.g / color.a, color.b / color.a]
}

fn blend_rgb(backdrop: [f32; 3], source: [f32; 3], mode: RgbaBlendMode) -> [f32; 3] {
    match mode {
        RgbaBlendMode::Multiply => [
            backdrop[0] * source[0],
            backdrop[1] * source[1],
            backdrop[2] * source[2],
        ],
        RgbaBlendMode::Overlay => [
            overlay_channel(backdrop[0], source[0]),
            overlay_channel(backdrop[1], source[1]),
            overlay_channel(backdrop[2], source[2]),
        ],
    }
}

fn overlay_channel(backdrop: f32, source: f32) -> f32 {
    if backdrop <= 0.5 {
        2.0 * backdrop * source
    } else {
        1.0 - 2.0 * (1.0 - backdrop) * (1.0 - source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d1_u8() -> GlaFormat {
        GlaFormat {
            channel_count: ChannelCount::D1,
            channel_type: ChannelType::U8,
        }
    }

    fn d2_u8() -> GlaFormat {
        GlaFormat {
            channel_count: ChannelCount::D2,
            channel_type: ChannelType::U8,
        }
    }

    fn d4_u8() -> GlaFormat {
        GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::U8,
        }
    }

    #[test]
    fn default_interpretation_maps_dimensions_to_semantics() {
        assert_eq!(
            d1_u8().default_interpretation(),
            Some(PixelInterpretation::Value {
                semantic: ValueSemantic::Value
            })
        );
        assert_eq!(d2_u8().default_interpretation(), None);
        assert_eq!(
            d4_u8().default_interpretation(),
            Some(PixelInterpretation::Rgba {
                color_space: ColorSpace::LinearSrgb,
                alpha: AlphaMode::Premultiplied
            })
        );
    }

    #[test]
    fn composite_kind_allows_supported_dimension_pairs_only() {
        assert_eq!(
            composite_kind(d4_u8(), d4_u8(), BlendMode::Multiply),
            Some(CompositeKind::Rgba(RgbaBlendMode::Multiply))
        );
        assert_eq!(
            composite_kind(d1_u8(), d4_u8(), BlendMode::MaskAlpha),
            Some(CompositeKind::ValueToRgba(ValueToRgbaBlendMode::MaskAlpha))
        );
        assert_eq!(composite_kind(d1_u8(), d4_u8(), BlendMode::Multiply), None);
        assert_eq!(composite_kind(d2_u8(), d4_u8(), BlendMode::MaskAlpha), None);
    }

    #[test]
    fn value_mask_scales_premultiplied_rgba() {
        let color = PremultipliedRgbaF32::new(0.4, 0.2, 0.1, 0.5);

        assert_eq!(
            apply_value_mask_to_premultiplied_rgba(color, 0.5, 0.5),
            PremultipliedRgbaF32::new(0.1, 0.05, 0.025, 0.125)
        );
    }

    #[test]
    fn rgba_multiply_composites_premultiplied_colors() {
        let backdrop = PremultipliedRgbaF32::new(0.5, 0.25, 0.75, 1.0);
        let source = PremultipliedRgbaF32::new(0.5, 0.5, 0.25, 1.0);

        assert_eq!(
            composite_premultiplied_rgba(backdrop, source, RgbaBlendMode::Multiply, 1.0),
            PremultipliedRgbaF32::new(0.25, 0.125, 0.1875, 1.0)
        );
    }
}
