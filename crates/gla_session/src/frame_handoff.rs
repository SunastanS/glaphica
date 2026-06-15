use crate::{
    DrawSession, ImageTileSlot, SessionError, SessionImageContent, SessionImageWriter,
    checked_layout_tile_count,
};
use atlas::TilePos;
use gla_draw_on::{DrawOnInvocation, DrawOnPass};
use gla_image::ImageError;
use gla_renderer::{Pass, RendererDrawOnInvocation};
use std::collections::{HashMap, HashSet};
use tile_key::TileReadRef;

pub(super) fn resolve(
    session: &mut DrawSession<'_>,
    passes: &[DrawOnPass<ImageTileSlot>],
) -> Result<Vec<Pass>, SessionError> {
    let mut init_passes = Vec::new();
    let mut slot_to_pos = HashMap::new();

    for pass in passes.iter().copied() {
        let slot = pass.target();
        if !slot_to_pos.contains_key(&slot) {
            let dst = resolve_slot(session, slot, &mut init_passes)?;
            slot_to_pos.insert(slot, dst);
        }
    }

    let draw_on_passes = passes
        .iter()
        .copied()
        .map(|pass| {
            pass.invocation().map_target(|slot| {
                *slot_to_pos
                    .get(&slot)
                    .expect("draw-on slot must be resolved before renderer pass generation")
            })
        })
        .collect::<Vec<RendererDrawOnInvocation>>();

    Ok(finalize(init_passes, &draw_on_passes))
}

fn resolve_slot(
    session: &mut DrawSession<'_>,
    slot: ImageTileSlot,
    init_passes: &mut Vec<Pass>,
) -> Result<TilePos, SessionError> {
    let id = slot.image;
    let tile_index = slot.tile_index.value();
    let first_edit_write = {
        let image = session
            .images
            .get(&id)
            .ok_or(SessionError::MissingLocalImage { id })?;
        if !matches!(image.writer(), SessionImageWriter::DrawOn(_)) {
            return Err(SessionError::DestinationNotWritable { id });
        }
        match image.content() {
            SessionImageContent::Raw(_) => false,
            SessionImageContent::Edit(edit) => edit.tile(tile_index).is_none(),
        }
    };
    let source_ref = if first_edit_write {
        Some(session.global.read_global_ref(id, tile_index)?)
    } else {
        None
    };

    let image = session
        .images
        .get_mut(&id)
        .ok_or(SessionError::MissingLocalImage { id })?;
    match &mut image.content {
        SessionImageContent::Raw(raw) => {
            let tile = raw
                .tile_mut(tile_index)
                .map_err(|source| SessionError::Image { id, source })?;
            session
                .global
                .write_tile_pos_with_zero_init(tile, |dst| {
                    init_passes.push(Pass::Clear { dst });
                })
                .map_err(|source| SessionError::Tile { id, source })
        }
        SessionImageContent::Edit(edit) => {
            let tile_count = checked_layout_tile_count(id, image.layout)?;
            if tile_index >= tile_count {
                return Err(SessionError::Image {
                    id,
                    source: ImageError::TileIndexOutOfBounds {
                        tile_index,
                        tile_count,
                    },
                });
            }
            let tile = if first_edit_write {
                let tile = session
                    .global
                    .reserve_tile_for_format(image.format)
                    .map_err(|source| SessionError::Tile { id, source })?;
                edit.insert_tile(tile_index, tile)
            } else {
                edit.tile_mut(tile_index)
                    .expect("existing edit tile must be present")
            };

            match source_ref {
                Some(TileReadRef::Physical(src)) => {
                    let dst = session
                        .global
                        .write_tile_pos(tile)
                        .map_err(|source| SessionError::Tile { id, source })?;
                    init_passes.push(Pass::Copy { src, dst });
                    Ok(dst)
                }
                Some(TileReadRef::Zero) | None => session
                    .global
                    .write_tile_pos_with_zero_init(tile, |dst| {
                        init_passes.push(Pass::Clear { dst });
                    })
                    .map_err(|source| SessionError::Tile { id, source }),
            }
        }
    }
}

fn finalize(init_passes: Vec<Pass>, draw_on_passes: &[RendererDrawOnInvocation]) -> Vec<Pass> {
    let mut finalized = init_passes;
    let mut touched = Vec::new();
    let mut seen = HashSet::new();

    for invocation in draw_on_passes.iter().copied() {
        finalized.push(Pass::DrawOn(invocation));
        let dst = invocation.target();
        if seen.insert(dst) {
            touched.push(dst);
        }
    }
    flush_gutters(&mut finalized, &mut touched, &mut seen);
    finalized
}

fn flush_gutters(
    finalized: &mut Vec<Pass>,
    touched: &mut Vec<TilePos>,
    seen: &mut HashSet<TilePos>,
) {
    for dst in touched.drain(..) {
        finalized.push(Pass::FixGutter { dst });
    }
    seen.clear();
}
