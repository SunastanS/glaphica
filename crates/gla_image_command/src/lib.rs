use atlas::TilePos;
use gla_color::BlendMode;
use gla_command_core::{FootprintModifier, Mapping};
use gla_image::{GlaImageKey, GlaImageLayout};
use gla_renderer::Renderer;
use tile_key::TileKey;

pub trait RenderCtx {
    type Error;

    fn render(&mut self, image: GlaImageKey, tile_index: u32) -> Result<TileKey, Self::Error>;
    fn write_tile(&mut self, image: GlaImageKey, tile_index: u32) -> Result<TileKey, Self::Error>;
    fn acquire_for_read(&mut self, key: TileKey) -> Result<TilePos, Self::Error>;
    fn acquire_for_write(&mut self, key: TileKey) -> Result<TilePos, Self::Error>;
    fn renderer(&mut self) -> &mut Renderer;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageRef {
    pub key: GlaImageKey,
    pub layout: GlaImageLayout,
    pub mapping: Mapping,
    pub modifier: FootprintModifier,
}

impl ImageRef {
    pub fn new(key: GlaImageKey, layout: GlaImageLayout) -> Self {
        Self {
            key,
            layout,
            mapping: Mapping::Identity,
            modifier: FootprintModifier::None,
        }
    }

    pub fn with_footprint(
        key: GlaImageKey,
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
pub struct DeriveCommand {
    pub dst: GlaImageKey,
    pub layout: GlaImageLayout,
    pub ops: Box<[Derive]>,
}

impl DeriveCommand {
    pub fn new(dst: GlaImageKey, layout: GlaImageLayout, ops: impl Into<Box<[Derive]>>) -> Self {
        Self {
            dst,
            layout,
            ops: ops.into(),
        }
    }

    pub fn exec_tile<C>(&self, ctx: &mut C, tile_index: u32) -> Result<(), C::Error>
    where
        C: RenderCtx,
    {
        let dst = ctx.write_tile(self.dst, tile_index)?;
        for op in self.ops.iter().copied() {
            op.exec_tile(ctx, self.layout, dst, tile_index)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Derive {
    Copy(Copy),
    Clear(Clear),
    RenderTo(RenderTo),
}

impl Derive {
    pub fn exec_tile<C>(
        self,
        ctx: &mut C,
        dst_layout: GlaImageLayout,
        dst: TileKey,
        tile_index: u32,
    ) -> Result<(), C::Error>
    where
        C: RenderCtx,
    {
        match self {
            Self::Copy(op) => op.exec_tile(ctx, dst_layout, dst, tile_index),
            Self::Clear(op) => op.exec_tile(ctx, dst),
            Self::RenderTo(op) => op.exec_tile(ctx, dst_layout, dst, tile_index),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Copy {
    pub src: ImageRef,
}

impl Copy {
    pub fn new(src: ImageRef) -> Self {
        Self { src }
    }

    pub fn exec_tile<C>(
        self,
        ctx: &mut C,
        dst_layout: GlaImageLayout,
        dst: TileKey,
        tile_index: u32,
    ) -> Result<(), C::Error>
    where
        C: RenderCtx,
    {
        self.src
            .for_each_source_tile(dst_layout, tile_index, |source_tile_index| {
                let src = ctx.render(self.src.key, source_tile_index)?;
                let src = ctx.acquire_for_read(src)?;
                let dst = ctx.acquire_for_write(dst)?;
                ctx.renderer().copy(src, dst);
                Ok(())
            })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clear;

impl Clear {
    pub fn exec_tile<C>(self, ctx: &mut C, dst: TileKey) -> Result<(), C::Error>
    where
        C: RenderCtx,
    {
        let dst = ctx.acquire_for_write(dst)?;
        ctx.renderer().clear(dst);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderTo {
    pub src: ImageRef,
    pub blend_mode: BlendMode,
    pub opacity: f32,
}

impl RenderTo {
    pub fn new(src: ImageRef, blend_mode: BlendMode, opacity: f32) -> Self {
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
        dst: TileKey,
        tile_index: u32,
    ) -> Result<(), C::Error>
    where
        C: RenderCtx,
    {
        self.src
            .for_each_source_tile(dst_layout, tile_index, |source_tile_index| {
                let src = ctx.render(self.src.key, source_tile_index)?;
                let src = ctx.acquire_for_read(src)?;
                let dst = ctx.acquire_for_write(dst)?;
                ctx.renderer()
                    .render_to(src, dst, self.blend_mode, self.opacity);
                Ok(())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas::{AtlasLayout, NoAtlasTextures, TilePos};
    use gla_color::{ChannelCount, ChannelType, GlaFormat};
    use gla_renderer::Pass;
    use tile_key::Tiles;

    #[derive(Debug)]
    enum TestError {
        MissingReturn,
        Tiles(tile_key::TilesError),
    }

    struct TestCtx {
        tiles: Tiles,
        renderer: Renderer,
        calls: Vec<(GlaImageKey, u32)>,
        write_calls: Vec<(GlaImageKey, u32)>,
        returns: Vec<TileKey>,
        dst_tile: Option<TileKey>,
        nested_pass: Option<TilePos>,
    }

    impl RenderCtx for TestCtx {
        type Error = TestError;

        fn render(&mut self, image: GlaImageKey, tile_index: u32) -> Result<TileKey, Self::Error> {
            self.calls.push((image, tile_index));
            if let Some(dst) = self.nested_pass {
                self.renderer.clear(dst);
            }
            self.returns.pop().ok_or(TestError::MissingReturn)
        }

        fn write_tile(
            &mut self,
            image: GlaImageKey,
            tile_index: u32,
        ) -> Result<TileKey, Self::Error> {
            self.write_calls.push((image, tile_index));
            self.dst_tile.ok_or(TestError::MissingReturn)
        }

        fn acquire_for_read(&mut self, key: TileKey) -> Result<TilePos, Self::Error> {
            self.tiles.acquire_for_read(key).map_err(TestError::Tiles)
        }

        fn acquire_for_write(&mut self, key: TileKey) -> Result<TilePos, Self::Error> {
            self.tiles.acquire_for_write(key).map_err(TestError::Tiles)
        }

        fn renderer(&mut self) -> &mut Renderer {
            &mut self.renderer
        }
    }

    fn format() -> GlaFormat {
        GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::U8,
        }
    }

    fn ctx_with_tiles(returns: Vec<TileKey>) -> TestCtx {
        TestCtx {
            tiles: Tiles::new(),
            renderer: Renderer::new(),
            calls: Vec::new(),
            write_calls: Vec::new(),
            returns,
            dst_tile: None,
            nested_pass: None,
        }
    }

    fn layout() -> GlaImageLayout {
        GlaImageLayout::new(4096, 4096)
    }

    fn new_test_atlas(tiles: &mut Tiles) -> u8 {
        let mut textures = NoAtlasTextures;
        tiles
            .new_atlas(AtlasLayout::TINY8, format(), &mut textures)
            .unwrap()
    }

    #[test]
    fn copy_renders_source_before_recording_copy_pass() {
        let mut ctx = ctx_with_tiles(Vec::new());
        let atlas = new_test_atlas(&mut ctx.tiles);
        let source_tile = ctx.tiles.alloc_from(atlas).unwrap();
        let dst_tile = ctx.tiles.alloc_from(atlas).unwrap();
        let nested_dst = ctx.tiles.acquire_for_write(dst_tile).unwrap();
        let source_pos = ctx.tiles.acquire_for_read(source_tile).unwrap();
        let dst_pos = ctx.tiles.acquire_for_write(dst_tile).unwrap();
        ctx.returns.push(source_tile);
        ctx.dst_tile = Some(dst_tile);
        ctx.nested_pass = Some(nested_dst);

        let source_image = GlaImageKey::new(7, 0);
        let dst_image = GlaImageKey::new(8, 0);
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
            ]
        );
    }

    #[test]
    fn render_to_records_blend_mode_and_opacity() {
        let mut ctx = ctx_with_tiles(Vec::new());
        let atlas = new_test_atlas(&mut ctx.tiles);
        let source_tile = ctx.tiles.alloc_from(atlas).unwrap();
        let dst_tile = ctx.tiles.alloc_from(atlas).unwrap();
        let source_pos = ctx.tiles.acquire_for_read(source_tile).unwrap();
        let dst_pos = ctx.tiles.acquire_for_write(dst_tile).unwrap();
        ctx.returns.push(source_tile);
        ctx.dst_tile = Some(dst_tile);

        let source_image = GlaImageKey::new(9, 0);
        let dst_image = GlaImageKey::new(10, 0);
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
            &[Pass::RenderTo {
                src: source_pos,
                dst: dst_pos,
                blend_mode: BlendMode::Overlay,
                opacity: 0.25,
            }]
        );
    }

    #[test]
    fn render_to_records_value_to_rgba_mask_mode() {
        let mut ctx = ctx_with_tiles(Vec::new());
        let atlas = new_test_atlas(&mut ctx.tiles);
        let source_tile = ctx.tiles.alloc_from(atlas).unwrap();
        let dst_tile = ctx.tiles.alloc_from(atlas).unwrap();
        let source_pos = ctx.tiles.acquire_for_read(source_tile).unwrap();
        let dst_pos = ctx.tiles.acquire_for_write(dst_tile).unwrap();
        ctx.returns.push(source_tile);
        ctx.dst_tile = Some(dst_tile);

        let source_image = GlaImageKey::new(13, 0);
        let dst_image = GlaImageKey::new(14, 0);
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
            &[Pass::RenderTo {
                src: source_pos,
                dst: dst_pos,
                blend_mode: BlendMode::MaskAlpha,
                opacity: 0.75,
            }]
        );
    }

    #[test]
    fn clear_writes_without_rendering_source() {
        let mut ctx = ctx_with_tiles(Vec::new());
        let atlas = new_test_atlas(&mut ctx.tiles);
        let dst_tile = ctx.tiles.alloc_from(atlas).unwrap();
        let dst_pos = ctx.tiles.acquire_for_write(dst_tile).unwrap();
        ctx.dst_tile = Some(dst_tile);
        let dst_image = GlaImageKey::new(12, 0);
        let command = DeriveCommand::new(dst_image, layout(), [Derive::Clear(Clear)]);

        command.exec_tile(&mut ctx, 5).unwrap();

        assert!(ctx.calls.is_empty());
        assert_eq!(ctx.write_calls, vec![(dst_image, 5)]);
        assert_eq!(ctx.renderer.passes(), &[Pass::Clear { dst: dst_pos }]);
    }
}
