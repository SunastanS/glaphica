fn main() {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });

    let adapters: Vec<wgpu::Adapter> =
        pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    if adapters.is_empty() {
        println!("No wgpu adapters found");
        return;
    }

    for (index, adapter) in adapters.iter().enumerate() {
        let info = adapter.get_info();
        let limits = adapter.limits();

        println!("Adapter #{index}:");
        println!("  name: {}", info.name);
        println!("  backend: {:?}", info.backend);
        println!("  device_type: {:?}", info.device_type);
        println!("  vendor: {:#06x}", info.vendor);
        println!("  device: {:#06x}", info.device);
        println!(
            "  max_texture_dimension_2d: {}",
            limits.max_texture_dimension_2d
        );
        println!(
            "  max_texture_dimension_3d: {}",
            limits.max_texture_dimension_3d
        );
        println!("  max_bind_groups: {}", limits.max_bind_groups);
    }
}
