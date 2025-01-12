use wgpu::{include_wgsl, BindGroupDescriptor, BindGroupEntry, BindingResource, ComputePassDescriptor, ComputePipelineDescriptor};
use wgpu_macros::gpu_test;
use wgpu_test::{fail, GpuTestConfiguration, TestParameters, TestingContext};
use wgt::{CommandEncoderDescriptor, Extent3d, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages};

#[gpu_test]
static GENERIC_TEXTURE_USE: GpuTestConfiguration = GpuTestConfiguration::new()
    .parameters(
        TestParameters::default()
            .test_features_limits()
            .features(wgpu::Features::GENERIC_STORAGE_TEXTURES)
    )
    .run_sync(generic_texture_use);

fn generic_texture_use(ctx: TestingContext) {
    let texture = ctx.device.create_texture(&TextureDescriptor {
        label: None,
        size: Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: TextureUsages::STORAGE_BINDING,
        view_formats: &[],
    });
    let module = ctx.device.create_shader_module(include_wgsl!("generic_textures.wgsl"));
    let pipeline = ctx.device.create_compute_pipeline(&ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: &module,
        entry_point: None,
        compilation_options: Default::default(),
        cache: None,
    });
    let bg = ctx.device.create_bind_group(&BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            BindGroupEntry {
                binding: 0,
                resource: BindingResource::TextureView(&texture.create_view(&Default::default())),
            }
        ],
    });
    let mut encoder = ctx.device.create_command_encoder(&CommandEncoderDescriptor {
        label: Some("generic texture encoder"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&ComputePassDescriptor {
            label: None,
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, Some(&bg), &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    ctx.queue.submit([encoder.finish()]);
}

#[gpu_test]
static INVALID_GENERIC_TEXTURE_CREATE: GpuTestConfiguration = GpuTestConfiguration::new()
    .parameters(
        TestParameters::default()
            .test_features_limits()
            .features(wgpu::Features::GENERIC_STORAGE_TEXTURES)
    )
    .run_sync(invalid_generic_texture_create);

fn invalid_generic_texture_create(ctx: TestingContext) {
    fail(&ctx.device, || ctx.device.create_texture(&TextureDescriptor {
        label: None,
        size: Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::F32,
        usage: TextureUsages::STORAGE_BINDING,
        view_formats: &[],
    }), None);
    fail(&ctx.device, || ctx.device.create_texture(&TextureDescriptor {
        label: None,
        size: Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: TextureUsages::STORAGE_BINDING,
        view_formats: &[TextureFormat::F32],
    }), None);
}