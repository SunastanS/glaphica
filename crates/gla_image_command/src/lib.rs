use atlas::TilePos;
use gla_color::BlendMode;
use gla_command_core::{FootprintModifier, Mapping};
use gla_core::TileGridError;
use gla_image::{GlaImageLayout, ImageError, ImageLayoutError};
use std::error::Error;
use std::fmt::{Display, Formatter};
use tile_key::TileReadRef;

pub trait RenderCtx {
    type ImageKey: std::marker::Copy;
    type Error;

    fn render(
        &mut self,
        image: Self::ImageKey,
        tile_index: u32,
    ) -> Result<TileReadRef, Self::Error>;
    fn write_pos(&mut self, image: Self::ImageKey, tile_index: u32)
    -> Result<TilePos, Self::Error>;
    fn clear(&mut self, dst: TilePos);
    fn copy(&mut self, src: TilePos, dst: TilePos);
    fn render_to(&mut self, src: TilePos, dst: TilePos, blend_mode: BlendMode, opacity: f32);
    fn fix_gutter(&mut self, dst: TilePos);
    fn footprint_error(&mut self, source: SourceFootprintError) -> Self::Error;
    fn unsupported_zero_source_render_to(&mut self, blend_mode: BlendMode) -> Self::Error;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SourceFootprintError {
    Unsupported {
        mapping: Mapping,
        modifier: FootprintModifier,
    },
    TileGrid {
        source: TileGridError,
    },
    ImageLayout {
        source: ImageLayoutError,
    },
    TileIndexOutOfBounds {
        tile_index: u32,
        tile_count: u32,
    },
}

impl Display for SourceFootprintError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported { mapping, modifier } => write!(
                f,
                "unsupported source footprint mapping {mapping:?} modifier {modifier:?}"
            ),
            Self::TileGrid { source } => write!(f, "invalid source footprint grid: {source}"),
            Self::ImageLayout { source } => {
                write!(f, "invalid source footprint image layout: {source}")
            }
            Self::TileIndexOutOfBounds {
                tile_index,
                tile_count,
            } => write!(
                f,
                "source footprint tile index {tile_index} out of bounds for image with {tile_count} tiles"
            ),
        }
    }
}

impl Error for SourceFootprintError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::TileGrid { source } => Some(source),
            Self::ImageLayout { source } => Some(source),
            _ => None,
        }
    }
}

impl SourceFootprintError {
    fn from_image_error(source: ImageError) -> Self {
        match source {
            ImageError::InvalidLayout { source } => Self::ImageLayout { source },
            ImageError::TileIndexOutOfBounds {
                tile_index,
                tile_count,
            } => Self::TileIndexOutOfBounds {
                tile_index,
                tile_count,
            },
            ImageError::TileAllocFailed { .. } => {
                unreachable!("tile footprint validation does not allocate image tiles")
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageRef<K> {
    pub key: K,
    pub layout: GlaImageLayout,
    pub mapping: Mapping,
    pub modifier: FootprintModifier,
}

impl<K> ImageRef<K> {
    pub fn new(key: K, layout: GlaImageLayout) -> Self {
        Self {
            key,
            layout,
            mapping: Mapping::Identity,
            modifier: FootprintModifier::None,
        }
    }

    pub fn with_footprint(
        key: K,
        layout: GlaImageLayout,
        mapping: Mapping,
        modifier: FootprintModifier,
    ) -> Self {
        Self {
            key,
            layout,
            mapping,
            modifier,
        }
    }

    fn source_tiles(
        self,
        dst_layout: GlaImageLayout,
        tile_index: u32,
    ) -> Result<Vec<u32>, SourceFootprintError> {
        match (self.mapping, self.modifier) {
            (Mapping::Identity, FootprintModifier::None) => {
                let dst_tile_index = dst_layout
                    .tile_index(tile_index)
                    .map_err(SourceFootprintError::from_image_error)?;
                let dst_rect = dst_layout
                    .tile_rect(dst_tile_index)
                    .map_err(|source| SourceFootprintError::TileGrid { source })?;
                let source_tiles = self
                    .layout
                    .tile_set_covering_rect(dst_rect)
                    .map_err(|source| SourceFootprintError::TileGrid { source })?;
                Ok(source_tiles
                    .tile_indices()
                    .expect("tiles covering a rect are represented as explicit tile indices")
                    .iter()
                    .map(|tile| tile.value())
                    .collect())
            }
            (mapping, modifier) => Err(SourceFootprintError::Unsupported { mapping, modifier }),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeriveCommand<K> {
    pub dst: K,
    pub layout: GlaImageLayout,
    pub ops: Box<[Derive<K>]>,
}

impl<K> DeriveCommand<K> {
    pub fn new(dst: K, layout: GlaImageLayout, ops: impl Into<Box<[Derive<K>]>>) -> Self {
        Self {
            dst,
            layout,
            ops: ops.into(),
        }
    }

    pub fn exec_tile<C>(&self, ctx: &mut C, tile_index: u32) -> Result<(), C::Error>
    where
        C: RenderCtx<ImageKey = K>,
        K: std::marker::Copy,
    {
        let dst = ctx.write_pos(self.dst, tile_index)?;
        for op in self.ops.iter().copied() {
            op.exec_tile(ctx, self.layout, dst, tile_index)?;
        }
        ctx.fix_gutter(dst);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Derive<K> {
    Copy(Copy<K>),
    Clear(Clear),
    RenderTo(RenderTo<K>),
}

impl<K> Derive<K> {
    pub fn exec_tile<C>(
        self,
        ctx: &mut C,
        dst_layout: GlaImageLayout,
        dst: TilePos,
        tile_index: u32,
    ) -> Result<(), C::Error>
    where
        C: RenderCtx<ImageKey = K>,
        K: std::marker::Copy,
    {
        match self {
            Self::Copy(op) => op.exec_tile(ctx, dst_layout, dst, tile_index),
            Self::Clear(op) => op.exec_tile(ctx, dst),
            Self::RenderTo(op) => op.exec_tile(ctx, dst_layout, dst, tile_index),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Copy<K> {
    pub src: ImageRef<K>,
}

impl<K> Copy<K> {
    pub fn new(src: ImageRef<K>) -> Self {
        Self { src }
    }

    pub fn exec_tile<C>(
        self,
        ctx: &mut C,
        dst_layout: GlaImageLayout,
        dst: TilePos,
        tile_index: u32,
    ) -> Result<(), C::Error>
    where
        C: RenderCtx<ImageKey = K>,
        K: std::marker::Copy,
    {
        let source_tiles = self
            .src
            .source_tiles(dst_layout, tile_index)
            .map_err(|source| ctx.footprint_error(source))?;
        for source_tile_index in source_tiles {
            let src = ctx.render(self.src.key, source_tile_index)?;
            match src {
                TileReadRef::Zero => ctx.clear(dst),
                TileReadRef::Physical(src) => ctx.copy(src, dst),
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clear;

impl Clear {
    pub fn exec_tile<C>(self, ctx: &mut C, dst: TilePos) -> Result<(), C::Error>
    where
        C: RenderCtx,
    {
        ctx.clear(dst);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderTo<K> {
    pub src: ImageRef<K>,
    pub blend_mode: BlendMode,
    pub opacity: f32,
}

impl<K> RenderTo<K> {
    pub fn new(src: ImageRef<K>, blend_mode: BlendMode, opacity: f32) -> Self {
        Self {
            src,
            blend_mode,
            opacity,
        }
    }

    pub fn exec_tile<C>(
        self,
        ctx: &mut C,
        dst_layout: GlaImageLayout,
        dst: TilePos,
        tile_index: u32,
    ) -> Result<(), C::Error>
    where
        C: RenderCtx<ImageKey = K>,
        K: std::marker::Copy,
    {
        let source_tiles = self
            .src
            .source_tiles(dst_layout, tile_index)
            .map_err(|source| ctx.footprint_error(source))?;
        for source_tile_index in source_tiles {
            let src = ctx.render(self.src.key, source_tile_index)?;
            match src {
                TileReadRef::Zero => {
                    return Err(ctx.unsupported_zero_source_render_to(self.blend_mode));
                }
                TileReadRef::Physical(src) => {
                    ctx.render_to(src, dst, self.blend_mode, self.opacity);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas::TilePos;
    use gla_renderer::Pass;

    #[derive(Debug)]
    enum TestError {
        MissingReturn,
        Footprint(SourceFootprintError),
        UnsupportedZeroSourceRenderTo(BlendMode),
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    struct TestImageKey(u32);

    struct TestCtx {
        passes: Vec<Pass>,
        calls: Vec<(TestImageKey, u32)>,
        write_calls: Vec<(TestImageKey, u32)>,
        returns: Vec<TileReadRef>,
        dst_pos: Option<TilePos>,
        nested_pass: Option<TilePos>,
    }

    impl RenderCtx for TestCtx {
        type ImageKey = TestImageKey;
        type Error = TestError;

        fn render(
            &mut self,
            image: TestImageKey,
            tile_index: u32,
        ) -> Result<TileReadRef, Self::Error> {
            self.calls.push((image, tile_index));
            if let Some(dst) = self.nested_pass {
                self.clear(dst);
            }
            self.returns.pop().ok_or(TestError::MissingReturn)
        }

        fn write_pos(
            &mut self,
            image: TestImageKey,
            tile_index: u32,
        ) -> Result<TilePos, Self::Error> {
            self.write_calls.push((image, tile_index));
            self.dst_pos.ok_or(TestError::MissingReturn)
        }

        fn clear(&mut self, dst: TilePos) {
            self.passes.push(Pass::Clear { dst });
        }

        fn copy(&mut self, src: TilePos, dst: TilePos) {
            self.passes.push(Pass::Copy { src, dst });
        }

        fn render_to(&mut self, src: TilePos, dst: TilePos, blend_mode: BlendMode, opacity: f32) {
            self.passes.push(Pass::RenderTo {
                src,
                dst,
                blend_mode,
                opacity,
            });
        }

        fn fix_gutter(&mut self, dst: TilePos) {
            self.passes.push(Pass::FixGutter { dst });
        }

        fn footprint_error(&mut self, source: SourceFootprintError) -> Self::Error {
            TestError::Footprint(source)
        }

        fn unsupported_zero_source_render_to(&mut self, blend_mode: BlendMode) -> Self::Error {
            TestError::UnsupportedZeroSourceRenderTo(blend_mode)
        }
    }

    fn ctx_with_tiles(returns: Vec<TileReadRef>) -> TestCtx {
        TestCtx {
            passes: Vec::new(),
            calls: Vec::new(),
            write_calls: Vec::new(),
            returns,
            dst_pos: None,
            nested_pass: None,
        }
    }

    fn layout() -> GlaImageLayout {
        GlaImageLayout::new(4096, 4096).unwrap()
    }

    fn layout_with_tiles(width_tiles: u32, height_tiles: u32) -> GlaImageLayout {
        GlaImageLayout::new(
            width_tiles * gla_image::IMAGE_TILE_SIZE,
            height_tiles * gla_image::IMAGE_TILE_SIZE,
        )
        .unwrap()
    }

    #[test]
    fn copy_renders_source_before_recording_copy_pass() {
        let mut ctx = ctx_with_tiles(Vec::new());
        let nested_dst = TilePos::new(0, 0, 6, 0);
        let source_pos = TilePos::new(0, 0, 7, 0);
        let dst_pos = TilePos::new(0, 0, 8, 0);
        ctx.returns.push(TileReadRef::Physical(source_pos));
        ctx.dst_pos = Some(dst_pos);
        ctx.nested_pass = Some(nested_dst);

        let source_image = TestImageKey(7);
        let dst_image = TestImageKey(8);
        let command = DeriveCommand::new(
            dst_image,
            layout(),
            [Derive::Copy(Copy::new(ImageRef::new(
                source_image,
                layout(),
            )))],
        );

        command.exec_tile(&mut ctx, 3).unwrap();

        assert_eq!(ctx.calls, vec![(source_image, 3)]);
        assert_eq!(ctx.write_calls, vec![(dst_image, 3)]);
        assert_eq!(
            ctx.passes,
            vec![
                Pass::Clear { dst: nested_dst },
                Pass::Copy {
                    src: source_pos,
                    dst: dst_pos,
                },
                Pass::FixGutter { dst: dst_pos },
            ]
        );
    }

    #[test]
    fn render_to_records_blend_mode_and_opacity() {
        let mut ctx = ctx_with_tiles(Vec::new());
        let source_pos = TilePos::new(0, 0, 9, 0);
        let dst_pos = TilePos::new(0, 0, 10, 0);
        ctx.returns.push(TileReadRef::Physical(source_pos));
        ctx.dst_pos = Some(dst_pos);

        let source_image = TestImageKey(9);
        let dst_image = TestImageKey(10);
        let command = DeriveCommand::new(
            dst_image,
            layout(),
            [Derive::RenderTo(RenderTo::new(
                ImageRef::new(source_image, layout()),
                BlendMode::Overlay,
                0.25,
            ))],
        );

        command.exec_tile(&mut ctx, 11).unwrap();

        assert_eq!(ctx.calls, vec![(source_image, 11)]);
        assert_eq!(ctx.write_calls, vec![(dst_image, 11)]);
        assert_eq!(
            ctx.passes,
            vec![
                Pass::RenderTo {
                    src: source_pos,
                    dst: dst_pos,
                    blend_mode: BlendMode::Overlay,
                    opacity: 0.25,
                },
                Pass::FixGutter { dst: dst_pos },
            ]
        );
    }

    #[test]
    fn render_to_records_normal_blend_mode() {
        let mut ctx = ctx_with_tiles(Vec::new());
        let source_pos = TilePos::new(0, 0, 17, 0);
        let dst_pos = TilePos::new(0, 0, 18, 0);
        ctx.returns.push(TileReadRef::Physical(source_pos));
        ctx.dst_pos = Some(dst_pos);

        let command = DeriveCommand::new(
            TestImageKey(18),
            layout(),
            [Derive::RenderTo(RenderTo::new(
                ImageRef::new(TestImageKey(17), layout()),
                BlendMode::Normal,
                1.0,
            ))],
        );

        command.exec_tile(&mut ctx, 4).unwrap();

        assert_eq!(
            ctx.passes,
            vec![
                Pass::RenderTo {
                    src: source_pos,
                    dst: dst_pos,
                    blend_mode: BlendMode::Normal,
                    opacity: 1.0,
                },
                Pass::FixGutter { dst: dst_pos },
            ]
        );
    }

    #[test]
    fn render_to_records_value_to_rgba_mask_mode() {
        let mut ctx = ctx_with_tiles(Vec::new());
        let source_pos = TilePos::new(0, 0, 13, 0);
        let dst_pos = TilePos::new(0, 0, 14, 0);
        ctx.returns.push(TileReadRef::Physical(source_pos));
        ctx.dst_pos = Some(dst_pos);

        let source_image = TestImageKey(13);
        let dst_image = TestImageKey(14);
        let command = DeriveCommand::new(
            dst_image,
            layout(),
            [Derive::RenderTo(RenderTo::new(
                ImageRef::new(source_image, layout()),
                BlendMode::MaskAlpha,
                0.75,
            ))],
        );

        command.exec_tile(&mut ctx, 4).unwrap();

        assert_eq!(
            ctx.passes,
            vec![
                Pass::RenderTo {
                    src: source_pos,
                    dst: dst_pos,
                    blend_mode: BlendMode::MaskAlpha,
                    opacity: 0.75,
                },
                Pass::FixGutter { dst: dst_pos },
            ]
        );
    }

    #[test]
    fn clear_writes_without_rendering_source() {
        let mut ctx = ctx_with_tiles(Vec::new());
        let dst_pos = TilePos::new(0, 0, 12, 0);
        ctx.dst_pos = Some(dst_pos);
        let dst_image = TestImageKey(12);
        let command = DeriveCommand::new(dst_image, layout(), [Derive::Clear(Clear)]);

        command.exec_tile(&mut ctx, 5).unwrap();

        assert!(ctx.calls.is_empty());
        assert_eq!(ctx.write_calls, vec![(dst_image, 5)]);
        assert_eq!(
            ctx.passes,
            vec![
                Pass::Clear { dst: dst_pos },
                Pass::FixGutter { dst: dst_pos }
            ]
        );
    }

    #[test]
    fn copy_zero_source_records_clear() {
        let mut ctx = ctx_with_tiles(vec![TileReadRef::Zero]);
        let dst_pos = TilePos::new(0, 0, 15, 0);
        ctx.dst_pos = Some(dst_pos);

        let source_image = TestImageKey(15);
        let dst_image = TestImageKey(16);
        let command = DeriveCommand::new(
            dst_image,
            layout(),
            [Derive::Copy(Copy::new(ImageRef::new(
                source_image,
                layout(),
            )))],
        );

        command.exec_tile(&mut ctx, 2).unwrap();

        assert_eq!(
            ctx.passes,
            vec![
                Pass::Clear { dst: dst_pos },
                Pass::FixGutter { dst: dst_pos }
            ]
        );
    }

    #[test]
    fn copy_identity_uses_layout_aware_source_tiles() {
        let dst_pos = TilePos::new(0, 0, 23, 0);
        let source_pos = TilePos::new(0, 0, 21, 0);
        let mut ctx = ctx_with_tiles(vec![TileReadRef::Physical(source_pos)]);
        ctx.dst_pos = Some(dst_pos);

        let source_image = TestImageKey(21);
        let dst_image = TestImageKey(22);
        let command = DeriveCommand::new(
            dst_image,
            layout_with_tiles(1, 2),
            [Derive::Copy(Copy::new(ImageRef::new(
                source_image,
                layout_with_tiles(2, 2),
            )))],
        );

        command.exec_tile(&mut ctx, 1).unwrap();

        assert_eq!(ctx.calls, vec![(source_image, 2)]);
        assert_eq!(
            ctx.passes,
            vec![
                Pass::Copy {
                    src: source_pos,
                    dst: dst_pos,
                },
                Pass::FixGutter { dst: dst_pos },
            ]
        );
    }

    #[test]
    fn matrix_footprint_returns_explicit_error() {
        let mut ctx = ctx_with_tiles(Vec::new());
        ctx.dst_pos = Some(TilePos::new(0, 0, 24, 0));
        let mapping = Mapping::Matrix(gla_command_core::Affine2D {
            m11: 1.0,
            m12: 0.0,
            m21: 0.0,
            m22: 1.0,
            tx: 2.0,
            ty: 0.0,
        });
        let command = DeriveCommand::new(
            TestImageKey(25),
            layout(),
            [Derive::Copy(Copy::new(ImageRef::with_footprint(
                TestImageKey(24),
                layout(),
                mapping,
                FootprintModifier::None,
            )))],
        );

        let err = command.exec_tile(&mut ctx, 0).unwrap_err();

        assert!(matches!(
            err,
            TestError::Footprint(SourceFootprintError::Unsupported {
                mapping: err_mapping,
                modifier: FootprintModifier::None,
            }) if err_mapping == mapping
        ));
    }

    #[test]
    fn render_to_zero_source_returns_explicit_error() {
        let mut ctx = ctx_with_tiles(vec![TileReadRef::Zero]);
        ctx.dst_pos = Some(TilePos::new(0, 0, 26, 0));
        let command = DeriveCommand::new(
            TestImageKey(26),
            layout(),
            [Derive::RenderTo(RenderTo::new(
                ImageRef::new(TestImageKey(27), layout()),
                BlendMode::Overlay,
                0.5,
            ))],
        );

        let err = command.exec_tile(&mut ctx, 0).unwrap_err();

        assert!(matches!(
            err,
            TestError::UnsupportedZeroSourceRenderTo(BlendMode::Overlay)
        ));
    }
}
