#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Color {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl Color {
    pub const TRANSPARENT: Self = Self::new(0, 0, 0, 0);
    pub const BLACK: Self = Self::new(0, 0, 0, 255);
    pub const WHITE: Self = Self::new(255, 255, 255, 255);

    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn rgba8(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }

    pub const fn r(self) -> u8 {
        self.r
    }

    pub const fn g(self) -> u8 {
        self.g
    }

    pub const fn b(self) -> u8 {
        self.b
    }

    pub const fn a(self) -> u8 {
        self.a
    }

    pub fn to_unorm_f32(self) -> [f32; 4] {
        [
            self.r as f32 / 255.0,
            self.g as f32 / 255.0,
            self.b as f32 / 255.0,
            self.a as f32 / 255.0,
        ]
    }

    pub fn from_unorm_f32(rgba: [f32; 4]) -> Self {
        Self::new(
            unorm_to_u8(rgba[0]),
            unorm_to_u8(rgba[1]),
            unorm_to_u8(rgba[2]),
            unorm_to_u8(rgba[3]),
        )
    }
}

impl From<[u8; 4]> for Color {
    fn from(value: [u8; 4]) -> Self {
        Self::new(value[0], value[1], value[2], value[3])
    }
}

impl From<Color> for [u8; 4] {
    fn from(value: Color) -> Self {
        value.rgba8()
    }
}

fn unorm_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}
