struct DabWrite {
    write_min_x: u32,
    write_min_y: u32,
    write_max_x: u32,
    write_max_y: u32,
    atlas_layer: u32,
    pressure: f32,
};

struct DabWriteMeta {
    dab_count: u32,
    texture_width: u32,
    texture_height: u32,
    _padding0: u32,
};

@group(0) @binding(0)
var<storage, read> dabs: array<DabWrite>;

@group(0) @binding(1)
var<uniform> dab_meta: DabWriteMeta;

@group(0) @binding(2)
var brush_buffer: texture_storage_2d_array<r32float, write>;

@compute @workgroup_size(1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let index = gid.x;
    if (index >= dab_meta.dab_count) {
        return;
    }

    let dab = dabs[index];

    let texture_width = i32(dab_meta.texture_width);
    let texture_height = i32(dab_meta.texture_height);
    let write_min_x = i32(dab.write_min_x);
    let write_min_y = i32(dab.write_min_y);
    let write_max_x = i32(dab.write_max_x);
    let write_max_y = i32(dab.write_max_y);
    if (write_min_x > write_max_x || write_min_y > write_max_y) {
        return;
    }
    for (var write_y: i32 = write_min_y; write_y <= write_max_y; write_y = write_y + 1) {
        if (write_y < 0 || write_y >= texture_height) {
            continue;
        }
        for (var write_x: i32 = write_min_x; write_x <= write_max_x; write_x = write_x + 1) {
            if (write_x < 0 || write_x >= texture_width) {
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
