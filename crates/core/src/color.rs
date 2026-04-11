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

    pub fn to_hsv(self) -> [f32; 3] {
        let [r, g, b, _] = self.to_unorm_f32();
        rgb_to_hsv(r, g, b)
    }

    pub fn to_hsl(self) -> [f32; 3] {
        let [r, g, b, _] = self.to_unorm_f32();
        rgb_to_hsl(r, g, b)
    }

    pub fn to_hsva(self) -> [f32; 4] {
        let [h, s, v] = self.to_hsv();
        [h, s, v, self.a as f32 / 255.0]
    }

    pub fn to_hsla(self) -> [f32; 4] {
        let [h, s, l] = self.to_hsl();
        [h, s, l, self.a as f32 / 255.0]
    }

    pub fn from_hsv(hsv: [f32; 3], alpha: f32) -> Self {
        let [r, g, b] = hsv_to_rgb(hsv[0], hsv[1], hsv[2]);
        Self::from_unorm_f32([r, g, b, alpha])
    }

    pub fn from_hsl(hsl: [f32; 3], alpha: f32) -> Self {
        let [r, g, b] = hsl_to_rgb(hsl[0], hsl[1], hsl[2]);
        Self::from_unorm_f32([r, g, b, alpha])
    }

    pub fn from_hsva(hsva: [f32; 4]) -> Self {
        Self::from_hsv([hsva[0], hsva[1], hsva[2]], hsva[3])
    }

    pub fn from_hsla(hsla: [f32; 4]) -> Self {
        Self::from_hsl([hsla[0], hsla[1], hsla[2]], hsla[3])
    }

    pub fn to_linear_rgb(self) -> [f32; 3] {
        let [r, g, b, _] = self.to_unorm_f32();
        [
            srgb_channel_to_linear(r),
            srgb_channel_to_linear(g),
            srgb_channel_to_linear(b),
        ]
    }

    pub fn to_linear_rgba(self) -> [f32; 4] {
        let [r, g, b] = self.to_linear_rgb();
        [r, g, b, self.a as f32 / 255.0]
    }

    pub fn from_linear_rgb(rgb: [f32; 3], alpha: f32) -> Self {
        Self::from_unorm_f32([
            linear_channel_to_srgb(rgb[0]).clamp(0.0, 1.0),
            linear_channel_to_srgb(rgb[1]).clamp(0.0, 1.0),
            linear_channel_to_srgb(rgb[2]).clamp(0.0, 1.0),
            alpha,
        ])
    }

    pub fn from_linear_rgba(rgba: [f32; 4]) -> Self {
        Self::from_linear_rgb([rgba[0], rgba[1], rgba[2]], rgba[3])
    }

    pub fn to_xyz(self) -> [f32; 3] {
        let [r, g, b, _] = self.to_unorm_f32();
        srgb_to_xyz(r, g, b)
    }

    pub fn to_xyza(self) -> [f32; 4] {
        let [x, y, z] = self.to_xyz();
        [x, y, z, self.a as f32 / 255.0]
    }

    pub fn from_xyz(xyz: [f32; 3], alpha: f32) -> Self {
        let [r, g, b] = xyz_to_srgb(xyz[0], xyz[1], xyz[2]);
        Self::from_unorm_f32([r, g, b, alpha])
    }

    pub fn from_xyza(xyza: [f32; 4]) -> Self {
        Self::from_xyz([xyza[0], xyza[1], xyza[2]], xyza[3])
    }

    pub fn to_lab(self) -> [f32; 3] {
        let [r, g, b, _] = self.to_unorm_f32();
        srgb_to_lab(r, g, b)
    }

    pub fn to_laba(self) -> [f32; 4] {
        let [l, a, b] = self.to_lab();
        [l, a, b, self.a as f32 / 255.0]
    }

    pub fn from_lab(lab: [f32; 3], alpha: f32) -> Self {
        let [r, g, b] = lab_to_srgb(lab[0], lab[1], lab[2]);
        Self::from_unorm_f32([r, g, b, alpha])
    }

    pub fn to_lch(self) -> [f32; 3] {
        let [l, a, b] = self.to_lab();
        lab_to_lch(l, a, b)
    }

    pub fn to_lcha(self) -> [f32; 4] {
        let [l, c, h] = self.to_lch();
        [l, c, h, self.a as f32 / 255.0]
    }

    pub fn from_laba(laba: [f32; 4]) -> Self {
        Self::from_lab([laba[0], laba[1], laba[2]], laba[3])
    }

    pub fn from_lch(lch: [f32; 3], alpha: f32) -> Self {
        let [l, a, b] = lch_to_lab(lch[0], lch[1], lch[2]);
        Self::from_lab([l, a, b], alpha)
    }

    pub fn from_lcha(lcha: [f32; 4]) -> Self {
        Self::from_lch([lcha[0], lcha[1], lcha[2]], lcha[3])
    }

    pub fn to_oklab(self) -> [f32; 3] {
        let [r, g, b] = self.to_linear_rgb();
        linear_srgb_to_oklab(r, g, b)
    }

    pub fn to_oklaba(self) -> [f32; 4] {
        let [l, a, b] = self.to_oklab();
        [l, a, b, self.a as f32 / 255.0]
    }

    pub fn from_oklab(oklab: [f32; 3], alpha: f32) -> Self {
        let [r, g, b] = oklab_to_linear_srgb(oklab[0], oklab[1], oklab[2]);
        Self::from_linear_rgb([r, g, b], alpha)
    }

    pub fn from_oklaba(oklaba: [f32; 4]) -> Self {
        Self::from_oklab([oklaba[0], oklaba[1], oklaba[2]], oklaba[3])
    }

    pub fn to_oklch(self) -> [f32; 3] {
        let [l, a, b] = self.to_oklab();
        lab_to_lch(l, a, b)
    }

    pub fn to_oklcha(self) -> [f32; 4] {
        let [l, c, h] = self.to_oklch();
        [l, c, h, self.a as f32 / 255.0]
    }

    pub fn from_oklch(oklch: [f32; 3], alpha: f32) -> Self {
        let [l, a, b] = lch_to_lab(oklch[0], oklch[1], oklch[2]);
        Self::from_oklab([l, a, b], alpha)
    }

    pub fn from_oklcha(oklcha: [f32; 4]) -> Self {
        Self::from_oklch([oklcha[0], oklcha[1], oklcha[2]], oklcha[3])
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

fn rgb_to_hsv(r: f32, g: f32, b: f32) -> [f32; 3] {
    let max = r.max(g.max(b));
    let min = r.min(g.min(b));
    let delta = max - min;

    let hue = rgb_to_hue(r, g, b, max, delta);
    let saturation = if max <= f32::EPSILON {
        0.0
    } else {
        delta / max
    };
    [hue, saturation, max]
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> [f32; 3] {
    let max = r.max(g.max(b));
    let min = r.min(g.min(b));
    let delta = max - min;
    let lightness = (max + min) * 0.5;

    let saturation = if delta <= f32::EPSILON {
        0.0
    } else {
        delta / (1.0 - (2.0 * lightness - 1.0).abs())
    };

    [rgb_to_hue(r, g, b, max, delta), saturation, lightness]
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let hue = h.rem_euclid(360.0);
    let saturation = s.clamp(0.0, 1.0);
    let value = v.clamp(0.0, 1.0);

    if saturation <= f32::EPSILON {
        return [value, value, value];
    }

    let c = value * saturation;
    let x = c * (1.0 - ((hue / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = value - c;

    let (r1, g1, b1) = match hue {
        hue if hue < 60.0 => (c, x, 0.0),
        hue if hue < 120.0 => (x, c, 0.0),
        hue if hue < 180.0 => (0.0, c, x),
        hue if hue < 240.0 => (0.0, x, c),
        hue if hue < 300.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };

    [r1 + m, g1 + m, b1 + m]
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    let hue = h.rem_euclid(360.0);
    let saturation = s.clamp(0.0, 1.0);
    let lightness = l.clamp(0.0, 1.0);

    let c = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let x = c * (1.0 - ((hue / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = lightness - c * 0.5;

    let (r1, g1, b1) = match hue {
        hue if hue < 60.0 => (c, x, 0.0),
        hue if hue < 120.0 => (x, c, 0.0),
        hue if hue < 180.0 => (0.0, c, x),
        hue if hue < 240.0 => (0.0, x, c),
        hue if hue < 300.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };

    [r1 + m, g1 + m, b1 + m]
}

fn rgb_to_hue(r: f32, g: f32, b: f32, max: f32, delta: f32) -> f32 {
    if delta <= f32::EPSILON {
        0.0
    } else if max == r {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    }
}

fn srgb_to_lab(r: f32, g: f32, b: f32) -> [f32; 3] {
    let [x, y, z] = srgb_to_xyz(r, g, b);
    xyz_to_lab(x, y, z)
}

fn lab_to_srgb(l: f32, a: f32, b: f32) -> [f32; 3] {
    let [x, y, z] = lab_to_xyz(l, a, b);
    xyz_to_srgb(x, y, z)
}

fn lab_to_lch(l: f32, a: f32, b: f32) -> [f32; 3] {
    let chroma = (a * a + b * b).sqrt();
    let hue = if chroma <= f32::EPSILON {
        0.0
    } else {
        b.atan2(a).to_degrees().rem_euclid(360.0)
    };
    [l, chroma, hue]
}

fn lch_to_lab(l: f32, c: f32, h: f32) -> [f32; 3] {
    let hue = h.rem_euclid(360.0).to_radians();
    [l, c * hue.cos(), c * hue.sin()]
}

fn srgb_to_xyz(r: f32, g: f32, b: f32) -> [f32; 3] {
    let r = srgb_channel_to_linear(r);
    let g = srgb_channel_to_linear(g);
    let b = srgb_channel_to_linear(b);

    [
        (0.412_456_4 * r) + (0.357_576_1 * g) + (0.180_437_5 * b),
        (0.212_672_9 * r) + (0.715_152_2 * g) + (0.072_175 * b),
        (0.019_333_9 * r) + (0.119_192 * g) + (0.950_304_1 * b),
    ]
}

fn xyz_to_srgb(x: f32, y: f32, z: f32) -> [f32; 3] {
    let r = (3.240_454_2 * x) + (-1.537_138_5 * y) + (-0.498_531_4 * z);
    let g = (-0.969_266 * x) + (1.876_010_8 * y) + (0.041_556 * z);
    let b = (0.055_643_4 * x) + (-0.204_025_9 * y) + (1.057_225_2 * z);

    [
        linear_channel_to_srgb(r).clamp(0.0, 1.0),
        linear_channel_to_srgb(g).clamp(0.0, 1.0),
        linear_channel_to_srgb(b).clamp(0.0, 1.0),
    ]
}

fn linear_srgb_to_oklab(r: f32, g: f32, b: f32) -> [f32; 3] {
    let l = 0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;

    let l_root = l.cbrt();
    let m_root = m.cbrt();
    let s_root = s.cbrt();

    [
        0.210_454_26 * l_root + 0.793_617_8 * m_root - 0.004_072_047 * s_root,
        1.977_998_5 * l_root - 2.428_592_2 * m_root + 0.450_593_7 * s_root,
        0.025_904_037 * l_root + 0.782_771_77 * m_root - 0.808_675_77 * s_root,
    ]
}

fn oklab_to_linear_srgb(l: f32, a: f32, b: f32) -> [f32; 3] {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;

    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;

    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

fn xyz_to_lab(x: f32, y: f32, z: f32) -> [f32; 3] {
    let fx = lab_f(x / D65_WHITE[0]);
    let fy = lab_f(y / D65_WHITE[1]);
    let fz = lab_f(z / D65_WHITE[2]);

    [(116.0 * fy) - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn lab_to_xyz(l: f32, a: f32, b: f32) -> [f32; 3] {
    let fy = (l + 16.0) / 116.0;
    let fx = fy + (a / 500.0);
    let fz = fy - (b / 200.0);

    [
        D65_WHITE[0] * lab_f_inv(fx),
        D65_WHITE[1] * lab_f_inv(fy),
        D65_WHITE[2] * lab_f_inv(fz),
    ]
}

fn srgb_channel_to_linear(channel: f32) -> f32 {
    let channel = channel.clamp(0.0, 1.0);
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_channel_to_srgb(channel: f32) -> f32 {
    if channel <= 0.003_130_8 {
        12.92 * channel
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    }
}

fn lab_f(t: f32) -> f32 {
    if t > LAB_EPSILON {
        t.cbrt()
    } else {
        ((LAB_KAPPA * t) + 16.0) / 116.0
    }
}

fn lab_f_inv(t: f32) -> f32 {
    let t3 = t * t * t;
    if t3 > LAB_EPSILON {
        t3
    } else {
        (116.0 * t - 16.0) / LAB_KAPPA
    }
}

const D65_WHITE: [f32; 3] = [0.950_47, 1.0, 1.088_83];
const LAB_EPSILON: f32 = 216.0 / 24_389.0;
const LAB_KAPPA: f32 = 24_389.0 / 27.0;

#[cfg(test)]
mod tests {
    use super::Color;

    #[test]
    fn hsv_roundtrip_preserves_primary_color() {
        let color = Color::new(255, 0, 0, 128);
        let hsva = color.to_hsva();

        assert!((hsva[0] - 0.0).abs() < 0.001);
        assert!((hsva[1] - 1.0).abs() < 0.001);
        assert!((hsva[2] - 1.0).abs() < 0.001);
        assert!((hsva[3] - (128.0 / 255.0)).abs() < 0.001);
        assert_eq!(Color::from_hsva(hsva), color);
    }

    #[test]
    fn hsv_accepts_wrapped_hue() {
        let color = Color::from_hsv([420.0, 1.0, 1.0], 1.0);

        assert_eq!(color, Color::new(255, 255, 0, 255));
    }

    #[test]
    fn hsl_roundtrip_preserves_primary_color() {
        let color = Color::new(255, 0, 0, 128);
        let hsla = color.to_hsla();

        assert!((hsla[0] - 0.0).abs() < 0.001);
        assert!((hsla[1] - 1.0).abs() < 0.001);
        assert!((hsla[2] - 0.5).abs() < 0.001);
        assert!((hsla[3] - (128.0 / 255.0)).abs() < 0.001);
        assert_eq!(Color::from_hsla(hsla), color);
    }

    #[test]
    fn hsl_handles_grayscale() {
        let color = Color::new(64, 64, 64, 255);
        let hsl = color.to_hsl();

        assert!(hsl[0].abs() < 0.001);
        assert!(hsl[1].abs() < 0.001);
        assert!((hsl[2] - (64.0 / 255.0)).abs() < 0.001);
        assert_eq!(Color::from_hsl(hsl, 1.0), color);
    }

    #[test]
    fn lab_roundtrip_stays_close() {
        let color = Color::new(12, 34, 56, 78);
        let restored = Color::from_laba(color.to_laba());

        assert_rgba_close(color, restored, 1);
    }

    #[test]
    fn lab_white_point_matches_reference() {
        let lab = Color::WHITE.to_lab();

        assert!((lab[0] - 100.0).abs() < 0.01);
        assert!(lab[1].abs() < 0.01);
        assert!(lab[2].abs() < 0.01);
    }

    #[test]
    fn linear_rgb_roundtrip_stays_close() {
        let color = Color::new(12, 34, 56, 78);
        let restored = Color::from_linear_rgba(color.to_linear_rgba());

        assert_rgba_close(color, restored, 1);
    }

    #[test]
    fn xyz_roundtrip_stays_close() {
        let color = Color::new(90, 120, 150, 200);
        let restored = Color::from_xyza(color.to_xyza());

        assert_rgba_close(color, restored, 1);
    }

    #[test]
    fn xyz_white_matches_d65_reference() {
        let xyz = Color::WHITE.to_xyz();

        assert!((xyz[0] - 0.950_47).abs() < 0.0001);
        assert!((xyz[1] - 1.0).abs() < 0.0001);
        assert!((xyz[2] - 1.088_83).abs() < 0.0001);
    }

    #[test]
    fn lch_roundtrip_stays_close() {
        let color = Color::new(120, 80, 40, 160);
        let restored = Color::from_lcha(color.to_lcha());

        assert_rgba_close(color, restored, 1);
    }

    #[test]
    fn oklab_roundtrip_stays_close() {
        let color = Color::new(20, 140, 220, 180);
        let restored = Color::from_oklaba(color.to_oklaba());

        assert_rgba_close(color, restored, 1);
    }

    #[test]
    fn oklab_white_matches_reference() {
        let oklab = Color::WHITE.to_oklab();

        assert!((oklab[0] - 1.0).abs() < 0.001);
        assert!(oklab[1].abs() < 0.001);
        assert!(oklab[2].abs() < 0.001);
    }

    #[test]
    fn oklch_roundtrip_stays_close() {
        let color = Color::new(210, 70, 150, 90);
        let restored = Color::from_oklcha(color.to_oklcha());

        assert_rgba_close(color, restored, 1);
    }

    fn assert_rgba_close(expected: Color, actual: Color, tolerance: u8) {
        for (expected, actual) in expected.rgba8().into_iter().zip(actual.rgba8()) {
            assert!(expected.abs_diff(actual) <= tolerance);
        }
    }
}
