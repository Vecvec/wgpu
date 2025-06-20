use alloc::vec::Vec;

#[cfg(custom)]
use crate::custom;
use crate::{
    dispatch, BindGroupLayout, Label, PipelineCache, PipelineCompilationOptions, PipelineLayout,
    ShaderModule,
};

/// Handle to a ray tracing pipeline.
///
/// A `RayTracingPipeline` object represents a ray tracing pipeline and its shader stages.
/// It can be created with [`Device::create_ray_tracing_pipeline`].
///
/// Native only
#[derive(Debug, Clone)]
pub struct RayTracingPipeline {
    pub(crate) inner: dispatch::DispatchRayTracingPipeline,
}

#[cfg(send_sync)]
static_assertions::assert_impl_all!(RayTracingPipeline: Send, Sync);

crate::cmp::impl_eq_ord_hash_proxy!(RayTracingPipeline => .inner);

impl RayTracingPipeline {
    /// Get an object representing the bind group layout at a given index.
    ///
    /// If this pipeline was created with a [default layout][ComputePipelineDescriptor::layout],
    /// then bind groups created with the returned `BindGroupLayout` can only be used with this
    /// pipeline.
    ///
    /// This method will raise a validation error if there is no bind group layout at `index`.
    pub fn get_bind_group_layout(&self, index: u32) -> BindGroupLayout {
        let bind_group = self.inner.get_bind_group_layout(index);
        BindGroupLayout { inner: bind_group }
    }

    #[cfg(custom)]
    /// Returns custom implementation of ComputePipeline (if custom backend and is internally T)
    pub fn as_custom<T: custom::RayTracingPipelineInterface>(&self) -> Option<&T> {
        self.inner.as_custom()
    }
}

/// Describes the shader for processing a hit which is the closest
#[derive(Clone, Debug)]
pub struct RayClosestHitState<'a> {
    /// The compiled shader module for the closest hit stage.
    pub module: &'a ShaderModule,
    /// The name of the closest hit entry point in the compiled shader to use.
    ///
    /// If [`Some`], there must be a closest hit shader entry point with this name in `module`.
    /// Otherwise, expect exactly one closest hit shader entry point in `module`, which will be
    /// selected.
    // NOTE: keep phrasing in sync. with `FragmentState::entry_point`
    // NOTE: keep phrasing in sync. with `VertexState::entry_point`
    pub entry_point: Option<&'a str>,
    /// Advanced options for when this pipeline is compiled
    ///
    /// This implements `Default`, and for most users can be set to `Default::default()`
    pub compilation_options: PipelineCompilationOptions<'a>,
}

/// Describes the shader for processing any hit that a ray intersects
#[derive(Clone, Debug)]
pub struct RayAnyHitState<'a> {
    /// The compiled shader module for the any hit stage.
    pub module: &'a ShaderModule,
    /// The name of the closest hit entry point in the compiled shader to use.
    ///
    /// If [`Some`], there must be an any hit shader entry point with this name in `module`.
    /// Otherwise, expect exactly one any hit shader entry point in `module`, which will be
    /// selected.
    // NOTE: keep phrasing in sync. with `FragmentState::entry_point`
    // NOTE: keep phrasing in sync. with `VertexState::entry_point`
    pub entry_point: Option<&'a str>,
    /// Advanced options for when this pipeline is compiled
    ///
    /// This implements `Default`, and for most users can be set to `Default::default()`
    pub compilation_options: PipelineCompilationOptions<'a>,
}

/// Describes the shaders for processing a hit
#[derive(Clone, Debug)]
pub struct RayTracingHitGroup<'a> {
    /// The shader that gets invoked once a particular hit is found to be the closest
    pub ray_closest_hit: RayClosestHitState<'a>,
    /// The shader that gets invoked on any hit (apart from when certain flags are set).
    pub ray_any_hit: Option<RayAnyHitState<'a>>,
}

/// Describes a ray tracing pipeline.
///
/// For use with [`Device::create_ray_tracing_pipeline`].
///
/// Native only.
#[derive(Clone, Debug)]
pub struct RayTracingPipelineDescriptor<'a> {
    /// Debug label of the pipeline. This will show up in graphics debuggers for easy identification.
    pub label: Label<'a>,
    /// The layout of bind groups for this pipeline.
    ///
    /// If this is set, then [`Device::create_ray_tracing_pipeline`] will raise a validation error if
    /// the layout doesn't match what the shader module(s) expect.
    ///
    /// Using the same [`PipelineLayout`] for many [`RenderPipeline`] or [`ComputePipeline`]
    /// pipelines guarantees that you don't have to rebind any resources when switching between
    /// those pipelines.
    ///
    /// ## Default pipeline layout
    ///
    /// If `layout` is `None`, then the pipeline has a [default layout] created and used instead.
    /// The default layout is deduced from the shader modules.
    ///
    /// You can use [`RayTracingPipeline::get_bind_group_layout`] to create bind groups for use with
    /// the default layout. However, these bind groups cannot be used with any other pipelines. This
    /// is convenient for simple pipelines, but using an explicit layout is recommended in most
    /// cases.
    ///
    /// [default layout]: https://www.w3.org/TR/webgpu/#default-pipeline-layout
    pub layout: Option<&'a PipelineLayout>,
    /// The compiled shader module for the ray generation stage.
    pub ray_generation_module: &'a ShaderModule,
    /// The name of the ray generation entry point in the compiled shader to use.
    ///
    /// If [`Some`], there must be a ray generation shader entry point with this name in `module`.
    /// Otherwise, expect exactly one ray generation shader entry point in `module`, which will be
    /// selected.
    // NOTE: keep phrasing in sync. with `FragmentState::entry_point`
    // NOTE: keep phrasing in sync. with `VertexState::entry_point`
    pub ray_generation_entry_point: Option<&'a str>,
    /// Advanced options for when this pipeline is compiled
    ///
    /// This implements `Default`, and for most users can be set to `Default::default()`
    pub ray_generation_compilation_options: PipelineCompilationOptions<'a>,
    /// The compiled shader module for the ray miss stage.
    pub ray_miss_module: &'a ShaderModule,
    /// The name of the ray miss entry point in the compiled shader to use.
    ///
    /// If [`Some`], there must be a ray miss shader entry point with this name in `module`.
    /// Otherwise, expect exactly one ray miss shader entry point in `module`, which will be
    /// selected.
    // NOTE: keep phrasing in sync. with `FragmentState::entry_point`
    // NOTE: keep phrasing in sync. with `VertexState::entry_point`
    pub ray_miss_entry_point: Option<&'a str>,
    /// Advanced options for when this pipeline is compiled
    ///
    /// This implements `Default`, and for most users can be set to `Default::default()`
    pub ray_miss_compilation_options: PipelineCompilationOptions<'a>,
    /// The shaders for processing hits (different ones can be bound to different [`TlasInstance`]s
    ///
    /// [`TlasInstance`]: crate::TlasInstance
    pub hit_groups: Vec<RayTracingHitGroup<'a>>,
    /// The maximum recursion depth allowed. Must be at least one.
    pub max_recursion_depth: u32,
    /// The pipeline cache to use when creating this pipeline.
    pub cache: Option<&'a PipelineCache>,
}
#[cfg(send_sync)]
static_assertions::assert_impl_all!(RayTracingPipelineDescriptor<'_>: Send, Sync);
