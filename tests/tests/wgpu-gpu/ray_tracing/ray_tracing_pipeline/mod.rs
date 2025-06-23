use wgpu::include_wgsl;
use wgpu::util::DeviceExt;
use wgpu::wgt::AccelerationStructureFlags;
use wgpu_macros::gpu_test;
use wgpu_test::{FailureCase, GpuTestConfiguration, TestParameters, TestingContext};

#[gpu_test]
static RAY_TRACING_PIPELINE: GpuTestConfiguration = GpuTestConfiguration::new()
    .parameters(
        TestParameters::default()
            .test_features_limits()
            .features(
                wgpu::Features::EXPERIMENTAL_RAY_TRACING_ACCELERATION_STRUCTURE
                    | wgpu::Features::EXPERIMENTAL_RAY_TRACING_PIPELINES,
            )
            // https://github.com/gfx-rs/wgpu/issues/6727
            .skip(FailureCase::backend_adapter(wgpu::Backends::VULKAN, "AMD")),
    )
    .run_sync(ray_tracing_pipelines);

fn ray_tracing_pipelines(ctx: TestingContext) {
    let shader = ctx
        .device
        .create_shader_module(include_wgsl!("ray_tracing_pipeline.wgsl"));
    let pipeline = ctx
        .device
        .create_ray_tracing_pipeline(&wgpu::RayTracingPipelineDescriptor {
            label: None,
            layout: None,
            ray_generation_module: &shader,
            ray_generation_entry_point: None,
            ray_generation_compilation_options: Default::default(),
            ray_miss_module: &shader,
            ray_miss_entry_point: None,
            ray_miss_compilation_options: Default::default(),
            hit_groups: vec![wgpu::RayTracingHitGroup {
                ray_closest_hit: wgpu::RayClosestHitState {
                    module: &shader,
                    entry_point: None,
                    compilation_options: Default::default(),
                },
                ray_any_hit: Some(wgpu::RayAnyHitState {
                    module: &shader,
                    entry_point: None,
                    compilation_options: Default::default(),
                }),
            }],
            max_recursion_depth: 1,
            cache: None,
        });
    let mut acceleration_structure_ctx = super::AsBuildContext::new(
        &ctx,
        AccelerationStructureFlags::empty(),
        AccelerationStructureFlags::empty(),
    );
    acceleration_structure_ctx.tlas.linked_pipeline = Some(pipeline.clone());
    acceleration_structure_ctx.tlas[0]
        .as_mut()
        .unwrap()
        .linked_pipeline_hit_group_index = Some(0);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.build_acceleration_structures(
        [&acceleration_structure_ctx.blas_build_entry()],
        [&acceleration_structure_ctx.tlas],
    );
    ctx.queue.submit(Some(encoder.finish()));

    #[repr(C)]
    #[derive(Debug, Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
    struct Rays {
        origin: [f32; 3],
        _pad_1: u32,
        direction: [f32; 3],
        _pad_2: u32,
    }

    let rays_to_trace = &[
        Rays {
            origin: [0.0, 0.0, 0.0],
            _pad_1: 0,
            direction: [0.0, 0.0, 1.0],
            _pad_2: 0,
        },
        Rays {
            origin: [0.0, 1000.0, 0.0],
            _pad_1: 0,
            direction: [0.0, 0.0, 1.0],
            _pad_2: 0,
        },
    ];

    let ray_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(rays_to_trace),
            usage: wgpu::BufferUsages::STORAGE,
        });

    let bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::AccelerationStructure(
                    &acceleration_structure_ctx.tlas,
                ),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Buffer(ray_buffer.as_entire_buffer_binding()),
            },
        ],
    });

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut rt_pass = encoder.begin_ray_tracing_pass(&wgpu::RayTracingPassDescriptor {
            label: None,
            timestamp_writes: None,
        });

        rt_pass.set_pipeline(&pipeline);
        rt_pass.set_bind_group(0, &bind_group, &[]);
        rt_pass.trace_rays(2, 1, 1);
    }
    ctx.queue.submit(Some(encoder.finish()));
}
