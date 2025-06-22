use wgpu::include_wgsl;
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
    acceleration_structure_ctx.tlas.linked_pipeline = Some(pipeline);
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
}
