struct DabWrite {
    pixel_x: u32,
    pixel_y: u32,
    atlas_layer: u32,
    pressure: f32,
};

struct DabWriteMeta {
    dab_count: u32,
    texture_width: u32,
    texture_height: u32,
    tile_stride: u32,
};

@group(0) @binding(0)
var<storage, read> dabs: array<DabWrite>;

@group(0) @binding(1)
var<uniform> dab_meta: DabWriteMeta;

@group(0) @binding(2)
var brush_buffer: texture_storage_2d_array<r32float, write>;

const DEFAULT_BRUSH_RADIUS_PIXELS: i32 = 3;

@compute @workgroup_size(1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let index = gid.x;
    if (index >= dab_meta.dab_count) {
        return;
    }

    let dab = dabs[index];
    if (dab.pixel_x >= dab_meta.texture_width || dab.pixel_y >= dab_meta.texture_height) {
        return;
    }

    let center_x = i32(dab.pixel_x);
    let center_y = i32(dab.pixel_y);
    let texture_width = i32(dab_meta.texture_width);
    let texture_height = i32(dab_meta.texture_height);
    let tile_stride = i32(dab_meta.tile_stride);
    if (tile_stride <= 0) {
        return;
    }
    let slot_origin_x = (center_x / tile_stride) * tile_stride;
    let slot_origin_y = (center_y / tile_stride) * tile_stride;
    let slot_max_x = slot_origin_x + tile_stride - 1;
    let slot_max_y = slot_origin_y + tile_stride - 1;
    for (var offset_y: i32 = -DEFAULT_BRUSH_RADIUS_PIXELS; offset_y <= DEFAULT_BRUSH_RADIUS_PIXELS; offset_y = offset_y + 1) {
        let write_y = center_y + offset_y;
        if (write_y < 0 || write_y >= texture_height || write_y < slot_origin_y || write_y > slot_max_y) {
            continue;
        }
        for (var offset_x: i32 = -DEFAULT_BRUSH_RADIUS_PIXELS; offset_x <= DEFAULT_BRUSH_RADIUS_PIXELS; offset_x = offset_x + 1) {
            let write_x = center_x + offset_x;
            if (write_x < 0 || write_x >= texture_width || write_x < slot_origin_x || write_x > slot_max_x) {
                continue;
            }
            textureStore(
                brush_buffer,
                vec2<i32>(write_x, write_y),
                i32(dab.atlas_layer),
                vec4<f32>(dab.pressure, 0.0, 0.0, 1.0),
            );
        }
    }
}
