use std::fmt;
use std::sync::Arc;

use lcms2::{Flags, Intent, PixelFormat, Profile, ToneCurve, Transform};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaMode {
    Straight,
    Premultiplied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderingIntent {
    Perceptual,
    RelativeColorimetric,
    Saturation,
    AbsoluteColorimetric,
}

impl RenderingIntent {
    fn to_lcms(self) -> Intent {
        match self {
            Self::Perceptual => Intent::Perceptual,
            Self::RelativeColorimetric => Intent::RelativeColorimetric,
            Self::Saturation => Intent::Saturation,
            Self::AbsoluteColorimetric => Intent::AbsoluteColorimetric,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SimpleTransferCurve {
    Linear,
    Gamma(f64),
    Srgb,
}

impl SimpleTransferCurve {
    fn build_tone_curve(self) -> Result<ToneCurve, ColorManagementError> {
        match self {
            Self::Linear => Ok(ToneCurve::new(1.0)),
            Self::Gamma(gamma) if gamma.is_finite() && gamma > 0.0 => Ok(ToneCurve::new(gamma)),
            Self::Gamma(gamma) => Err(ColorManagementError::InvalidGamma(gamma)),
            Self::Srgb => ToneCurve::new_parametric(
                4,
                &[2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045],
            )
            .map_err(ColorManagementError::from),
        }
    }

    fn gpu_transfer(self) -> GpuTransferCurve {
        match self {
            Self::Linear => GpuTransferCurve::Linear,
            Self::Gamma(gamma) => GpuTransferCurve::Gamma(gamma as f32),
            Self::Srgb => GpuTransferCurve::Srgb,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chromaticity {
    pub x: f64,
    pub y: f64,
}

impl Chromaticity {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    fn validate(self) -> Result<(), ColorManagementError> {
        if !self.x.is_finite() || !self.y.is_finite() || self.y <= 0.0 {
            return Err(ColorManagementError::InvalidChromaticity(self));
        }
        if self.x <= 0.0 || self.x >= 1.0 || self.y >= 1.0 || self.x + self.y >= 1.0 {
            return Err(ColorManagementError::InvalidChromaticity(self));
        }
        Ok(())
    }

    fn to_lcms_xyy(self) -> lcms2::CIExyY {
        lcms2::CIExyY {
            x: self.x,
            y: self.y,
            Y: 1.0,
        }
    }

    fn to_xyz(self) -> [f64; 3] {
        let x = self.x / self.y;
        let y = 1.0;
        let z = (1.0 - self.x - self.y) / self.y;
        [x, y, z]
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RgbPrimaries {
    pub red: Chromaticity,
    pub green: Chromaticity,
    pub blue: Chromaticity,
}

impl RgbPrimaries {
    pub const fn new(red: Chromaticity, green: Chromaticity, blue: Chromaticity) -> Self {
        Self { red, green, blue }
    }

    pub const fn srgb() -> Self {
        Self::new(
            Chromaticity::new(0.64, 0.33),
            Chromaticity::new(0.30, 0.60),
            Chromaticity::new(0.15, 0.06),
        )
    }

    fn validate(self) -> Result<(), ColorManagementError> {
        self.red.validate()?;
        self.green.validate()?;
        self.blue.validate()?;
        Ok(())
    }

    fn to_lcms(self) -> lcms2::CIExyYTRIPLE {
        lcms2::CIExyYTRIPLE {
            Red: self.red.to_lcms_xyy(),
            Green: self.green.to_lcms_xyy(),
            Blue: self.blue.to_lcms_xyy(),
        }
    }

    fn rgb_to_xyz(self, white_point: Chromaticity) -> Result<[f32; 9], ColorManagementError> {
        let white = white_point.to_xyz();
        let red = self.red.to_xyz();
        let green = self.green.to_xyz();
        let blue = self.blue.to_xyz();
        let basis = [
            red[0], green[0], blue[0], red[1], green[1], blue[1], red[2], green[2], blue[2],
        ];
        let inverse_basis = invert_3x3(basis)?;
        let scales = mul_3x3_vec3(inverse_basis, white);
        Ok([
            (red[0] * scales[0]) as f32,
            (green[0] * scales[1]) as f32,
            (blue[0] * scales[2]) as f32,
            (red[1] * scales[0]) as f32,
            (green[1] * scales[1]) as f32,
            (blue[1] * scales[2]) as f32,
            (red[2] * scales[0]) as f32,
            (green[2] * scales[1]) as f32,
            (blue[2] * scales[2]) as f32,
        ])
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CustomRgbProfile {
    pub name: Arc<str>,
    pub white_point: Chromaticity,
    pub primaries: RgbPrimaries,
    pub transfer: SimpleTransferCurve,
}

impl CustomRgbProfile {
    pub fn new(
        name: impl Into<Arc<str>>,
        white_point: Chromaticity,
        primaries: RgbPrimaries,
        transfer: SimpleTransferCurve,
    ) -> Result<Self, ColorManagementError> {
        white_point.validate()?;
        primaries.validate()?;
        if let SimpleTransferCurve::Gamma(gamma) = transfer
            && (!gamma.is_finite() || gamma <= 0.0)
        {
            return Err(ColorManagementError::InvalidGamma(gamma));
        }
        Ok(Self {
            name: name.into(),
            white_point,
            primaries,
            transfer,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ColorProfile {
    Srgb,
    LinearSrgb,
    CustomRgb(CustomRgbProfile),
    Icc {
        name: Option<Arc<str>>,
        bytes: Arc<[u8]>,
    },
}

impl ColorProfile {
    pub const fn srgb() -> Self {
        Self::Srgb
    }

    pub const fn linear_srgb() -> Self {
        Self::LinearSrgb
    }

    pub fn custom_rgb(profile: CustomRgbProfile) -> Self {
        Self::CustomRgb(profile)
    }

    pub fn from_icc_bytes(
        name: Option<impl Into<Arc<str>>>,
        bytes: impl Into<Arc<[u8]>>,
    ) -> Result<Self, ColorManagementError> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return Err(ColorManagementError::EmptyIccProfile);
        }
        Profile::new_icc(&bytes).map_err(ColorManagementError::from)?;
        Ok(Self::Icc {
            name: name.map(Into::into),
            bytes,
        })
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Srgb => Some("sRGB"),
            Self::LinearSrgb => Some("Linear sRGB"),
            Self::CustomRgb(profile) => Some(profile.name.as_ref()),
            Self::Icc { name, .. } => name.as_deref(),
        }
    }

    pub fn icc_bytes(&self) -> Result<Vec<u8>, ColorManagementError> {
        match self {
            Self::Icc { bytes, .. } => Ok(bytes.to_vec()),
            _ => self
                .to_lcms_profile()?
                .icc()
                .map_err(ColorManagementError::from),
        }
    }

    pub fn gpu_color_space(&self) -> Result<GpuColorSpace, ColorManagementError> {
        match self {
            Self::Srgb => GpuColorSpace::from_components(
                RgbPrimaries::srgb(),
                Chromaticity::new(0.3127, 0.3290),
                GpuTransferCurve::Srgb,
            ),
            Self::LinearSrgb => GpuColorSpace::from_components(
                RgbPrimaries::srgb(),
                Chromaticity::new(0.3127, 0.3290),
                GpuTransferCurve::Linear,
            ),
            Self::CustomRgb(profile) => GpuColorSpace::from_components(
                profile.primaries,
                profile.white_point,
                profile.transfer.gpu_transfer(),
            ),
            Self::Icc { .. } => Err(ColorManagementError::GpuTransformUnavailable),
        }
    }

    fn to_lcms_profile(&self) -> Result<Profile, ColorManagementError> {
        match self {
            Self::Srgb => Ok(Profile::new_srgb()),
            Self::LinearSrgb => {
                let linear_curve = ToneCurve::new(1.0);
                Profile::new_rgb(
                    &Chromaticity::new(0.3127, 0.3290).to_lcms_xyy(),
                    &RgbPrimaries::srgb().to_lcms(),
                    &[&linear_curve, &linear_curve, &linear_curve],
                )
                .map_err(ColorManagementError::from)
            }
            Self::CustomRgb(profile) => {
                let curve = profile.transfer.build_tone_curve()?;
                Profile::new_rgb(
                    &profile.white_point.to_lcms_xyy(),
                    &profile.primaries.to_lcms(),
                    &[&curve, &curve, &curve],
                )
                .map_err(ColorManagementError::from)
            }
            Self::Icc { bytes, .. } => Profile::new_icc(bytes).map_err(ColorManagementError::from),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CpuTransformOptions {
    pub alpha_mode: AlphaMode,
    pub intent: RenderingIntent,
    pub black_point_compensation: bool,
    pub high_resolution_precalc: bool,
    pub no_cache: bool,
    pub no_optimize: bool,
}

impl Default for CpuTransformOptions {
    fn default() -> Self {
        Self {
            alpha_mode: AlphaMode::Straight,
            intent: RenderingIntent::RelativeColorimetric,
            black_point_compensation: true,
            high_resolution_precalc: true,
            no_cache: false,
            no_optimize: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CpuColorTransform {
    source: ColorProfile,
    destination: ColorProfile,
    options: CpuTransformOptions,
}

impl CpuColorTransform {
    pub fn new(
        source: ColorProfile,
        destination: ColorProfile,
        options: CpuTransformOptions,
    ) -> Self {
        Self {
            source,
            destination,
            options,
        }
    }

    pub fn source(&self) -> &ColorProfile {
        &self.source
    }

    pub fn destination(&self) -> &ColorProfile {
        &self.destination
    }

    pub fn options(&self) -> &CpuTransformOptions {
        &self.options
    }

    pub fn transform_in_place(&self, pixels: &mut [u8]) -> Result<(), ColorManagementError> {
        validate_rgba_slice(pixels.len())?;
        if pixels.is_empty() {
            return Ok(());
        }
        let source = self.source.to_lcms_profile()?;
        let destination = self.destination.to_lcms_profile()?;
        let flags = self.flags();
        let transform = if self.options.no_cache {
            Transform::<u8, u8, _, _>::new_flags(
                &source,
                PixelFormat::RGBA_8,
                &destination,
                PixelFormat::RGBA_8,
                self.options.intent.to_lcms(),
                flags | Flags::NO_CACHE,
            )
        } else {
            Transform::<u8, u8>::new_flags(
                &source,
                PixelFormat::RGBA_8,
                &destination,
                PixelFormat::RGBA_8,
                self.options.intent.to_lcms(),
                flags,
            )
        }
        .map_err(ColorManagementError::from)?;

        match self.options.alpha_mode {
            AlphaMode::Straight => transform.transform_in_place(pixels),
            AlphaMode::Premultiplied => {
                let mut scratch = pixels.to_vec();
                unpremultiply_rgba8_in_place(&mut scratch);
                transform.transform_in_place(&mut scratch);
                premultiply_rgba8_in_place(&mut scratch);
                pixels.copy_from_slice(&scratch);
            }
        }

        Ok(())
    }

    pub fn transform_to(
        &self,
        source_pixels: &[u8],
        destination_pixels: &mut [u8],
    ) -> Result<(), ColorManagementError> {
        validate_rgba_slice(source_pixels.len())?;
        validate_rgba_slice(destination_pixels.len())?;
        if source_pixels.len() != destination_pixels.len() {
            return Err(ColorManagementError::MismatchedPixelBufferLengths {
                source: source_pixels.len(),
                destination: destination_pixels.len(),
            });
        }
        destination_pixels.copy_from_slice(source_pixels);
        self.transform_in_place(destination_pixels)
    }

    fn flags(&self) -> Flags {
        let mut flags = Flags::COPY_ALPHA;
        if self.options.black_point_compensation {
            flags = flags | Flags::BLACKPOINT_COMPENSATION;
        }
        if self.options.high_resolution_precalc {
            flags = flags | Flags::HIGHRES_PRECALC;
        }
        if self.options.no_optimize {
            flags = flags | Flags::NO_OPTIMIZE;
        }
        flags
    }
}

#[derive(Clone, Debug)]
pub struct ColorManagement {
    working_profile: ColorProfile,
}

impl ColorManagement {
    pub fn new(working_profile: ColorProfile) -> Self {
        Self { working_profile }
    }

    pub fn working_profile(&self) -> &ColorProfile {
        &self.working_profile
    }

    pub fn import_transform(
        &self,
        source_profile: ColorProfile,
        options: CpuTransformOptions,
    ) -> CpuColorTransform {
        CpuColorTransform::new(source_profile, self.working_profile.clone(), options)
    }

    pub fn export_transform(
        &self,
        destination_profile: ColorProfile,
        options: CpuTransformOptions,
    ) -> CpuColorTransform {
        CpuColorTransform::new(self.working_profile.clone(), destination_profile, options)
    }

    pub fn display_transform(
        &self,
        destination_profile: &ColorProfile,
    ) -> Result<GpuColorTransform, ColorManagementError> {
        GpuColorTransform::new(
            self.working_profile.gpu_color_space()?,
            destination_profile.gpu_color_space()?,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GpuTransferCurve {
    Linear,
    Srgb,
    Gamma(f32),
}

impl GpuTransferCurve {
    fn kind_code(self) -> u32 {
        match self {
            Self::Linear => 0,
            Self::Srgb => 1,
            Self::Gamma(_) => 2,
        }
    }

    fn gamma(self) -> f32 {
        match self {
            Self::Linear | Self::Srgb => 1.0,
            Self::Gamma(gamma) => gamma,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuColorSpace {
    pub rgb_to_xyz: [f32; 9],
    pub xyz_to_rgb: [f32; 9],
    pub transfer: GpuTransferCurve,
}

impl GpuColorSpace {
    fn from_components(
        primaries: RgbPrimaries,
        white_point: Chromaticity,
        transfer: GpuTransferCurve,
    ) -> Result<Self, ColorManagementError> {
        let rgb_to_xyz = primaries.rgb_to_xyz(white_point)?;
        let xyz_to_rgb = invert_3x3_f32(rgb_to_xyz)?;
        Ok(Self {
            rgb_to_xyz,
            xyz_to_rgb,
            transfer,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuColorTransform {
    pub source_rgb_to_xyz: [f32; 9],
    pub destination_xyz_to_rgb: [f32; 9],
    pub source_transfer: GpuTransferCurve,
    pub destination_transfer: GpuTransferCurve,
}

impl GpuColorTransform {
    pub fn new(
        source: GpuColorSpace,
        destination: GpuColorSpace,
    ) -> Result<Self, ColorManagementError> {
        Ok(Self {
            source_rgb_to_xyz: source.rgb_to_xyz,
            destination_xyz_to_rgb: destination.xyz_to_rgb,
            source_transfer: source.transfer,
            destination_transfer: destination.transfer,
        })
    }

    pub fn uniform(self) -> GpuColorTransformUniform {
        GpuColorTransformUniform {
            source_rgb_to_xyz: mat3_to_std140(self.source_rgb_to_xyz),
            destination_xyz_to_rgb: mat3_to_std140(self.destination_xyz_to_rgb),
            source_transfer_kind: self.source_transfer.kind_code(),
            destination_transfer_kind: self.destination_transfer.kind_code(),
            source_gamma: self.source_transfer.gamma(),
            destination_gamma: self.destination_transfer.gamma(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct GpuColorTransformUniform {
    pub source_rgb_to_xyz: [[f32; 4]; 3],
    pub destination_xyz_to_rgb: [[f32; 4]; 3],
    pub source_transfer_kind: u32,
    pub destination_transfer_kind: u32,
    pub source_gamma: f32,
    pub destination_gamma: f32,
}

#[derive(Debug)]
pub enum ColorManagementError {
    EmptyIccProfile,
    InvalidChromaticity(Chromaticity),
    InvalidGamma(f64),
    GpuTransformUnavailable,
    MismatchedPixelBufferLengths { source: usize, destination: usize },
    NonRgba8PixelBuffer { len: usize },
    NonInvertibleMatrix,
    Lcms(lcms2::Error),
}

impl fmt::Display for ColorManagementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyIccProfile => write!(f, "ICC profile bytes are empty"),
            Self::InvalidChromaticity(value) => {
                write!(f, "invalid chromaticity x={} y={}", value.x, value.y)
            }
            Self::InvalidGamma(gamma) => write!(f, "invalid gamma value {gamma}"),
            Self::GpuTransformUnavailable => {
                write!(
                    f,
                    "profile cannot be represented as a GPU-side matrix/curve transform"
                )
            }
            Self::MismatchedPixelBufferLengths {
                source,
                destination,
            } => write!(
                f,
                "source and destination pixel buffer lengths differ: {source} vs {destination}"
            ),
            Self::NonRgba8PixelBuffer { len } => {
                write!(
                    f,
                    "pixel buffer length {len} is not a multiple of 4 RGBA8 bytes"
                )
            }
            Self::NonInvertibleMatrix => write!(f, "RGB color space matrix is non-invertible"),
            Self::Lcms(error) => write!(f, "littleCMS error: {error}"),
        }
    }
}

impl std::error::Error for ColorManagementError {}

impl From<lcms2::Error> for ColorManagementError {
    fn from(value: lcms2::Error) -> Self {
        Self::Lcms(value)
    }
}

fn validate_rgba_slice(len: usize) -> Result<(), ColorManagementError> {
    if len % 4 == 0 {
        Ok(())
    } else {
        Err(ColorManagementError::NonRgba8PixelBuffer { len })
    }
}

fn unpremultiply_rgba8_in_place(pixels: &mut [u8]) {
    for rgba in pixels.chunks_exact_mut(4) {
        let alpha = rgba[3] as f32 / 255.0;
        if alpha <= 0.0 {
            rgba[0] = 0;
            rgba[1] = 0;
            rgba[2] = 0;
            continue;
        }
        rgba[0] = ((rgba[0] as f32 / alpha).clamp(0.0, 255.0)).round() as u8;
        rgba[1] = ((rgba[1] as f32 / alpha).clamp(0.0, 255.0)).round() as u8;
        rgba[2] = ((rgba[2] as f32 / alpha).clamp(0.0, 255.0)).round() as u8;
    }
}

fn premultiply_rgba8_in_place(pixels: &mut [u8]) {
    for rgba in pixels.chunks_exact_mut(4) {
        let alpha = rgba[3] as f32 / 255.0;
        rgba[0] = ((rgba[0] as f32 * alpha).clamp(0.0, 255.0)).round() as u8;
        rgba[1] = ((rgba[1] as f32 * alpha).clamp(0.0, 255.0)).round() as u8;
        rgba[2] = ((rgba[2] as f32 * alpha).clamp(0.0, 255.0)).round() as u8;
    }
}

fn mat3_to_std140(matrix: [f32; 9]) -> [[f32; 4]; 3] {
    [
        [matrix[0], matrix[3], matrix[6], 0.0],
        [matrix[1], matrix[4], matrix[7], 0.0],
        [matrix[2], matrix[5], matrix[8], 0.0],
    ]
}

fn invert_3x3_f32(matrix: [f32; 9]) -> Result<[f32; 9], ColorManagementError> {
    let matrix64 = [
        matrix[0] as f64,
        matrix[1] as f64,
        matrix[2] as f64,
        matrix[3] as f64,
        matrix[4] as f64,
        matrix[5] as f64,
        matrix[6] as f64,
        matrix[7] as f64,
        matrix[8] as f64,
    ];
    invert_3x3(matrix64).map(|value| {
        [
            value[0] as f32,
            value[1] as f32,
            value[2] as f32,
            value[3] as f32,
            value[4] as f32,
            value[5] as f32,
            value[6] as f32,
            value[7] as f32,
            value[8] as f32,
        ]
    })
}

fn invert_3x3(matrix: [f64; 9]) -> Result<[f64; 9], ColorManagementError> {
    let det = matrix[0] * (matrix[4] * matrix[8] - matrix[5] * matrix[7])
        - matrix[1] * (matrix[3] * matrix[8] - matrix[5] * matrix[6])
        + matrix[2] * (matrix[3] * matrix[7] - matrix[4] * matrix[6]);

    if det.abs() <= 1e-12 {
        return Err(ColorManagementError::NonInvertibleMatrix);
    }

    let inv_det = 1.0 / det;
    Ok([
        (matrix[4] * matrix[8] - matrix[5] * matrix[7]) * inv_det,
        (matrix[2] * matrix[7] - matrix[1] * matrix[8]) * inv_det,
        (matrix[1] * matrix[5] - matrix[2] * matrix[4]) * inv_det,
        (matrix[5] * matrix[6] - matrix[3] * matrix[8]) * inv_det,
        (matrix[0] * matrix[8] - matrix[2] * matrix[6]) * inv_det,
        (matrix[2] * matrix[3] - matrix[0] * matrix[5]) * inv_det,
        (matrix[3] * matrix[7] - matrix[4] * matrix[6]) * inv_det,
        (matrix[1] * matrix[6] - matrix[0] * matrix[7]) * inv_det,
        (matrix[0] * matrix[4] - matrix[1] * matrix[3]) * inv_det,
    ])
}

fn mul_3x3_vec3(matrix: [f64; 9], vector: [f64; 3]) -> [f64; 3] {
    [
        matrix[0] * vector[0] + matrix[1] * vector[1] + matrix[2] * vector[2],
        matrix[3] * vector[0] + matrix[4] * vector[1] + matrix[5] * vector[2],
        matrix[6] * vector[0] + matrix[7] * vector[1] + matrix[8] * vector[2],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_to_linear_srgb_changes_midtones() {
        let transform = CpuColorTransform::new(
            ColorProfile::srgb(),
            ColorProfile::linear_srgb(),
            CpuTransformOptions::default(),
        );
        let mut pixels = [128, 128, 128, 255];
        transform.transform_in_place(&mut pixels).unwrap();
        assert!(pixels[0] < 80);
        assert_eq!(pixels[3], 255);
    }

    #[test]
    fn premultiplied_alpha_is_preserved_across_identity_transform() {
        let transform = CpuColorTransform::new(
            ColorProfile::linear_srgb(),
            ColorProfile::linear_srgb(),
            CpuTransformOptions {
                alpha_mode: AlphaMode::Premultiplied,
                ..Default::default()
            },
        );
        let mut pixels = [64, 32, 16, 128];
        transform.transform_in_place(&mut pixels).unwrap();
        assert_eq!(pixels, [64, 32, 16, 128]);
    }

    #[test]
    fn gpu_transform_is_identity_for_linear_to_srgb_primaries() {
        let cms = ColorManagement::new(ColorProfile::linear_srgb());
        let transform = cms.display_transform(&ColorProfile::srgb()).unwrap();
        let uniform = transform.uniform();
        assert_eq!(
            uniform.source_transfer_kind,
            GpuTransferCurve::Linear.kind_code()
        );
        assert_eq!(
            uniform.destination_transfer_kind,
            GpuTransferCurve::Srgb.kind_code()
        );
        assert!((uniform.source_rgb_to_xyz[0][0] - 0.4123908).abs() < 1e-4);
    }

    #[test]
    fn custom_rgb_profile_round_trips_to_icc() {
        let profile = ColorProfile::custom_rgb(
            CustomRgbProfile::new(
                "Test RGB",
                Chromaticity::new(0.3127, 0.3290),
                RgbPrimaries::srgb(),
                SimpleTransferCurve::Gamma(2.2),
            )
            .unwrap(),
        );

        let icc_bytes = profile.icc_bytes().unwrap();
        let reparsed = ColorProfile::from_icc_bytes(Some("Roundtrip"), icc_bytes).unwrap();
        assert!(matches!(reparsed, ColorProfile::Icc { .. }));
    }
}
