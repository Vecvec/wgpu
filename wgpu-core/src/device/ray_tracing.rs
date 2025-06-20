#[cfg(feature = "trace")]
use crate::device::trace;
use crate::device::{DeviceError, ImplicitPipelineIds, ENTRYPOINT_FAILURE_ERROR};
use crate::hash_utils::FastHashMap;
use crate::pipeline::{
    ResolvedProgrammableStageDescriptor, ResolvedRayTracingHitGroup, ShaderBindingData,
};
use crate::resource::ParentDevice;
use crate::{
    api_log, binding_model,
    device::Device,
    global::Global,
    id::{self, BlasId, TlasId},
    lock::RwLock,
    lock::{rank, Mutex},
    pipeline,
    ray_tracing::BlasPrepareCompactError,
    ray_tracing::{CreateBlasError, CreateTlasError},
    resource,
    resource::{
        BlasCompactCallback, BlasCompactState, Fallible, InvalidResourceError, TrackingData,
    },
    snatch::Snatchable,
    validation, LabelHelpers,
};
use alloc::{string::ToString as _, sync::Arc, vec::Vec};
use core::mem::{size_of, ManuallyDrop};
use hal::AccelerationStructureTriangleIndices;
use alloc::string::String;
use core::ptr;
use wgt::Features;

impl Device {
    fn create_blas(
        self: &Arc<Self>,
        blas_desc: &resource::BlasDescriptor,
        sizes: wgt::BlasGeometrySizeDescriptors,
    ) -> Result<Arc<resource::Blas>, CreateBlasError> {
        self.check_is_valid()?;
        self.require_features(Features::EXPERIMENTAL_RAY_TRACING_ACCELERATION_STRUCTURE)?;

        if blas_desc
            .flags
            .contains(wgt::AccelerationStructureFlags::ALLOW_RAY_HIT_VERTEX_RETURN)
        {
            self.require_features(Features::EXPERIMENTAL_RAY_HIT_VERTEX_RETURN)?;
        }

        let size_info = match &sizes {
            wgt::BlasGeometrySizeDescriptors::Triangles { descriptors } => {
                let mut entries =
                    Vec::<hal::AccelerationStructureTriangles<dyn hal::DynBuffer>>::with_capacity(
                        descriptors.len(),
                    );
                for desc in descriptors {
                    if desc.index_count.is_some() != desc.index_format.is_some() {
                        return Err(CreateBlasError::MissingIndexData);
                    }
                    let indices =
                        desc.index_count
                            .map(|count| AccelerationStructureTriangleIndices::<
                                dyn hal::DynBuffer,
                            > {
                                format: desc.index_format.unwrap(),
                                buffer: None,
                                offset: 0,
                                count,
                            });
                    if !self
                        .features
                        .allowed_vertex_formats_for_blas()
                        .contains(&desc.vertex_format)
                    {
                        return Err(CreateBlasError::InvalidVertexFormat(
                            desc.vertex_format,
                            self.features.allowed_vertex_formats_for_blas(),
                        ));
                    }

                    let mut transform = None;

                    if blas_desc
                        .flags
                        .contains(wgt::AccelerationStructureFlags::USE_TRANSFORM)
                    {
                        transform = Some(wgpu_hal::AccelerationStructureTriangleTransform {
                            buffer: self.zero_buffer.as_ref(),
                            offset: 0,
                        })
                    }

                    entries.push(hal::AccelerationStructureTriangles::<dyn hal::DynBuffer> {
                        vertex_buffer: None,
                        vertex_format: desc.vertex_format,
                        first_vertex: 0,
                        vertex_count: desc.vertex_count,
                        vertex_stride: 0,
                        indices,
                        transform,
                        flags: desc.flags,
                    });
                }
                unsafe {
                    self.raw().get_acceleration_structure_build_sizes(
                        &hal::GetAccelerationStructureBuildSizesDescriptor {
                            entries: &hal::AccelerationStructureEntries::Triangles(entries),
                            flags: blas_desc.flags,
                        },
                    )
                }
            }
        };

        let raw = unsafe {
            self.raw()
                .create_acceleration_structure(&hal::AccelerationStructureDescriptor {
                    label: blas_desc.label.as_deref(),
                    size: size_info.acceleration_structure_size,
                    format: hal::AccelerationStructureFormat::BottomLevel,
                    allow_compaction: blas_desc
                        .flags
                        .contains(wgpu_types::AccelerationStructureFlags::ALLOW_COMPACTION),
                })
        }
        .map_err(|e| self.handle_hal_error_with_nonfatal_oom(e))?;

        let compaction_buffer = if blas_desc
            .flags
            .contains(wgpu_types::AccelerationStructureFlags::ALLOW_COMPACTION)
        {
            Some(ManuallyDrop::new(unsafe {
                self.raw()
                    .create_buffer(&hal::BufferDescriptor {
                        label: Some("(wgpu internal) compaction read-back buffer"),
                        size: size_of::<wgpu_types::BufferAddress>() as wgpu_types::BufferAddress,
                        usage: wgpu_types::BufferUses::ACCELERATION_STRUCTURE_QUERY
                            | wgpu_types::BufferUses::MAP_READ,
                        memory_flags: hal::MemoryFlags::PREFER_COHERENT,
                    })
                    .map_err(DeviceError::from_hal)?
            }))
        } else {
            None
        };

        let handle = unsafe {
            self.raw()
                .get_acceleration_structure_device_address(raw.as_ref())
        };

        Ok(Arc::new(resource::Blas {
            raw: Snatchable::new(raw),
            device: self.clone(),
            size_info,
            sizes,
            flags: blas_desc.flags,
            update_mode: blas_desc.update_mode,
            handle,
            label: blas_desc.label.to_string(),
            built_index: RwLock::new(rank::BLAS_BUILT_INDEX, None),
            tracking_data: TrackingData::new(self.tracker_indices.blas_s.clone()),
            compaction_buffer,
            compacted_state: Mutex::new(rank::BLAS_COMPACTION_STATE, BlasCompactState::Idle),
        }))
    }

    fn create_tlas(
        self: &Arc<Self>,
        desc: &resource::TlasDescriptor,
    ) -> Result<Arc<resource::Tlas>, CreateTlasError> {
        self.check_is_valid()?;
        self.require_features(Features::EXPERIMENTAL_RAY_TRACING_ACCELERATION_STRUCTURE)?;

        if desc
            .flags
            .contains(wgt::AccelerationStructureFlags::USE_TRANSFORM)
        {
            return Err(CreateTlasError::DisallowedFlag(
                wgt::AccelerationStructureFlags::USE_TRANSFORM,
            ));
        }

        if desc
            .flags
            .contains(wgt::AccelerationStructureFlags::ALLOW_RAY_HIT_VERTEX_RETURN)
        {
            self.require_features(Features::EXPERIMENTAL_RAY_HIT_VERTEX_RETURN)?;
        }

        let size_info = unsafe {
            self.raw().get_acceleration_structure_build_sizes(
                &hal::GetAccelerationStructureBuildSizesDescriptor {
                    entries: &hal::AccelerationStructureEntries::Instances(
                        hal::AccelerationStructureInstances {
                            buffer: None,
                            offset: 0,
                            count: desc.max_instances,
                        },
                    ),
                    flags: desc.flags,
                },
            )
        };

        let raw = unsafe {
            self.raw()
                .create_acceleration_structure(&hal::AccelerationStructureDescriptor {
                    label: desc.label.as_deref(),
                    size: size_info.acceleration_structure_size,
                    format: hal::AccelerationStructureFormat::TopLevel,
                    allow_compaction: false,
                })
        }
        .map_err(|e| self.handle_hal_error_with_nonfatal_oom(e))?;

        let instance_buffer_size =
            self.alignments.raw_tlas_instance_size * desc.max_instances.max(1) as usize;
        let instance_buffer = unsafe {
            self.raw().create_buffer(&hal::BufferDescriptor {
                label: Some("(wgpu-core) instances_buffer"),
                size: instance_buffer_size as u64,
                usage: wgt::BufferUses::COPY_DST
                    | wgt::BufferUses::TOP_LEVEL_ACCELERATION_STRUCTURE_INPUT,
                memory_flags: hal::MemoryFlags::PREFER_COHERENT,
            })
        }
        .map_err(|e| self.handle_hal_error_with_nonfatal_oom(e))?;

        Ok(Arc::new(resource::Tlas {
            raw: Snatchable::new(raw),
            device: self.clone(),
            size_info,
            flags: desc.flags,
            update_mode: desc.update_mode,
            built_index: RwLock::new(rank::TLAS_BUILT_INDEX, None),
            dependencies: RwLock::new(rank::TLAS_DEPENDENCIES, Vec::new()),
            instance_buffer: ManuallyDrop::new(instance_buffer),
            label: desc.label.to_string(),
            max_instance_count: desc.max_instances,
            tracking_data: TrackingData::new(self.tracker_indices.tlas_s.clone()),
        }))
    }

    pub(crate) fn create_ray_tracing_pipeline(
        self: &Arc<Self>,
        desc: pipeline::ResolvedRayTracingPipelineDescriptor,
    ) -> Result<Arc<pipeline::RayTracingPipeline>, pipeline::CreateRayTracingPipelineError> {
        self.check_is_valid()?;

        self.require_features(Features::EXPERIMENTAL_RAY_TRACING_PIPELINES)?;

        let is_auto_layout = desc.layout.is_none();

        let pipeline_layout = match desc.layout {
            Some(pipeline_layout) => {
                pipeline_layout.same_device(self)?;
                Some(pipeline_layout)
            }
            None => None,
        };

        let mut binding_layout_source = match pipeline_layout {
            Some(ref pipeline_layout) => {
                validation::BindingLayoutSource::Provided(pipeline_layout.get_binding_maps())
            }
            None => validation::BindingLayoutSource::new_derived(&self.limits),
        };

        let mut shader_binding_sizes = FastHashMap::default();
        let mut io = validation::StageIo::default();
        let mut validated_stages = wgt::ShaderStages::empty();

        let ray_gen_entry_point_name;
        let ray_gen_stage = {
            let stage_desc = &desc.ray_generation_stage;
            let stage = wgt::ShaderStages::RAY_GENERATION;

            let ray_gen_shader_module = &stage_desc.module;
            ray_gen_shader_module.same_device(self)?;

            let stage_err = |error| pipeline::CreateRayTracingPipelineError::Stage { stage, error };

            ray_gen_entry_point_name = ray_gen_shader_module
                .finalize_entry_point_name(
                    stage,
                    stage_desc.entry_point.as_ref().map(|ep| ep.as_ref()),
                )
                .map_err(stage_err)?;

            if let Some(ref interface) = ray_gen_shader_module.interface {
                let _ = interface
                    .check_stage(
                        &mut binding_layout_source,
                        &mut shader_binding_sizes,
                        &ray_gen_entry_point_name,
                        stage,
                        io,
                        None,
                    )
                    .map_err(stage_err)?;
                validated_stages |= stage;
            }

            hal::ProgrammableStage {
                module: ray_gen_shader_module.raw(),
                entry_point: &ray_gen_entry_point_name,
                constants: &stage_desc.constants,
                zero_initialize_workgroup_memory: stage_desc.zero_initialize_workgroup_memory,
            }
        };
        io = validation::StageIo::default();

        let ray_miss_entry_point_name;
        let ray_miss_stage = {
            let stage_desc = &desc.ray_miss_stage;
            let stage = wgt::ShaderStages::RAY_MISS;

            let ray_miss_shader_module = &stage_desc.module;
            ray_miss_shader_module.same_device(self)?;

            let stage_err = |error| pipeline::CreateRayTracingPipelineError::Stage { stage, error };

            ray_miss_entry_point_name = ray_miss_shader_module
                .finalize_entry_point_name(
                    stage,
                    stage_desc.entry_point.as_ref().map(|ep| ep.as_ref()),
                )
                .map_err(stage_err)?;

            if let Some(ref interface) = ray_miss_shader_module.interface {
                let _ = interface
                    .check_stage(
                        &mut binding_layout_source,
                        &mut shader_binding_sizes,
                        &ray_miss_entry_point_name,
                        stage,
                        io,
                        None,
                    )
                    .map_err(stage_err)?;
                validated_stages |= stage;
            }

            hal::ProgrammableStage {
                module: ray_miss_shader_module.raw(),
                entry_point: &ray_miss_entry_point_name,
                constants: &stage_desc.constants,
                zero_initialize_workgroup_memory: stage_desc.zero_initialize_workgroup_memory,
            }
        };

        let mut ray_intersection_group_entry_point_names =
            Vec::with_capacity(desc.ray_hit_groups.len());

        for ray_hit_group in &desc.ray_hit_groups {
            let stage_desc = &ray_hit_group.ray_closest_hit_stage;
            let stage = wgt::ShaderStages::RAY_CLOSEST_HIT;

            let ray_closest_hit_shader_module = &stage_desc.module;
            ray_closest_hit_shader_module.same_device(self)?;

            let stage_err = |error| pipeline::CreateRayTracingPipelineError::Stage { stage, error };

            let ray_closest_hit_entry_point_name = ray_closest_hit_shader_module
                .finalize_entry_point_name(
                    stage,
                    stage_desc.entry_point.as_ref().map(|ep| ep.as_ref()),
                )
                .map_err(stage_err)?;

            let mut ray_any_hit_entry_point_name = None;
            if let Some(ref stage_desc) = ray_hit_group.ray_any_hit_stage {
                let stage = wgt::ShaderStages::RAY_ANY_HIT;

                let ray_any_hit_shader_module = &stage_desc.module;
                ray_any_hit_shader_module.same_device(self)?;

                let stage_err =
                    |error| pipeline::CreateRayTracingPipelineError::Stage { stage, error };

                ray_any_hit_entry_point_name = Some(
                    ray_any_hit_shader_module
                        .finalize_entry_point_name(
                            stage,
                            stage_desc.entry_point.as_ref().map(|ep| ep.as_ref()),
                        )
                        .map_err(stage_err)?,
                );
            }

            ray_intersection_group_entry_point_names.push((
                ray_closest_hit_entry_point_name,
                ray_any_hit_entry_point_name,
            ));
        }

        let mut ray_intersection_groups = Vec::with_capacity(desc.ray_hit_groups.len());

        for (idx, ray_hit_group) in desc.ray_hit_groups.iter().enumerate() {
            io = validation::StageIo::default();
            let stage_desc = &ray_hit_group.ray_closest_hit_stage;
            let stage = wgt::ShaderStages::RAY_CLOSEST_HIT;

            let ray_closest_hit_shader_module = &stage_desc.module;
            ray_closest_hit_shader_module.same_device(self)?;

            let stage_err = |error| pipeline::CreateRayTracingPipelineError::Stage { stage, error };

            let (ray_closest_hit_entry_point_name, ray_any_hit_entry_point_name) =
                &ray_intersection_group_entry_point_names[idx];

            if let Some(ref interface) = ray_closest_hit_shader_module.interface {
                let _ = interface
                    .check_stage(
                        &mut binding_layout_source,
                        &mut shader_binding_sizes,
                        ray_closest_hit_entry_point_name,
                        stage,
                        io,
                        None,
                    )
                    .map_err(stage_err)?;
                validated_stages |= stage;
            }

            let ray_closest_hit_programmable_stage = hal::ProgrammableStage {
                module: ray_closest_hit_shader_module.raw(),
                entry_point: ray_closest_hit_entry_point_name,
                constants: &stage_desc.constants,
                zero_initialize_workgroup_memory: stage_desc.zero_initialize_workgroup_memory,
            };

            io = validation::StageIo::default();
            let mut ray_any_hit_programmable_stage = None;
            if let Some(ref stage_desc) = ray_hit_group.ray_any_hit_stage {
                let stage = wgt::ShaderStages::RAY_ANY_HIT;

                let ray_any_hit_shader_module = &stage_desc.module;
                ray_any_hit_shader_module.same_device(self)?;

                let stage_err =
                    |error| pipeline::CreateRayTracingPipelineError::Stage { stage, error };

                if let Some(ref interface) = ray_any_hit_shader_module.interface {
                    let _ = interface
                        .check_stage(
                            &mut binding_layout_source,
                            &mut shader_binding_sizes,
                            ray_any_hit_entry_point_name.as_ref().unwrap(),
                            stage,
                            io,
                            None,
                        )
                        .map_err(stage_err)?;
                    validated_stages |= stage;
                }

                ray_any_hit_programmable_stage = Some(hal::ProgrammableStage {
                    module: ray_any_hit_shader_module.raw(),
                    entry_point: ray_any_hit_entry_point_name.as_ref().unwrap(),
                    constants: &stage_desc.constants,
                    zero_initialize_workgroup_memory: stage_desc.zero_initialize_workgroup_memory,
                });
            }

            ray_intersection_groups.push(hal::RayTracingIntersectionGroup {
                ray_closest_hit: ray_closest_hit_programmable_stage,
                ray_any_hit: ray_any_hit_programmable_stage,
            });
        }

        let pipeline_layout = match binding_layout_source {
            validation::BindingLayoutSource::Provided(_) => {
                drop(binding_layout_source);
                pipeline_layout.unwrap()
            }
            validation::BindingLayoutSource::Derived(entries) => {
                self.derive_pipeline_layout(entries)?
            }
        };

        let late_sized_buffer_groups =
            Device::make_late_sized_buffer_groups(&shader_binding_sizes, &pipeline_layout);

        let cache = match desc.cache {
            Some(cache) => {
                cache.same_device(self)?;
                Some(cache)
            }
            None => None,
        };

        let ray_pipeline = unsafe {
            self.raw()
                .create_ray_tracing_pipeline(&hal::RayTracingPipelineDescriptor {
                    label: desc.label.to_hal(self.instance_flags),
                    layout: pipeline_layout.raw(),
                    ray_generation_stage: ray_gen_stage,
                    ray_miss_stage,
                    intersection_groups: ray_intersection_groups,
                    max_recursion_depth: desc.max_recursion_depth,
                    cache: cache.as_ref().map(|it| it.raw()),
                })
                .map_err(|err| match err {
                    hal::PipelineError::Device(error) => {
                        pipeline::CreateRayTracingPipelineError::Device(
                            self.handle_hal_error(error),
                        )
                    }
                    hal::PipelineError::Linkage(stage, msg) => {
                        pipeline::CreateRayTracingPipelineError::Internal { stage, error: msg }
                    }
                    hal::PipelineError::EntryPoint(stage) => {
                        pipeline::CreateRayTracingPipelineError::Internal {
                            stage: hal::auxil::map_naga_stage(stage),
                            error: ENTRYPOINT_FAILURE_ERROR.to_string(),
                        }
                    }
                    hal::PipelineError::PipelineConstants(stage, error) => {
                        pipeline::CreateRayTracingPipelineError::PipelineConstants { stage, error }
                    }
                })?
        };

        let num_hit_groups = desc.ray_hit_groups.len() as u32;

        let shader_modules = {
            // Allocate a vector with a conservative capacity - we will need at least this many, but
            // could need almost double.
            let mut shader_modules = Vec::with_capacity(desc.ray_hit_groups.len() + 2);
            shader_modules.push(desc.ray_generation_stage.module);
            shader_modules.push(desc.ray_miss_stage.module);
            for ray_hit_group in desc.ray_hit_groups {
                shader_modules.push(ray_hit_group.ray_closest_hit_stage.module);
                if let Some(any_hit) = ray_hit_group.ray_any_hit_stage {
                    shader_modules.push(any_hit.module);
                }
            }
            shader_modules
        };

        let mut shader_binding_data = unsafe {
            self.raw()
                .get_shader_binding_data(ray_pipeline.as_ref())
                .map_err(|err| {
                    pipeline::CreateRayTracingPipelineError::Device(self.handle_hal_error(err))
                })?
        };

        let shader_binding_buffer = unsafe {
            let buf = self
                .raw()
                .create_buffer(&hal::BufferDescriptor {
                    label: None,
                    size: shader_binding_data.data.len() as wgt::BufferAddress,
                    usage: wgt::BufferUses::SHADER_BINDING_DATA | wgt::BufferUses::MAP_WRITE,
                    memory_flags: hal::MemoryFlags::empty(),
                })
                .map_err(|err| {
                    pipeline::CreateRayTracingPipelineError::Device(self.handle_hal_error(err))
                })?;
            let mapping = self
                .raw()
                .map_buffer(
                    buf.as_ref(),
                    hal::MemoryRange {
                        start: 0,
                        end: shader_binding_data.data.len() as wgt::BufferAddress,
                    },
                )
                .map_err(|err| {
                    pipeline::CreateRayTracingPipelineError::Device(self.handle_hal_error(err))
                })?;
            if !shader_binding_data.data.is_empty() {
                mapping.ptr.copy_from(
                    ptr::NonNull::new(shader_binding_data.data.as_mut_ptr()).unwrap(),
                    shader_binding_data.data.len(),
                )
            }
            if !mapping.is_coherent {
                self.raw().flush_mapped_ranges(
                    buf.as_ref(),
                    &[hal::MemoryRange {
                        start: 0,
                        end: shader_binding_data.data.len() as wgt::BufferAddress,
                    }],
                );
            }
            buf
        };

        let pipeline = pipeline::RayTracingPipeline {
            raw: ManuallyDrop::new(ray_pipeline),
            layout: pipeline_layout,
            device: self.clone(),
            _shader_modules: shader_modules,
            late_sized_buffer_groups,
            label: desc.label.to_string(),
            tracking_data: TrackingData::new(self.tracker_indices.ray_tracing_pipelines.clone()),
            shader_binding_data: ShaderBindingData {
                buffer: ManuallyDrop::new(shader_binding_buffer),
                ray_generation_offset: shader_binding_data.ray_generation_offset,
                ray_generation_size: shader_binding_data.ray_generation_size,
                ray_miss_offset: shader_binding_data.ray_miss_offset,
                ray_miss_size: shader_binding_data.ray_miss_size,
                ray_hit_offset: shader_binding_data.ray_hit_offset,
                ray_hit_size: shader_binding_data.ray_hit_size,
            },
            num_hit_groups,
        };

        let pipeline = Arc::new(pipeline);

        if is_auto_layout {
            for bgl in pipeline.layout.bind_group_layouts.iter() {
                // `bind_group_layouts` might contain duplicate entries, so we need to ignore the result.
                let _ = bgl
                    .exclusive_pipeline
                    .set(binding_model::ExclusivePipeline::RayTracing(
                        Arc::downgrade(&pipeline),
                    ));
            }
        }

        Ok(pipeline)
    }
}

impl Global {
    pub fn device_create_blas(
        &self,
        device_id: id::DeviceId,
        desc: &resource::BlasDescriptor,
        sizes: wgt::BlasGeometrySizeDescriptors,
        id_in: Option<BlasId>,
    ) -> (BlasId, Option<u64>, Option<CreateBlasError>) {
        profiling::scope!("Device::create_blas");

        let fid = self.hub.blas_s.prepare(id_in);

        let error = 'error: {
            let device = self.hub.devices.get(device_id);

            #[cfg(feature = "trace")]
            if let Some(trace) = device.trace.lock().as_mut() {
                trace.add(trace::Action::CreateBlas {
                    id: fid.id(),
                    desc: desc.clone(),
                    sizes: sizes.clone(),
                });
            }

            let blas = match device.create_blas(desc, sizes) {
                Ok(blas) => blas,
                Err(e) => break 'error e,
            };
            let handle = blas.handle;

            let id = fid.assign(Fallible::Valid(blas));
            api_log!("Device::create_blas -> {id:?}");

            return (id, Some(handle), None);
        };

        let id = fid.assign(Fallible::Invalid(Arc::new(error.to_string())));
        (id, None, Some(error))
    }

    pub fn device_create_tlas(
        &self,
        device_id: id::DeviceId,
        desc: &resource::TlasDescriptor,
        id_in: Option<TlasId>,
    ) -> (TlasId, Option<CreateTlasError>) {
        profiling::scope!("Device::create_tlas");

        let fid = self.hub.tlas_s.prepare(id_in);

        let error = 'error: {
            let device = self.hub.devices.get(device_id);

            #[cfg(feature = "trace")]
            if let Some(trace) = device.trace.lock().as_mut() {
                trace.add(trace::Action::CreateTlas {
                    id: fid.id(),
                    desc: desc.clone(),
                });
            }

            let tlas = match device.create_tlas(desc) {
                Ok(tlas) => tlas,
                Err(e) => break 'error e,
            };

            let id = fid.assign(Fallible::Valid(tlas));
            api_log!("Device::create_tlas -> {id:?}");

            return (id, None);
        };

        let id = fid.assign(Fallible::Invalid(Arc::new(error.to_string())));
        (id, Some(error))
    }

    pub fn blas_drop(&self, blas_id: BlasId) {
        profiling::scope!("Blas::drop");
        api_log!("Blas::drop {blas_id:?}");

        let _blas = self.hub.blas_s.remove(blas_id);

        #[cfg(feature = "trace")]
        if let Ok(blas) = _blas.get() {
            if let Some(t) = blas.device.trace.lock().as_mut() {
                t.add(trace::Action::DestroyBlas(blas_id));
            }
        }
    }

    pub fn tlas_drop(&self, tlas_id: TlasId) {
        profiling::scope!("Tlas::drop");
        api_log!("Tlas::drop {tlas_id:?}");

        let _tlas = self.hub.tlas_s.remove(tlas_id);

        #[cfg(feature = "trace")]
        if let Ok(tlas) = _tlas.get() {
            if let Some(t) = tlas.device.trace.lock().as_mut() {
                t.add(trace::Action::DestroyTlas(tlas_id));
            }
        }
    }

    pub fn blas_prepare_compact_async(
        &self,
        blas_id: BlasId,
        callback: Option<BlasCompactCallback>,
    ) -> Result<crate::SubmissionIndex, BlasPrepareCompactError> {
        profiling::scope!("Blas::prepare_compact_async");
        api_log!("Blas::prepare_compact_async {blas_id:?}");

        let hub = &self.hub;

        let compact_result = match hub.blas_s.get(blas_id).get() {
            Ok(blas) => blas.prepare_compact_async(callback),
            Err(e) => Err((callback, e.into())),
        };

        match compact_result {
            Ok(submission_index) => Ok(submission_index),
            Err((mut callback, err)) => {
                if let Some(callback) = callback.take() {
                    callback(Err(err.clone()));
                }
                Err(err)
            }
        }
    }

    pub fn ready_for_compaction(&self, blas_id: BlasId) -> Result<bool, InvalidResourceError> {
        profiling::scope!("Blas::prepare_compact_async");
        api_log!("Blas::prepare_compact_async {blas_id:?}");

        let hub = &self.hub;

        let blas = hub.blas_s.get(blas_id).get()?;

        let lock = blas.compacted_state.lock();

        Ok(matches!(*lock, BlasCompactState::Ready { .. }))
    }

    pub fn device_create_ray_tracing_pipeline(
        &self,
        device_id: id::DeviceId,
        desc: &pipeline::RayTracingPipelineDescriptor,
        id_in: Option<id::RayTracingPipelineId>,
        implicit_pipeline_ids: Option<ImplicitPipelineIds<'_>>,
    ) -> (
        id::RayTracingPipelineId,
        Option<pipeline::CreateRayTracingPipelineError>,
    ) {
        profiling::scope!("Device::create_ray_tracing_pipeline");

        let hub = &self.hub;

        let missing_implicit_pipeline_ids =
            desc.layout.is_none() && id_in.is_some() && implicit_pipeline_ids.is_none();

        let fid = hub.ray_tracing_pipelines.prepare(id_in);
        let implicit_context = implicit_pipeline_ids.map(|ipi| ipi.prepare(hub));

        let error = 'error: {
            if missing_implicit_pipeline_ids {
                // TODO: categorize this error as API misuse
                break 'error pipeline::ImplicitLayoutError::MissingImplicitPipelineIds.into();
            }

            let device = self.hub.devices.get(device_id);

            #[cfg(feature = "trace")]
            if let Some(ref mut trace) = *device.trace.lock() {
                trace.add(trace::Action::CreateRayTracingPipeline {
                    id: fid.id(),
                    desc: desc.clone(),
                    implicit_context: implicit_context.clone(),
                });
            }

            let layout = desc
                .layout
                .map(|layout| hub.pipeline_layouts.get(layout).get())
                .transpose();
            let layout = match layout {
                Ok(layout) => layout,
                Err(e) => break 'error e.into(),
            };

            let cache = desc
                .cache
                .map(|cache| hub.pipeline_caches.get(cache).get())
                .transpose();
            let cache = match cache {
                Ok(cache) => cache,
                Err(e) => break 'error e.into(),
            };

            let ray_gen = {
                let module = hub
                    .shader_modules
                    .get(desc.ray_generation_stage.module)
                    .get()
                    .map_err(|e| pipeline::CreateRayTracingPipelineError::Stage {
                        stage: wgt::ShaderStages::RAY_GENERATION,
                        error: e.into(),
                    });
                let module = match module {
                    Ok(module) => module,
                    Err(e) => break 'error e,
                };
                ResolvedProgrammableStageDescriptor {
                    module,
                    entry_point: desc.ray_generation_stage.entry_point.clone(),
                    constants: desc.ray_generation_stage.constants.clone(),
                    zero_initialize_workgroup_memory: desc
                        .ray_generation_stage
                        .zero_initialize_workgroup_memory,
                }
            };

            let ray_miss = {
                let module = hub
                    .shader_modules
                    .get(desc.ray_miss_stage.module)
                    .get()
                    .map_err(|e| pipeline::CreateRayTracingPipelineError::Stage {
                        stage: wgt::ShaderStages::RAY_MISS,
                        error: e.into(),
                    });
                let module = match module {
                    Ok(module) => module,
                    Err(e) => break 'error e,
                };
                ResolvedProgrammableStageDescriptor {
                    module,
                    entry_point: desc.ray_miss_stage.entry_point.clone(),
                    constants: desc.ray_miss_stage.constants.clone(),
                    zero_initialize_workgroup_memory: desc
                        .ray_miss_stage
                        .zero_initialize_workgroup_memory,
                }
            };

            let mut ray_hit_groups = Vec::with_capacity(desc.ray_hit_groups.len());

            for ray_hit_group in &desc.ray_hit_groups {
                let closest_hit_module = hub
                    .shader_modules
                    .get(ray_hit_group.ray_closest_hit_stage.module)
                    .get()
                    .map_err(|e| pipeline::CreateRayTracingPipelineError::Stage {
                        stage: wgt::ShaderStages::RAY_CLOSEST_HIT,
                        error: e.into(),
                    });
                let module = match closest_hit_module {
                    Ok(module) => module,
                    Err(e) => break 'error e,
                };
                let resolved_closest_hit = ResolvedProgrammableStageDescriptor {
                    module,
                    entry_point: ray_hit_group.ray_closest_hit_stage.entry_point.clone(),
                    constants: ray_hit_group.ray_closest_hit_stage.constants.clone(),
                    zero_initialize_workgroup_memory: ray_hit_group
                        .ray_closest_hit_stage
                        .zero_initialize_workgroup_memory,
                };
                let mut resolved_any_hit = None;
                if let Some(ref ray_any_hit_stage) = ray_hit_group.ray_any_hit_stage {
                    let any_hit_module = hub
                        .shader_modules
                        .get(ray_any_hit_stage.module)
                        .get()
                        .map_err(|e| pipeline::CreateRayTracingPipelineError::Stage {
                            stage: wgt::ShaderStages::RAY_ANY_HIT,
                            error: e.into(),
                        });
                    let module = match any_hit_module {
                        Ok(module) => module,
                        Err(e) => break 'error e,
                    };
                    resolved_any_hit = Some(ResolvedProgrammableStageDescriptor {
                        module,
                        entry_point: ray_any_hit_stage.entry_point.clone(),
                        constants: ray_any_hit_stage.constants.clone(),
                        zero_initialize_workgroup_memory: ray_any_hit_stage
                            .zero_initialize_workgroup_memory,
                    });
                }
                ray_hit_groups.push(ResolvedRayTracingHitGroup {
                    ray_closest_hit_stage: resolved_closest_hit,
                    ray_any_hit_stage: resolved_any_hit,
                });
            }

            let desc = pipeline::ResolvedRayTracingPipelineDescriptor {
                label: desc.label.clone(),
                layout,
                ray_generation_stage: ray_gen,
                ray_miss_stage: ray_miss,
                ray_hit_groups,
                max_recursion_depth: desc.max_recursion_depth,
                cache,
            };

            let pipeline = match device.create_ray_tracing_pipeline(desc) {
                Ok(pair) => pair,
                Err(e) => break 'error e,
            };

            if let Some(ids) = implicit_context.as_ref() {
                let group_count = pipeline.layout.bind_group_layouts.len();
                if ids.group_ids.len() < group_count {
                    log::error!(
                        "Not enough bind group IDs ({}) specified for the implicit layout ({})",
                        ids.group_ids.len(),
                        group_count
                    );
                    // TODO: categorize this error as API misuse
                    break 'error pipeline::ImplicitLayoutError::MissingIds(group_count as _)
                        .into();
                }

                let mut pipeline_layout_guard = hub.pipeline_layouts.write();
                let mut bgl_guard = hub.bind_group_layouts.write();
                pipeline_layout_guard.insert(ids.root_id, Fallible::Valid(pipeline.layout.clone()));
                let mut group_ids = ids.group_ids.iter();
                // NOTE: If the first iterator is longer than the second, the `.zip()` impl will still advance the
                // the first iterator before realizing that the second iterator has finished.
                // The `pipeline.layout.bind_group_layouts` iterator will always be shorter than `ids.group_ids`,
                // so using it as the first iterator for `.zip()` will work properly.
                for (bgl, bgl_id) in pipeline
                    .layout
                    .bind_group_layouts
                    .iter()
                    .zip(&mut group_ids)
                {
                    bgl_guard.insert(*bgl_id, Fallible::Valid(bgl.clone()));
                }
                for bgl_id in group_ids {
                    bgl_guard.insert(*bgl_id, Fallible::Invalid(Arc::new(String::new())));
                }
            }

            let id = fid.assign(Fallible::Valid(pipeline));
            api_log!("Device::create_ray_tracing_pipeline -> {id:?}");

            return (id, None);
        };

        let id = fid.assign(Fallible::Invalid(Arc::new(desc.label.to_string())));

        // We also need to assign errors to the implicit pipeline layout and the
        // implicit bind group layouts.
        if let Some(ids) = implicit_context {
            let mut pipeline_layout_guard = hub.pipeline_layouts.write();
            let mut bgl_guard = hub.bind_group_layouts.write();
            pipeline_layout_guard.insert(ids.root_id, Fallible::Invalid(Arc::new(String::new())));
            for bgl_id in ids.group_ids {
                bgl_guard.insert(bgl_id, Fallible::Invalid(Arc::new(String::new())));
            }
        }

        (id, Some(error))
    }

    /// Get an ID of one of the bind group layouts. The ID adds a refcount,
    /// which needs to be released by calling `bind_group_layout_drop`.
    pub fn ray_tracing_pipeline_get_bind_group_layout(
        &self,
        pipeline_id: id::RayTracingPipelineId,
        index: u32,
        id_in: Option<id::BindGroupLayoutId>,
    ) -> (
        id::BindGroupLayoutId,
        Option<binding_model::GetBindGroupLayoutError>,
    ) {
        let hub = &self.hub;

        let fid = hub.bind_group_layouts.prepare(id_in);

        let error = 'error: {
            let pipeline = match hub.ray_tracing_pipelines.get(pipeline_id).get() {
                Ok(pipeline) => pipeline,
                Err(e) => break 'error e.into(),
            };

            let id = match pipeline.layout.bind_group_layouts.get(index as usize) {
                Some(bg) => fid.assign(Fallible::Valid(bg.clone())),
                None => {
                    break 'error binding_model::GetBindGroupLayoutError::InvalidGroupIndex(index)
                }
            };

            return (id, None);
        };

        let id = fid.assign(Fallible::Invalid(Arc::new(String::new())));
        (id, Some(error))
    }

    pub fn ray_tracing_pipeline_drop(&self, ray_tracing_pipeline_id: id::RayTracingPipelineId) {
        profiling::scope!("RayTracingPipeline::drop");
        api_log!("RayTracingPipeline::drop {ray_tracing_pipeline_id:?}");

        let hub = &self.hub;

        let _pipeline = hub.ray_tracing_pipelines.remove(ray_tracing_pipeline_id);

        #[cfg(feature = "trace")]
        if let Ok(pipeline) = _pipeline.get() {
            if let Some(t) = pipeline.device.trace.lock().as_mut() {
                t.add(trace::Action::DestroyRayTracingPipeline(
                    ray_tracing_pipeline_id,
                ));
            }
        }
    }
}
