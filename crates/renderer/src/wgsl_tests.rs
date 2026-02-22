#[test]
fn renderer_wgsl_sources_parse_successfully() {
    parse_wgsl("brush_dab_write.wgsl", include_str!("brush_dab_write.wgsl"));
    parse_wgsl("merge_tile.wgsl", include_str!("merge_tile.wgsl"));
    parse_wgsl("tile_composite.wgsl", include_str!("tile_composite.wgsl"));
    parse_wgsl(
        "tile_composite_slot.wgsl",
        include_str!("tile_composite_slot.wgsl"),
    );
}

fn parse_wgsl(label: &str, source: &str) {
    naga::front::wgsl::parse_str(source).unwrap_or_else(|error| {
        panic!(
            "WGSL parse failed for {label}: {}",
            error.emit_to_string(source)
        )
    });
}
