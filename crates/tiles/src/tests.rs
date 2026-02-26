use super::*;

fn create_device_queue() -> (wgpu::Device, wgpu::Queue) {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .expect("request wgpu adapter");
        let limits = adapter.limits();
        adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("tiles tests"),
                required_features: wgpu::Features::empty(),
                required_limits: limits,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .expect("request wgpu device")
    })
}

fn create_store(
    device: &wgpu::Device,
    config: TileAtlasConfig,
) -> (TileAtlasStore, TileAtlasGpuArray) {
    TileAtlasStore::with_config(device, config).expect("TileAtlasStore::with_config")
}

fn create_generic_store_r32f(
    device: &wgpu::Device,
    config: GenericTileAtlasConfig,
) -> (
    GenericR32FloatTileAtlasStore,
    GenericR32FloatTileAtlasGpuArray,
) {
    GenericR32FloatTileAtlasStore::with_config(device, config)
        .expect("GenericR32FloatTileAtlasStore::with_config")
}

fn create_generic_store_r8u(
    device: &wgpu::Device,
    config: GenericTileAtlasConfig,
) -> (GenericR8UintTileAtlasStore, GenericR8UintTileAtlasGpuArray) {
    GenericR8UintTileAtlasStore::with_config(device, config)
        .expect("GenericR8UintTileAtlasStore::with_config")
}

fn read_tile_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    address: TileAddress,
) -> Vec<u8> {
    let buffer_size = (TILE_IMAGE as u64) * (TILE_IMAGE as u64) * 4;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tile readback"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("tile readback"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: tile_origin(address),
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(TILE_IMAGE * 4),
                rows_per_image: Some(TILE_IMAGE),
            },
        },
        wgpu::Extent3d {
            width: TILE_IMAGE,
            height: TILE_IMAGE,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    let slice = buffer.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        sender.send(result).expect("map callback send");
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll");
    receiver
        .recv()
        .expect("map callback recv")
        .expect("map tile readback");
    let tile = slice.get_mapped_range().to_vec();
    buffer.unmap();
    tile
}

fn read_texel_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    x: u32,
    y: u32,
    z: u32,
) -> [u8; 4] {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tile texel readback"),
        size: 256,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("tile texel readback"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y, z },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(1),
            },
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    let slice = buffer.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        sender.send(result).expect("map callback send");
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll");
    receiver
        .recv()
        .expect("map callback recv")
        .expect("map texel readback");
    let mapped = slice.get_mapped_range();
    let texel = [mapped[0], mapped[1], mapped[2], mapped[3]];
    drop(mapped);
    buffer.unmap();
    texel
}

fn read_tile_r32float(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    address: TileAddress,
) -> Vec<f32> {
    let buffer_size = (TILE_IMAGE as u64) * (TILE_IMAGE as u64) * 4;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tile r32float readback"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("tile r32float readback"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: tile_origin(address),
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(TILE_IMAGE * 4),
                rows_per_image: Some(TILE_IMAGE),
            },
        },
        wgpu::Extent3d {
            width: TILE_IMAGE,
            height: TILE_IMAGE,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    let slice = buffer.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        sender.send(result).expect("map callback send");
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll");
    receiver
        .recv()
        .expect("map callback recv")
        .expect("map tile readback");
    let mapped = slice.get_mapped_range();
    let floats = mapped
        .chunks_exact(4)
        .map(|chunk| f32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect::<Vec<f32>>();
    drop(mapped);
    buffer.unmap();
    floats
}

fn read_tile_r8uint(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    address: TileAddress,
) -> Vec<u8> {
    let row_bytes = TILE_IMAGE as usize;
    let padded_row_bytes = row_bytes.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
    let buffer_size = (padded_row_bytes as u64) * (TILE_IMAGE as u64);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tile r8uint readback"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("tile r8uint readback"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: tile_origin(address),
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row_bytes as u32),
                rows_per_image: Some(TILE_IMAGE),
            },
        },
        wgpu::Extent3d {
            width: TILE_IMAGE,
            height: TILE_IMAGE,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    let slice = buffer.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        sender.send(result).expect("map callback send");
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll");
    receiver
        .recv()
        .expect("map callback recv")
        .expect("map tile readback");
    let mapped = slice.get_mapped_range();
    let mut tile = vec![0u8; row_bytes * (TILE_IMAGE as usize)];
    for row in 0..(TILE_IMAGE as usize) {
        let source_start = row * padded_row_bytes;
        let source_end = source_start + row_bytes;
        let destination_start = row * row_bytes;
        let destination_end = destination_start + row_bytes;
        tile[destination_start..destination_end].copy_from_slice(&mapped[source_start..source_end]);
    }
    drop(mapped);
    buffer.unmap();
    tile
}

fn source_texel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let index = ((y as usize) * (TILE_IMAGE as usize) + (x as usize)) * 4;
    [
        bytes[index],
        bytes[index + 1],
        bytes[index + 2],
        bytes[index + 3],
    ]
}

fn supports_r32float_storage(device: &wgpu::Device) -> bool {
    GenericR32FloatTileAtlasStore::with_config(
        device,
        GenericTileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC
                | TileAtlasUsage::STORAGE_BINDING,
            ..GenericTileAtlasConfig::default()
        },
    )
    .is_ok()
}

#[test]
fn config_default_max_layers_is_four() {
    let config = TileAtlasConfig::default();
    assert_eq!(config.max_layers, 4);
}

#[test]
fn atlas_size_should_match_tile_stride_and_capacity_contract() {
    let tile_stride = TILE_STRIDE;
    assert_eq!(
        ATLAS_SIZE,
        TILES_PER_ROW * tile_stride,
        "atlas should preserve tiles-per-row capacity when adding 1px gutter"
    );
}

#[test]
fn ingest_tile_should_define_gutter_pixels_from_edge_texels() {
    let (device, queue) = create_device_queue();
    let (store, gpu) = create_store(
        &device,
        TileAtlasConfig {
            max_layers: 1,
            ..TileAtlasConfig::default()
        },
    );

    let mut bytes = vec![0u8; (TILE_IMAGE as usize) * (TILE_IMAGE as usize) * 4];
    for y in 0..TILE_IMAGE {
        for x in 0..TILE_IMAGE {
            let index = ((y as usize) * (TILE_IMAGE as usize) + (x as usize)) * 4;
            bytes[index] = (x % 251) as u8;
            bytes[index + 1] = (y % 251) as u8;
            bytes[index + 2] = ((x + y) % 251) as u8;
            bytes[index + 3] = 255;
        }
    }

    let key = store
        .ingest_tile(TILE_IMAGE, TILE_IMAGE, &bytes)
        .expect("ingest tile")
        .expect("non-empty tile");
    let address = store.resolve(key).expect("resolve key");
    let tile_count = gpu.drain_and_execute(&queue).expect("drain upload");
    assert_eq!(tile_count, 1);

    let tile_stride = TILE_STRIDE;
    let atlas_tile_origin_x = address.tile_x() * tile_stride;
    let atlas_tile_origin_y = address.tile_y() * tile_stride;

    let source_top_left = source_texel(&bytes, 0, 0);
    let source_top_right = source_texel(&bytes, TILE_IMAGE - 1, 0);
    let source_bottom_left = source_texel(&bytes, 0, TILE_IMAGE - 1);
    let source_bottom_right = source_texel(&bytes, TILE_IMAGE - 1, TILE_IMAGE - 1);

    assert_eq!(
        read_texel_rgba8(
            &device,
            &queue,
            gpu.texture(),
            atlas_tile_origin_x,
            atlas_tile_origin_y,
            address.atlas_layer,
        ),
        source_top_left,
        "top-left gutter texel should match top-left source texel"
    );
    assert_eq!(
        read_texel_rgba8(
            &device,
            &queue,
            gpu.texture(),
            atlas_tile_origin_x + tile_stride - 1,
            atlas_tile_origin_y,
            address.atlas_layer,
        ),
        source_top_right,
        "top-right gutter texel should match top-right source texel"
    );
    assert_eq!(
        read_texel_rgba8(
            &device,
            &queue,
            gpu.texture(),
            atlas_tile_origin_x,
            atlas_tile_origin_y + tile_stride - 1,
            address.atlas_layer,
        ),
        source_bottom_left,
        "bottom-left gutter texel should match bottom-left source texel"
    );
    assert_eq!(
        read_texel_rgba8(
            &device,
            &queue,
            gpu.texture(),
            atlas_tile_origin_x + tile_stride - 1,
            atlas_tile_origin_y + tile_stride - 1,
            address.atlas_layer,
        ),
        source_bottom_right,
        "bottom-right gutter texel should match bottom-right source texel"
    );
    assert_eq!(
        read_texel_rgba8(
            &device,
            &queue,
            gpu.texture(),
            atlas_tile_origin_x + 1,
            atlas_tile_origin_y + 1,
            address.atlas_layer,
        ),
        source_top_left,
        "content origin should be shifted by 1px due to gutter"
    );
}

#[test]
fn release_is_cpu_only_and_dirty_triggers_clear_on_reuse() {
    let (device, queue) = create_device_queue();
    let (store, gpu) = create_store(
        &device,
        TileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC
                | TileAtlasUsage::TEXTURE_BINDING,
            ..TileAtlasConfig::default()
        },
    );

    let key0 = store.allocate().expect("allocate key0");
    let address0 = store.resolve(key0).expect("key0 address");
    let tile_count = gpu.drain_and_execute(&queue).expect("drain after key0");
    assert_eq!(tile_count, 0);

    assert!(store.release(key0));
    let tile_count = gpu.drain_and_execute(&queue).expect("drain after release");
    assert_eq!(tile_count, 0);

    let key1 = store.allocate().expect("allocate key1");
    let address1 = store.resolve(key1).expect("key1 address");
    assert_eq!(address1, address0);
    let tile_count = gpu.drain_and_execute(&queue).expect("drain clear");
    assert_eq!(tile_count, 1);

    let tile = read_tile_rgba8(&device, &queue, gpu.texture(), address1);
    assert!(tile.iter().all(|&byte| byte == 0));
}

#[test]
fn ingest_tile_enqueues_upload_and_writes_after_drain() {
    let (device, queue) = create_device_queue();
    let (store, gpu) = create_store(&device, TileAtlasConfig::default());

    let mut bytes = vec![0u8; (TILE_IMAGE as usize) * (TILE_IMAGE as usize) * 4];
    bytes[0] = 9;
    bytes[1] = 8;
    bytes[2] = 7;
    bytes[3] = 6;
    let key = store
        .ingest_tile(TILE_IMAGE, TILE_IMAGE, &bytes)
        .expect("ingest tile")
        .expect("non-empty tile");
    let address = store.resolve(key).expect("resolve key");

    let tile_count = gpu.drain_and_execute(&queue).expect("drain upload");
    assert_eq!(tile_count, 1);
    let tile = read_tile_rgba8(&device, &queue, gpu.texture(), address);
    assert_eq!(&tile[..4], &[9, 8, 7, 6]);
}

#[test]
fn ingest_image_rgba8_strided_keeps_sparse_tiles() {
    let (device, queue) = create_device_queue();
    let (store, gpu) = create_store(&device, TileAtlasConfig::default());

    let size_x = TILE_IMAGE + 1;
    let size_y = TILE_IMAGE + 1;
    let row_bytes = size_x * 4;
    let bytes_per_row = row_bytes + 8;
    let required_len =
        (bytes_per_row as usize) * ((size_y as usize).saturating_sub(1)) + (row_bytes as usize);
    let mut bytes = vec![0u8; required_len];
    let pixel_x = TILE_IMAGE;
    let pixel_y = TILE_IMAGE;
    let index = (pixel_y as usize) * (bytes_per_row as usize) + (pixel_x as usize) * 4;
    bytes[index] = 1;

    let image = store
        .ingest_image_rgba8_strided(size_x, size_y, &bytes, bytes_per_row)
        .expect("ingest image");
    assert_eq!(image.tiles_per_row(), 2);
    assert_eq!(image.tiles_per_column(), 2);
    assert_eq!(image.get_tile(0, 0), Ok(None));
    assert_eq!(image.get_tile(1, 0), Ok(None));
    assert_eq!(image.get_tile(0, 1), Ok(None));
    assert!(image.get_tile(1, 1).expect("get tile").is_some());

    let tile_count = gpu.drain_and_execute(&queue).expect("drain");
    assert_eq!(tile_count, 1);
}

#[test]
fn max_layers_limits_total_capacity() {
    let (device, _queue) = create_device_queue();
    let (store, _gpu) = create_store(
        &device,
        TileAtlasConfig {
            max_layers: 1,
            ..TileAtlasConfig::default()
        },
    );

    for _ in 0..TILES_PER_ATLAS {
        store.allocate().expect("allocate within layer capacity");
    }
    assert_eq!(store.allocate(), Err(TileAllocError::AtlasFull));
}

#[test]
fn generic_allocator_keys_are_unique() {
    let (device, _queue) = create_device_queue();
    let (store, _gpu) = create_generic_store_r8u(
        &device,
        GenericTileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC,
            ..GenericTileAtlasConfig::default()
        },
    );

    let key0 = store.allocate().expect("allocate key0");
    let key1 = store.allocate().expect("allocate key1");
    assert_ne!(key0, key1);
}

#[test]
fn generic_allocator_release_reuses_address() {
    let (device, _queue) = create_device_queue();
    let (store, _gpu) = create_generic_store_r8u(
        &device,
        GenericTileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC,
            ..GenericTileAtlasConfig::default()
        },
    );

    let key0 = store.allocate().expect("allocate key0");
    let address0 = store.resolve(key0).expect("resolve key0");
    assert!(store.release(key0));

    let key1 = store.allocate().expect("allocate key1");
    let address1 = store.resolve(key1).expect("resolve key1");
    assert_eq!(address0, address1);
}

#[test]
fn generic_allocator_capacity_is_bounded_by_layers() {
    let (device, _queue) = create_device_queue();
    let (store, _gpu) = create_generic_store_r8u(
        &device,
        GenericTileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC,
            ..GenericTileAtlasConfig::default()
        },
    );

    for _ in 0..TILES_PER_ATLAS {
        store.allocate().expect("allocate within layer capacity");
    }
    assert_eq!(store.allocate(), Err(TileAllocError::AtlasFull));
}

#[test]
fn generic_tile_set_reserve_resolve_release_lifecycle() {
    let (device, _queue) = create_device_queue();
    let (store, _gpu) = create_generic_store_r8u(
        &device,
        GenericTileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC,
            ..GenericTileAtlasConfig::default()
        },
    );

    let set = store.reserve_tile_set(3).expect("reserve tile set");
    assert_eq!(set.len(), 3);
    let resolved = store.resolve_tile_set(&set).expect("resolve tile set");
    assert_eq!(resolved.len(), 3);
    for (key, _address) in &resolved {
        assert!(store.is_allocated(*key));
    }

    assert_eq!(store.release_tile_set(set).expect("release tile set"), 3);
    for (key, _address) in resolved {
        assert!(!store.is_allocated(key));
    }
}

#[test]
fn generic_tile_set_rejects_duplicate_adopt_keys() {
    let (device, _queue) = create_device_queue();
    let (store, _gpu) = create_generic_store_r8u(
        &device,
        GenericTileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC,
            ..GenericTileAtlasConfig::default()
        },
    );

    let key = store.allocate().expect("allocate key");
    let adopt = store.adopt_tile_set([key, key]);
    assert!(matches!(adopt, Err(TileSetError::DuplicateTileKey)));
}

#[test]
fn generic_tile_set_enforces_store_ownership() {
    let (device, _queue) = create_device_queue();
    let (store_a, _gpu_a) = create_generic_store_r8u(
        &device,
        GenericTileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC,
            ..GenericTileAtlasConfig::default()
        },
    );
    let (store_b, _gpu_b) = create_generic_store_r8u(
        &device,
        GenericTileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC,
            ..GenericTileAtlasConfig::default()
        },
    );

    let set = store_a.reserve_tile_set(1).expect("reserve tile set");
    assert_eq!(
        store_b.resolve_tile_set(&set),
        Err(TileSetError::SetNotOwnedByStore)
    );
}

#[test]
fn clear_tile_set_fails_without_partial_clear_enqueue() {
    let (device, queue) = create_device_queue();
    let (store, gpu) = create_generic_store_r8u(
        &device,
        GenericTileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC,
            ..GenericTileAtlasConfig::default()
        },
    );

    let set = store.reserve_tile_set(2).expect("reserve tile set");
    let keys = set.keys().to_vec();
    let kept_key = keys[0];
    let released_key = keys[1];
    let kept_address = store.resolve(kept_key).expect("resolve kept key");
    let tile_bytes = vec![1u8; (TILE_IMAGE as usize) * (TILE_IMAGE as usize)];

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: gpu.texture(),
            mip_level: 0,
            origin: tile_origin(kept_address),
            aspect: wgpu::TextureAspect::All,
        },
        &tile_bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(TILE_IMAGE),
            rows_per_image: Some(TILE_IMAGE),
        },
        wgpu::Extent3d {
            width: TILE_IMAGE,
            height: TILE_IMAGE,
            depth_or_array_layers: 1,
        },
    );

    assert!(store.release(released_key));

    assert_eq!(
        store.clear_tile_set(&set),
        Err(TileSetError::UnknownTileKey)
    );
    let tile_count = gpu.drain_and_execute(&queue).expect("drain clear set");
    assert_eq!(tile_count, 0);

    let kept_tile = read_tile_r8uint(&device, &queue, gpu.texture(), kept_address);
    assert!(kept_tile.iter().all(|&value| value == 1));
}

#[test]
fn clear_tile_set_skips_stale_targets_after_release_and_reuse() {
    let (device, queue) = create_device_queue();
    let (store, gpu) = create_generic_store_r8u(
        &device,
        GenericTileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC,
            ..GenericTileAtlasConfig::default()
        },
    );

    let set = store.reserve_tile_set(1).expect("reserve tile set");
    let original_key = set.keys()[0];
    let original_address = store.resolve(original_key).expect("resolve original key");

    assert_eq!(store.clear_tile_set(&set).expect("enqueue clear set"), 1);

    assert!(store.release(original_key));
    let reused_key = store.allocate().expect("allocate reused key");
    let reused_address = store.resolve(reused_key).expect("resolve reused key");
    assert_eq!(reused_address, original_address);

    let tile_count = gpu
        .drain_and_execute(&queue)
        .expect("drain after stale clear");
    assert_eq!(tile_count, 1);

    let tile_bytes = vec![7u8; (TILE_IMAGE as usize) * (TILE_IMAGE as usize)];
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: gpu.texture(),
            mip_level: 0,
            origin: tile_origin(reused_address),
            aspect: wgpu::TextureAspect::All,
        },
        &tile_bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(TILE_IMAGE),
            rows_per_image: Some(TILE_IMAGE),
        },
        wgpu::Extent3d {
            width: TILE_IMAGE,
            height: TILE_IMAGE,
            depth_or_array_layers: 1,
        },
    );

    let reused_tile = read_tile_r8uint(&device, &queue, gpu.texture(), reused_address);
    assert!(reused_tile.iter().all(|&value| value == 7));
}

#[test]
fn clear_tile_set_reports_total_executed_tile_count() {
    let (device, queue) = create_device_queue();
    let (store, gpu) = create_generic_store_r8u(
        &device,
        GenericTileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC,
            ..GenericTileAtlasConfig::default()
        },
    );

    let set = store.reserve_tile_set(2).expect("reserve tile set");
    assert_eq!(store.clear_tile_set(&set).expect("enqueue clear set"), 2);

    let tile_count = gpu.drain_and_execute(&queue).expect("drain clear set");
    assert_eq!(tile_count, 2);
}

#[test]
fn stale_upload_is_skipped_after_release_and_reuse() {
    let (device, queue) = create_device_queue();
    let (store, gpu) = create_store(
        &device,
        TileAtlasConfig {
            max_layers: 1,
            ..TileAtlasConfig::default()
        },
    );

    let mut bytes = vec![0u8; (TILE_IMAGE as usize) * (TILE_IMAGE as usize) * 4];
    bytes[0] = 255;
    let old_key = store
        .ingest_tile(TILE_IMAGE, TILE_IMAGE, &bytes)
        .expect("ingest old tile")
        .expect("old tile key");
    let old_address = store.resolve(old_key).expect("resolve old key");

    assert!(store.release(old_key));
    let new_key = store.allocate().expect("allocate new key");
    let new_address = store.resolve(new_key).expect("resolve new key");
    assert_eq!(new_address, old_address);

    let tile_count = gpu
        .drain_and_execute(&queue)
        .expect("drain stale upload and clear");
    assert_eq!(tile_count, 1);

    let tile = read_tile_rgba8(&device, &queue, gpu.texture(), new_address);
    assert!(tile.iter().all(|&byte| byte == 0));
}

#[test]
fn release_tile_set_fails_without_partial_release() {
    let (device, _queue) = create_device_queue();
    let (store, _gpu) = create_generic_store_r8u(
        &device,
        GenericTileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC,
            ..GenericTileAtlasConfig::default()
        },
    );

    let set = store.reserve_tile_set(2).expect("reserve tile set");
    let keys = set.keys().to_vec();
    let kept_key = keys[0];
    let released_key = keys[1];

    assert!(store.release(released_key));

    assert_eq!(
        store.release_tile_set(set),
        Err(TileSetError::UnknownTileKey)
    );
    assert!(store.is_allocated(kept_key));
}

#[test]
fn runtime_factory_rejects_payload_format_mismatch() {
    let (device, _queue) = create_device_queue();
    let create = RuntimeGenericTileAtlasStore::with_config(
        &device,
        RuntimeGenericTileAtlasConfig {
            max_layers: 1,
            payload_kind: TilePayloadKind::R32Float,
            format: TileAtlasFormat::R8Uint,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC
                | TileAtlasUsage::STORAGE_BINDING,
            ..RuntimeGenericTileAtlasConfig::default()
        },
    );
    assert!(matches!(
        create,
        Err(TileAtlasCreateError::UnsupportedPayloadFormat)
    ));
}

#[test]
fn runtime_factory_r8uint_allocate_clear_and_drain() {
    let (device, queue) = create_device_queue();
    let (store, gpu) = RuntimeGenericTileAtlasStore::with_config(
        &device,
        RuntimeGenericTileAtlasConfig {
            max_layers: 1,
            payload_kind: TilePayloadKind::R8Uint,
            format: TileAtlasFormat::R8Uint,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC,
            ..RuntimeGenericTileAtlasConfig::default()
        },
    )
    .expect("create runtime r8uint atlas");

    let key = store.allocate().expect("allocate runtime key");
    let address = store.resolve(key).expect("resolve runtime key");
    assert!(store.clear(key).expect("clear runtime key"));
    let tile_count = gpu.drain_and_execute(&queue).expect("drain runtime ops");
    assert_eq!(tile_count, 1);
    let tile = read_tile_r8uint(&device, &queue, gpu.texture(), address);
    assert!(tile.iter().all(|&value| value == 0));
}

#[test]
fn r32float_config_validation_catches_invalid_format_and_usage() {
    let (device, _queue) = create_device_queue();

    let missing_storage = GenericR32FloatTileAtlasStore::with_config(
        &device,
        GenericTileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC,
            ..GenericTileAtlasConfig::default()
        },
    );
    assert!(matches!(
        missing_storage,
        Err(TileAtlasCreateError::MissingStorageBindingUsage)
    ));
}

#[test]
fn r32float_storage_only_usage_is_allowed() {
    let (device, _queue) = create_device_queue();
    let create = GenericR32FloatTileAtlasStore::with_config(
        &device,
        GenericTileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::STORAGE_BINDING,
            ..GenericTileAtlasConfig::default()
        },
    );

    if supports_r32float_storage(&device) {
        assert!(create.is_ok());
    } else {
        assert!(matches!(
            create,
            Err(TileAtlasCreateError::StorageBindingUnsupportedForFormat)
        ));
    }
}

#[test]
fn r32float_storage_only_clear_fails_fast_at_drain() {
    let (device, queue) = create_device_queue();
    if !supports_r32float_storage(&device) {
        return;
    }

    let (store, gpu) = GenericR32FloatTileAtlasStore::with_config(
        &device,
        GenericTileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::STORAGE_BINDING,
            ..GenericTileAtlasConfig::default()
        },
    )
    .expect("create storage-only r32float atlas");

    let key = store.allocate().expect("allocate key");
    assert!(store.clear(key).expect("enqueue clear key"));
    assert_eq!(
        gpu.drain_and_execute(&queue),
        Err(TileGpuDrainError::MissingCopyDstUsage)
    );
}

#[test]
fn group_atlas_requires_copy_dst_and_texture_binding_usage() {
    let (device, _queue) = create_device_queue();

    let missing_copy_dst = GroupTileAtlasStore::with_config(
        &device,
        TileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::TEXTURE_BINDING,
            ..TileAtlasConfig::default()
        },
    );
    assert!(matches!(
        missing_copy_dst,
        Err(TileAtlasCreateError::MissingCopyDstUsage)
    ));

    let missing_texture_binding = GroupTileAtlasStore::with_config(
        &device,
        TileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::COPY_DST,
            ..TileAtlasConfig::default()
        },
    );
    assert!(matches!(
        missing_texture_binding,
        Err(TileAtlasCreateError::MissingTextureBindingUsage)
    ));
}

#[test]
fn r32float_allocate_resolve_release_lifecycle() {
    let (device, _queue) = create_device_queue();
    if !supports_r32float_storage(&device) {
        let create = GenericR32FloatTileAtlasStore::with_config(
            &device,
            GenericTileAtlasConfig {
                max_layers: 1,
                usage: TileAtlasUsage::TEXTURE_BINDING
                    | TileAtlasUsage::COPY_DST
                    | TileAtlasUsage::COPY_SRC
                    | TileAtlasUsage::STORAGE_BINDING,
                ..GenericTileAtlasConfig::default()
            },
        );
        assert!(matches!(
            create,
            Err(TileAtlasCreateError::StorageBindingUnsupportedForFormat)
        ));
        return;
    }

    let (store, _gpu) = create_generic_store_r32f(
        &device,
        GenericTileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC
                | TileAtlasUsage::STORAGE_BINDING,
            ..GenericTileAtlasConfig::default()
        },
    );

    let key = store.allocate().expect("allocate key");
    assert!(store.is_allocated(key));
    assert!(store.resolve(key).is_some());
    assert!(store.release(key));
    assert!(!store.is_allocated(key));
    assert!(store.resolve(key).is_none());
}

#[test]
fn r32float_clear_enqueues_and_zeroes_tile() {
    let (device, queue) = create_device_queue();
    if !supports_r32float_storage(&device) {
        let create = GenericR32FloatTileAtlasStore::with_config(
            &device,
            GenericTileAtlasConfig {
                max_layers: 1,
                usage: TileAtlasUsage::TEXTURE_BINDING
                    | TileAtlasUsage::COPY_DST
                    | TileAtlasUsage::COPY_SRC
                    | TileAtlasUsage::STORAGE_BINDING,
                ..GenericTileAtlasConfig::default()
            },
        );
        assert!(matches!(
            create,
            Err(TileAtlasCreateError::StorageBindingUnsupportedForFormat)
        ));
        return;
    }

    let (store, gpu) = create_generic_store_r32f(
        &device,
        GenericTileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC
                | TileAtlasUsage::STORAGE_BINDING,
            ..GenericTileAtlasConfig::default()
        },
    );

    let key = store.allocate().expect("allocate key");
    let address = store.resolve(key).expect("resolve key");

    let ones = vec![1u8; rgba8_tile_len()];
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: gpu.texture(),
            mip_level: 0,
            origin: tile_origin(address),
            aspect: wgpu::TextureAspect::All,
        },
        &ones,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(TILE_IMAGE * 4),
            rows_per_image: Some(TILE_IMAGE),
        },
        wgpu::Extent3d {
            width: TILE_IMAGE,
            height: TILE_IMAGE,
            depth_or_array_layers: 1,
        },
    );

    assert_eq!(store.clear(key).expect("clear key"), true);
    let tile_count = gpu.drain_and_execute(&queue).expect("drain clear");
    assert_eq!(tile_count, 1);
    let tile = read_tile_r32float(&device, &queue, gpu.texture(), address);
    assert!(tile.iter().all(|&value| value == 0.0));
}

#[test]
fn r8uint_create_and_allocate_release_path() {
    let (device, queue) = create_device_queue();
    let (store, gpu) = create_generic_store_r8u(
        &device,
        GenericTileAtlasConfig {
            max_layers: 1,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC,
            ..GenericTileAtlasConfig::default()
        },
    );

    let key = store.allocate().expect("allocate key");
    let address = store.resolve(key).expect("resolve key");
    assert!(store.release(key));
    let key2 = store.allocate().expect("allocate key2");
    assert_eq!(store.resolve(key2).expect("resolve key2"), address);

    assert_eq!(store.clear(key2).expect("clear key2"), true);
    let tile_count = gpu.drain_and_execute(&queue).expect("drain clear");
    assert_eq!(tile_count, 2);
    let tile = read_tile_r8uint(&device, &queue, gpu.texture(), address);
    assert!(tile.iter().all(|&value| value == 0));
}

#[test]
fn tile_address_helpers_match_tile_origin_math() {
    let address = TileAddress {
        atlas_layer: 2,
        tile_index: (TILES_PER_ROW + 3) as u16,
    };

    assert_eq!(address.tile_x(), 3);
    assert_eq!(address.tile_y(), 1);

    let default_layout = TileAtlasLayout {
        tiles_per_row: TILES_PER_ROW,
        tiles_per_column: TILES_PER_ROW,
        atlas_width: ATLAS_SIZE,
        atlas_height: ATLAS_SIZE,
    };
    let (u, v) = address.atlas_uv_origin_in(default_layout);
    assert!((u - ((3 * TILE_STRIDE + TILE_GUTTER) as f32 / (ATLAS_SIZE as f32))).abs() < 1e-6);
    assert!((v - ((TILE_STRIDE + TILE_GUTTER) as f32 / (ATLAS_SIZE as f32))).abs() < 1e-6);

    let origin = tile_origin(address);
    assert_eq!(origin.x, 3 * TILE_STRIDE + TILE_GUTTER);
    assert_eq!(origin.y, TILE_STRIDE + TILE_GUTTER);
    assert_eq!(origin.z, 2);
}

#[test]
fn tile_address_layout_aware_helpers_support_square_atlas() {
    let layout = TileAtlasLayout {
        tiles_per_row: 8,
        tiles_per_column: 8,
        atlas_width: 8 * TILE_STRIDE,
        atlas_height: 8 * TILE_STRIDE,
    };
    let address = TileAddress {
        atlas_layer: 0,
        tile_index: (layout.tiles_per_row + 3) as u16,
    };

    assert_eq!(address.tile_x_in(layout), 3);
    assert_eq!(address.tile_y_in(layout), 1);

    let (slot_x, slot_y) = address.atlas_slot_origin_pixels_in(layout);
    assert_eq!(slot_x, 3 * TILE_STRIDE);
    assert_eq!(slot_y, TILE_STRIDE);

    let (content_x, content_y) = address.atlas_content_origin_pixels_in(layout);
    assert_eq!(content_x, 3 * TILE_STRIDE + TILE_GUTTER);
    assert_eq!(content_y, TILE_STRIDE + TILE_GUTTER);

    let (u, v) = address.atlas_uv_origin_in(layout);
    assert!((u - (content_x as f32 / layout.atlas_width as f32)).abs() < 1e-6);
    assert!((v - (content_y as f32 / layout.atlas_height as f32)).abs() < 1e-6);
}

#[test]
fn virtual_image_iter_tiles_skips_empty_and_preserves_tile_coordinates() {
    let mut image = VirtualImage::<u8>::new(TILE_IMAGE * 2, TILE_IMAGE * 2).expect("new image");
    image.set_tile(1, 0, 7).expect("set tile 1,0");
    image.set_tile(0, 1, 9).expect("set tile 0,1");

    let tiles: Vec<(u32, u32, u8)> = image
        .iter_tiles()
        .map(|(tile_x, tile_y, value)| (tile_x, tile_y, *value))
        .collect();

    assert_eq!(tiles, vec![(1, 0, 7), (0, 1, 9)]);
}

#[test]
fn new_export_is_transparent_black() {
    let image = VirtualImage::<u8>::new(17, 9).expect("new image");
    let bytes = image
        .export_rgba8(|_key| panic!("no tiles to load"))
        .expect("export");
    assert_eq!(bytes.len(), 17 * 9 * 4);
    assert!(bytes.iter().all(|&byte| byte == 0));
}

#[test]
fn force_release_all_keys_releases_allocated_tiles_and_is_idempotent() {
    let (device, _queue) = create_device_queue();
    let (store, _gpu) = create_store(
        &device,
        TileAtlasConfig {
            max_layers: 1,
            ..TileAtlasConfig::default()
        },
    );

    let key0 = store.allocate().expect("allocate key0");
    let key1 = store.allocate().expect("allocate key1");
    let key2 = store.allocate().expect("allocate key2");
    assert!(store.is_allocated(key0));
    assert!(store.is_allocated(key1));
    assert!(store.is_allocated(key2));

    let released = store.force_release_all_keys();
    assert_eq!(released, 3);
    assert!(!store.is_allocated(key0));
    assert!(!store.is_allocated(key1));
    assert!(!store.is_allocated(key2));

    let released_again = store.force_release_all_keys();
    assert_eq!(released_again, 0);
}

#[test]
fn gc_eviction_batches_are_reported_via_store_drain() {
    let (device, _queue) = create_device_queue();
    let (store, _gpu) = create_store(
        &device,
        TileAtlasConfig {
            max_layers: 1,
            tiles_per_row: 2,
            tiles_per_column: 2,
            ..TileAtlasConfig::default()
        },
    );

    let key0 = store.allocate().expect("allocate key0");
    let key1 = store.allocate().expect("allocate key1");
    let retain_id = store.retain_keys_new_batch(&[key0, key1]);

    assert!(store.drain_evicted_retain_batches().is_empty());

    let fill0 = store.allocate().expect("allocate fill0");
    let fill1 = store.allocate().expect("allocate fill1");
    assert!(store.is_allocated(fill0));
    assert!(store.is_allocated(fill1));

    let replacement = store.allocate().expect("allocate with gc retain eviction");
    assert!(store.is_allocated(replacement));
    assert!(!store.is_allocated(key0));
    assert!(!store.is_allocated(key1));

    let evicted = store.drain_evicted_retain_batches();
    assert_eq!(evicted.len(), 1);
    assert_eq!(evicted[0].retain_id, retain_id);
    assert_eq!(evicted[0].keys.len(), 2);
    assert!(evicted[0].keys.contains(&key0));
    assert!(evicted[0].keys.contains(&key1));
    assert!(store.drain_evicted_retain_batches().is_empty());
}

#[test]
fn brush_buffer_registry_releases_tiles_on_merge_failed() {
    let (device, _queue) = create_device_queue();
    let (store, _gpu) = create_store(
        &device,
        TileAtlasConfig {
            max_layers: 1,
            ..TileAtlasConfig::default()
        },
    );
    let mut registry = BrushBufferTileRegistry::default();

    registry
        .allocate_tiles(
            700,
            [BufferTileCoordinate {
                tile_x: 0,
                tile_y: 0,
            }],
            &store,
        )
        .expect("allocate brush buffer tile");

    let mut retained_keys = Vec::new();
    registry.visit_tiles(700, |_, tile_key| retained_keys.push(tile_key));
    assert_eq!(retained_keys.len(), 1);
    assert!(store.is_allocated(retained_keys[0]));

    registry.release_stroke_on_merge_failed(700, &store);
    assert!(!store.is_allocated(retained_keys[0]));
}

#[test]
fn brush_buffer_registry_applies_retained_eviction() {
    let (device, _queue) = create_device_queue();
    let (store, _gpu) = create_store(
        &device,
        TileAtlasConfig {
            max_layers: 1,
            ..TileAtlasConfig::default()
        },
    );
    let mut registry = BrushBufferTileRegistry::default();

    registry
        .allocate_tiles(
            701,
            [
                BufferTileCoordinate {
                    tile_x: 0,
                    tile_y: 0,
                },
                BufferTileCoordinate {
                    tile_x: 1,
                    tile_y: 0,
                },
            ],
            &store,
        )
        .expect("allocate brush buffer tiles");
    let retain_id = registry.retain_stroke_tiles(701, &store);

    let mut retained_keys = Vec::new();
    registry.visit_tiles(701, |_, tile_key| retained_keys.push(tile_key));
    assert_eq!(retained_keys.len(), 2);

    let evicted_stroke = registry.apply_retained_eviction(retain_id, &retained_keys);
    assert_eq!(evicted_stroke, Some(701));

    let visit_after_eviction = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        registry.visit_tiles(701, |_, _| {});
    }));
    assert!(
        visit_after_eviction.is_err(),
        "stroke mapping must be dropped after full retained eviction"
    );
}
