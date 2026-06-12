use gla_color::PremultipliedRgbaF32;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine2D {
    pub m11: f32,
    pub m12: f32,
    pub m21: f32,
    pub m22: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Affine2D {
    pub const IDENTITY: Self = Self {
        m11: 1.0,
        m12: 0.0,
        m21: 0.0,
        m22: 1.0,
        tx: 0.0,
        ty: 0.0,
    };
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mapping {
    Identity,
    Matrix(Affine2D),
}

impl Default for Mapping {
    fn default() -> Self {
        Self::Identity
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FootprintModifier {
    None,
    Expand(f32),
}

impl Default for FootprintModifier {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DrawOnToolKind {
    #[default]
    RadialKernel1D,
    ReplaceCircle4D,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawOnInput {
    pub center_x: f32,
    pub center_y: f32,
    pub footprint_radius_px: f32,
    pub kind: DrawOnInputKind,
}

impl DrawOnInput {
    pub fn radial_kernel_1d(
        center_x: f32,
        center_y: f32,
        footprint_radius_px: f32,
        radius_px: f32,
        amplitude: f32,
    ) -> Self {
        Self {
            center_x,
            center_y,
            footprint_radius_px,
            kind: DrawOnInputKind::RadialKernel1D {
                radius_px,
                amplitude,
            },
        }
    }

    pub fn replace_circle_4d(
        center_x: f32,
        center_y: f32,
        footprint_radius_px: f32,
        radius_px: f32,
        color: PremultipliedRgbaF32,
    ) -> Self {
        Self {
            center_x,
            center_y,
            footprint_radius_px,
            kind: DrawOnInputKind::ReplaceCircle4D { radius_px, color },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DrawOnInputKind {
    RadialKernel1D {
        radius_px: f32,
        amplitude: f32,
    },
    ReplaceCircle4D {
        radius_px: f32,
        color: PremultipliedRgbaF32,
    },
}
