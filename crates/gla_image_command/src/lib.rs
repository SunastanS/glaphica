use atlas::TilePos;
use gla_color::BlendMode;
use gla_command_core::{FootprintModifier, Mapping};
use gla_image::GlaImageLayout;
use gla_renderer::Renderer;
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
    fn renderer(&mut self) -> &mut Renderer;
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

    fn for_each_source_tile<E>(
        self,
        dst_layout: GlaImageLayout,
        tile_index: u32,
        mut f: impl FnMut(u32) -> Result<(), E>,
    ) -> Result<(), E> {
        debug_assert!(tile_index < dst_layout.tile_count());
        debug_assert!(tile_index < self.layout.tile_count());
        match (self.mapping, self.modifier) {
            (Mapping::Identity, FootprintModifier::None) => f(tile_index),
            (Mapping::Identity, FootprintModifier::Expand(_)) => {
                todo!("expanded identity footprints need image layouts")
            }
            (Mapping::Matrix(_), _) => todo!("matrix footprints need image layouts"),
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
        ctx.renderer().fix_gutter(dst);
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
        self.src
            .for_each_source_tile(dst_layout, tile_index, |source_tile_index| {
                let src = ctx.render(self.src.key, source_tile_index)?;
                match src {
                    TileReadRef::Zero => ctx.renderer().clear(dst),
                    TileReadRef::Physical(src) => ctx.renderer().copy(src, dst),
                }
                Ok(())
            })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clear;

impl Clear {
    pub fn exec_tile<C>(self, ctx: &mut C, dst: TilePos) -> Result<(), C::Error>
    where
        C: RenderCtx,
    {
        ctx.renderer().clear(dst);
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
        self.src
            .for_each_source_tile(dst_layout, tile_index, |source_tile_index| {
                let src = ctx.render(self.src.key, source_tile_index)?;
                match src {
                    TileReadRef::Zero => {
                        // The first materialized command seam only defines zero-source
                        // semantics for copy. Composite zero-source behavior is operation
                        // specific and must be made explicit before execution reaches here.
                        todo!("zero-source RenderTo semantics are operation-specific")
                    }
                    TileReadRef::Physical(src) => {
                        ctx.renderer()
                            .render_to(src, dst, self.blend_mode, self.opacity);
                    }
                }
                Ok(())
            })
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
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    struct TestImageKey(u32);

    struct TestCtx {
        renderer: Renderer,
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
                self.renderer.clear(dst);
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

        fn renderer(&mut self) -> &mut Renderer {
            &mut self.renderer
        }
    }

    fn ctx_with_tiles(returns: Vec<TileReadRef>) -> TestCtx {
        TestCtx {
            renderer: Renderer::new(),
            calls: Vec::new(),
            write_calls: Vec::new(),
            returns,
            dst_pos: None,
            nested_pass: None,
        }
    }

    fn layout() -> GlaImageLayout {
        GlaImageLayout::new(4096, 4096)
    }

    #[test]
    fn copy_renders_source_before_recording_copy_pass() {
        let mut ctx = ctx_with_tiles(Vec::new());
        let nested_dst = TilePos::new(0, 6);
        let source_pos = TilePos::new(0, 7);
        let dst_pos = TilePos::new(0, 8);
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
            ctx.renderer.passes(),
            &[
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
        let source_pos = TilePos::new(0, 9);
        let dst_pos = TilePos::new(0, 10);
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
            ctx.renderer.passes(),
            &[
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
    fn render_to_records_value_to_rgba_mask_mode() {
        let mut ctx = ctx_with_tiles(Vec::new());
        let source_pos = TilePos::new(0, 13);
        let dst_pos = TilePos::new(0, 14);
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
            ctx.renderer.passes(),
            &[
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
        let dst_pos = TilePos::new(0, 12);
        ctx.dst_pos = Some(dst_pos);
        let dst_image = TestImageKey(12);
        let command = DeriveCommand::new(dst_image, layout(), [Derive::Clear(Clear)]);

        command.exec_tile(&mut ctx, 5).unwrap();

        assert!(ctx.calls.is_empty());
        assert_eq!(ctx.write_calls, vec![(dst_image, 5)]);
        assert_eq!(
            ctx.renderer.passes(),
            &[
                Pass::Clear { dst: dst_pos },
                Pass::FixGutter { dst: dst_pos }
            ]
        );
    }

    #[test]
    fn copy_zero_source_records_clear() {
        let mut ctx = ctx_with_tiles(vec![TileReadRef::Zero]);
        let dst_pos = TilePos::new(0, 15);
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
            ctx.renderer.passes(),
            &[
                Pass::Clear { dst: dst_pos },
                Pass::FixGutter { dst: dst_pos }
            ]
        );
    }
}
