use gla_color::{ChannelCount, ChannelType, GlaFormat, PremultipliedRgbaF32};
use gla_image::{GlaImageLayout, IMAGE_TILE_SIZE, ImageLayoutError, ImageTileIndex};
pub use gla_ir::DrawOnToolKind;
use serde::{Deserialize, Serialize};

pub trait DrawOnToolSpec {
    fn target_format(self) -> GlaFormat;
    fn accepts_target_format(self, format: GlaFormat) -> bool;
    fn accepts_input_kind(self, kind: DrawOnInputKind) -> bool;
}

impl DrawOnToolSpec for DrawOnToolKind {
    fn target_format(self) -> GlaFormat {
        match self {
            Self::RadialKernel1D => GlaFormat {
                channel_count: ChannelCount::D1,
                channel_type: ChannelType::F32,
            },
            Self::ReplaceCircle4D => GlaFormat {
                channel_count: ChannelCount::D4,
                channel_type: ChannelType::F32,
            },
        }
    }

    fn accepts_target_format(self, format: GlaFormat) -> bool {
        format == self.target_format()
    }

    fn accepts_input_kind(self, kind: DrawOnInputKind) -> bool {
        self == kind.tool()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
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

impl DrawOnInputKind {
    pub fn tool(self) -> DrawOnToolKind {
        match self {
            Self::RadialKernel1D { .. } => DrawOnToolKind::RadialKernel1D,
            Self::ReplaceCircle4D { .. } => DrawOnToolKind::ReplaceCircle4D,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DrawOnPass<T> {
    Invoke(DrawOnInvocation<T>),
}

impl<T: Copy> DrawOnPass<T> {
    pub fn target(self) -> T {
        match self {
            Self::Invoke(invocation) => invocation.target(),
        }
    }

    pub fn invocation(self) -> DrawOnInvocation<T> {
        match self {
            Self::Invoke(invocation) => invocation,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DrawOnInvocation<T> {
    RadialKernel1D {
        dst: T,
        center_in_tile_x: f32,
        center_in_tile_y: f32,
        radius_px: f32,
        amplitude: f32,
    },
    ReplaceCircle4D {
        dst: T,
        center_in_tile_x: f32,
        center_in_tile_y: f32,
        radius_px: f32,
        color: PremultipliedRgbaF32,
    },
}

impl<T: Copy> DrawOnInvocation<T> {
    pub fn tool(self) -> DrawOnToolKind {
        match self {
            Self::RadialKernel1D { .. } => DrawOnToolKind::RadialKernel1D,
            Self::ReplaceCircle4D { .. } => DrawOnToolKind::ReplaceCircle4D,
        }
    }

    pub fn target(self) -> T {
        match self {
            Self::RadialKernel1D { dst, .. } | Self::ReplaceCircle4D { dst, .. } => dst,
        }
    }

    pub fn map_target<U>(self, f: impl FnOnce(T) -> U) -> DrawOnInvocation<U> {
        match self {
            Self::RadialKernel1D {
                dst,
                center_in_tile_x,
                center_in_tile_y,
                radius_px,
                amplitude,
            } => DrawOnInvocation::RadialKernel1D {
                dst: f(dst),
                center_in_tile_x,
                center_in_tile_y,
                radius_px,
                amplitude,
            },
            Self::ReplaceCircle4D {
                dst,
                center_in_tile_x,
                center_in_tile_y,
                radius_px,
                color,
            } => DrawOnInvocation::ReplaceCircle4D {
                dst: f(dst),
                center_in_tile_x,
                center_in_tile_y,
                radius_px,
                color,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum DrawOnLoweringError<E> {
    InputMismatch {
        tool: DrawOnToolKind,
        input: DrawOnToolKind,
    },
    InvalidLayout {
        source: ImageLayoutError,
    },
    Slot {
        source: E,
    },
}

pub fn lower_input<Target, PassTarget, E>(
    target: Target,
    tool: DrawOnToolKind,
    layout: GlaImageLayout,
    input: DrawOnInput,
    mut write_slot: impl FnMut(Target, ImageTileIndex) -> Result<PassTarget, E>,
) -> Result<Vec<DrawOnPass<PassTarget>>, DrawOnLoweringError<E>>
where
    Target: Copy,
    PassTarget: Copy,
{
    if !input.center_x.is_finite()
        || !input.center_y.is_finite()
        || !input.footprint_radius_px.is_finite()
        || input.footprint_radius_px <= 0.0
    {
        return Ok(Vec::new());
    }

    match (tool, input.kind) {
        (DrawOnToolKind::RadialKernel1D, DrawOnInputKind::RadialKernel1D { .. }) => {
            lower_radial_kernel_1d(target, layout, input, write_slot)
        }
        (DrawOnToolKind::ReplaceCircle4D, DrawOnInputKind::ReplaceCircle4D { .. }) => {
            lower_replace_circle_4d(target, layout, input, write_slot)
        }
        (tool, kind) => Err(DrawOnLoweringError::InputMismatch {
            tool,
            input: kind.tool(),
        }),
    }
}

#[derive(Clone, Copy, Debug)]
struct DabTile {
    index: ImageTileIndex,
    origin_x: u32,
    origin_y: u32,
}

fn lower_radial_kernel_1d<Target, PassTarget, E>(
    target: Target,
    layout: GlaImageLayout,
    input: DrawOnInput,
    mut write_slot: impl FnMut(Target, ImageTileIndex) -> Result<PassTarget, E>,
) -> Result<Vec<DrawOnPass<PassTarget>>, DrawOnLoweringError<E>>
where
    Target: Copy,
    PassTarget: Copy,
{
    let DrawOnInput {
        center_x,
        center_y,
        footprint_radius_px,
        kind:
            DrawOnInputKind::RadialKernel1D {
                radius_px,
                amplitude,
            },
    } = input
    else {
        unreachable!("lower_radial_kernel_1d called with non-radial input");
    };

    if !radius_px.is_finite() || radius_px <= 0.0 || !amplitude.is_finite() || amplitude <= 0.0 {
        return Ok(Vec::new());
    }

    let writes = radial_footprint_tiles(layout, center_x, center_y, footprint_radius_px)
        .map_err(map_footprint_error)?
        .into_iter()
        .map(|tile| {
            let dst = write_slot(target, tile.index)
                .map_err(|source| DrawOnLoweringError::Slot { source })?;
            Ok((tile, dst))
        })
        .collect::<Result<Vec<_>, DrawOnLoweringError<E>>>()?;

    Ok(writes
        .into_iter()
        .map(|(tile, dst)| {
            let center_in_tile_x = center_x - tile.origin_x as f32;
            let center_in_tile_y = center_y - tile.origin_y as f32;
            DrawOnPass::Invoke(DrawOnInvocation::RadialKernel1D {
                dst,
                center_in_tile_x,
                center_in_tile_y,
                radius_px,
                amplitude,
            })
        })
        .collect())
}

fn lower_replace_circle_4d<Target, PassTarget, E>(
    target: Target,
    layout: GlaImageLayout,
    input: DrawOnInput,
    mut write_slot: impl FnMut(Target, ImageTileIndex) -> Result<PassTarget, E>,
) -> Result<Vec<DrawOnPass<PassTarget>>, DrawOnLoweringError<E>>
where
    Target: Copy,
    PassTarget: Copy,
{
    let DrawOnInput {
        center_x,
        center_y,
        footprint_radius_px,
        kind: DrawOnInputKind::ReplaceCircle4D { radius_px, color },
    } = input
    else {
        unreachable!("lower_replace_circle_4d called with non-replace input");
    };

    if !radius_px.is_finite() || radius_px <= 0.0 {
        return Ok(Vec::new());
    }

    let writes = radial_footprint_tiles(layout, center_x, center_y, footprint_radius_px)
        .map_err(map_footprint_error)?
        .into_iter()
        .map(|tile| {
            let dst = write_slot(target, tile.index)
                .map_err(|source| DrawOnLoweringError::Slot { source })?;
            Ok((tile, dst))
        })
        .collect::<Result<Vec<_>, DrawOnLoweringError<E>>>()?;

    Ok(writes
        .into_iter()
        .map(|(tile, dst)| {
            let center_in_tile_x = center_x - tile.origin_x as f32;
            let center_in_tile_y = center_y - tile.origin_y as f32;
            DrawOnPass::Invoke(DrawOnInvocation::ReplaceCircle4D {
                dst,
                center_in_tile_x,
                center_in_tile_y,
                radius_px,
                color,
            })
        })
        .collect())
}

fn radial_footprint_tiles(
    layout: GlaImageLayout,
    center_x: f32,
    center_y: f32,
    radius: f32,
) -> Result<Vec<DabTile>, DrawOnLoweringError<()>> {
    layout
        .checked_tile_count()
        .map_err(|source| DrawOnLoweringError::InvalidLayout { source })?;
    if !footprint_intersects_layout(layout, center_x, center_y, radius) {
        return Ok(Vec::new());
    }

    let min_tx = tile_coord_for_px(center_x - radius, layout.width_px(), layout.tile_count_x());
    let max_tx = tile_coord_for_px(center_x + radius, layout.width_px(), layout.tile_count_x());
    let min_ty = tile_coord_for_px(center_y - radius, layout.height_px(), layout.tile_count_y());
    let max_ty = tile_coord_for_px(center_y + radius, layout.height_px(), layout.tile_count_y());
    let tile_count_x = layout.tile_count_x();
    let mut tiles = Vec::new();

    for ty in min_ty..=max_ty {
        for tx in min_tx..=max_tx {
            let index = layout
                .tile_index(ty * tile_count_x + tx)
                .expect("footprint tile coordinates must be in layout bounds");
            tiles.push(DabTile {
                index,
                origin_x: tx * IMAGE_TILE_SIZE,
                origin_y: ty * IMAGE_TILE_SIZE,
            });
        }
    }

    Ok(tiles)
}

fn footprint_intersects_layout(
    layout: GlaImageLayout,
    center_x: f32,
    center_y: f32,
    radius: f32,
) -> bool {
    let max_x = layout.width_px() as f32;
    let max_y = layout.height_px() as f32;
    center_x + radius >= 0.0
        && center_y + radius >= 0.0
        && center_x - radius < max_x
        && center_y - radius < max_y
}

fn tile_coord_for_px(px: f32, extent_px: u32, tile_count: u32) -> u32 {
    debug_assert!(tile_count > 0);
    let max_px = extent_px.saturating_sub(1) as f32;
    let clamped = finite_or_zero(px).max(0.0).min(max_px);
    ((clamped / IMAGE_TILE_SIZE as f32).floor() as u32).min(tile_count - 1)
}

fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

fn map_footprint_error<E>(error: DrawOnLoweringError<()>) -> DrawOnLoweringError<E> {
    match error {
        DrawOnLoweringError::InputMismatch { tool, input } => {
            DrawOnLoweringError::InputMismatch { tool, input }
        }
        DrawOnLoweringError::InvalidLayout { source } => {
            DrawOnLoweringError::InvalidLayout { source }
        }
        DrawOnLoweringError::Slot { .. } => {
            unreachable!("unit slot errors are not produced by footprint lowering")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_target_formats_are_static() {
        assert_eq!(
            DrawOnToolKind::RadialKernel1D.target_format(),
            GlaFormat {
                channel_count: ChannelCount::D1,
                channel_type: ChannelType::F32,
            }
        );
        assert_eq!(
            DrawOnToolKind::ReplaceCircle4D.target_format(),
            GlaFormat {
                channel_count: ChannelCount::D4,
                channel_type: ChannelType::F32,
            }
        );
    }

    #[test]
    fn input_kind_maps_to_owning_primitive() {
        assert_eq!(
            DrawOnInputKind::RadialKernel1D {
                radius_px: 1.0,
                amplitude: 1.0,
            }
            .tool(),
            DrawOnToolKind::RadialKernel1D
        );
        assert_eq!(
            DrawOnInputKind::ReplaceCircle4D {
                radius_px: 1.0,
                color: PremultipliedRgbaF32::TRANSPARENT,
            }
            .tool(),
            DrawOnToolKind::ReplaceCircle4D
        );
    }

    #[test]
    fn draw_on_pass_reports_target_and_invocation() {
        let dst = (0_u32, 0_u32, 1_u32, 2_u32);
        let invocation = DrawOnInvocation::RadialKernel1D {
            dst,
            center_in_tile_x: 1.0,
            center_in_tile_y: 2.0,
            radius_px: 3.0,
            amplitude: 4.0,
        };

        assert_eq!(DrawOnPass::Invoke(invocation).target(), dst);
        assert_eq!(DrawOnPass::Invoke(invocation).invocation(), invocation);
        assert_eq!(
            invocation.map_target(|dst| dst.2),
            DrawOnInvocation::RadialKernel1D {
                dst: 1,
                center_in_tile_x: 1.0,
                center_in_tile_y: 2.0,
                radius_px: 3.0,
                amplitude: 4.0,
            }
        );
    }

    #[test]
    fn lower_input_records_tile_local_radial_invocations() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE).unwrap();
        let input = DrawOnInput::radial_kernel_1d(IMAGE_TILE_SIZE as f32, 4.0, 1.0, 3.0, 0.25);
        let mut writes = Vec::new();

        let passes = lower_input(
            7_u8,
            DrawOnToolKind::RadialKernel1D,
            layout,
            input,
            |id, tile| {
                writes.push((id, tile.value()));
                Ok::<_, ()>((id, tile.value()))
            },
        )
        .unwrap();

        assert_eq!(writes, vec![(7, 0), (7, 1)]);
        assert_eq!(
            passes,
            vec![
                DrawOnPass::Invoke(DrawOnInvocation::RadialKernel1D {
                    dst: (7, 0),
                    center_in_tile_x: IMAGE_TILE_SIZE as f32,
                    center_in_tile_y: 4.0,
                    radius_px: 3.0,
                    amplitude: 0.25,
                }),
                DrawOnPass::Invoke(DrawOnInvocation::RadialKernel1D {
                    dst: (7, 1),
                    center_in_tile_x: 0.0,
                    center_in_tile_y: 4.0,
                    radius_px: 3.0,
                    amplitude: 0.25,
                }),
            ]
        );
    }

    #[test]
    fn lower_input_rejects_mismatched_primitive_input() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE).unwrap();
        let input = DrawOnInput::radial_kernel_1d(0.0, 0.0, 1.0, 3.0, 0.25);

        let err = lower_input(
            7_u8,
            DrawOnToolKind::ReplaceCircle4D,
            layout,
            input,
            |id, tile| Ok::<_, ()>((id, tile.value())),
        )
        .unwrap_err();

        assert_eq!(
            err,
            DrawOnLoweringError::InputMismatch {
                tool: DrawOnToolKind::ReplaceCircle4D,
                input: DrawOnToolKind::RadialKernel1D,
            }
        );
    }

    #[test]
    fn lower_input_treats_zero_effect_radial_input_as_noop() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE).unwrap();
        let input = DrawOnInput::radial_kernel_1d(0.0, 0.0, 1.0, 0.0, 0.25);
        let mut writes = Vec::new();

        let passes = lower_input(
            7_u8,
            DrawOnToolKind::RadialKernel1D,
            layout,
            input,
            |id, tile| {
                writes.push((id, tile.value()));
                Ok::<_, ()>((id, tile.value()))
            },
        )
        .unwrap();

        assert!(passes.is_empty());
        assert!(writes.is_empty());
    }

    #[test]
    fn lower_input_preserves_typed_radial_amplitude() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE).unwrap();
        let input = DrawOnInput::radial_kernel_1d(0.0, 0.0, 1.0, 3.0, 4.0);

        let passes = lower_input(
            7_u8,
            DrawOnToolKind::RadialKernel1D,
            layout,
            input,
            |id, tile| Ok::<_, ()>((id, tile.value())),
        )
        .unwrap();

        assert!(matches!(
            passes.as_slice(),
            [DrawOnPass::Invoke(DrawOnInvocation::RadialKernel1D { amplitude, .. })]
                if *amplitude == 4.0
        ));
    }

    #[test]
    fn lower_input_records_replace_circle_invocation() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE).unwrap();
        let color = PremultipliedRgbaF32::new(0.25, 0.5, 0.75, 1.0);
        let input = DrawOnInput::replace_circle_4d(0.0, 0.0, 1.0, 2.0, color);

        let passes = lower_input(
            7_u8,
            DrawOnToolKind::ReplaceCircle4D,
            layout,
            input,
            |id, tile| Ok::<_, ()>((id, tile.value())),
        )
        .unwrap();

        assert_eq!(
            passes,
            vec![DrawOnPass::Invoke(DrawOnInvocation::ReplaceCircle4D {
                dst: (7, 0),
                center_in_tile_x: 0.0,
                center_in_tile_y: 0.0,
                radius_px: 2.0,
                color,
            })]
        );
    }
}
