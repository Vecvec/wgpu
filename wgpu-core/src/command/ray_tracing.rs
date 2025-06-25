use alloc::borrow::Cow;
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    cmp::max,
    fmt,
    num::NonZeroU64,
    ops::{Deref, Range},
};
use thiserror::Error;

use crate::command::memory_init::fixup_discarded_surfaces;
use crate::device::DeviceError;
use crate::{
    binding_model::{LateMinBufferBindingSizeMismatch, PushConstantUploadError},
    command::{
        bind::Binder,
        bind::BinderError,
        memory_init::SurfacesInDiscardState,
        pass,
        query::{end_pipeline_statistics_query, validate_and_begin_pipeline_statistics_query},
        ray_tracing_command::ArcRayTracingCommand,
        ArcPassTimestampWrites, BasePass, BindGroupStateChange, CommandBuffer,
        CommandBufferMutable, CommandEncoderError, EncoderStateError, MapPassErr, PassErrorScope,
        PassTimestampWrites, StateChange,
    },
    device::{queue::TempResource, resource::CommandIndices, MissingDownlevelFlags},
    global::Global,
    hal_label,
    hub::Hub,
    id,
    id::{BlasId, CommandEncoderId, TlasId},
    init_tracker::MemoryInitKind,
    lock::RwLockWriteGuard,
    pipeline::RayTracingPipeline,
    ray_tracing::{
        AsAction, AsBuild, BlasBuildEntry, BlasGeometries, BlasTriangleGeometry,
        BuildAccelerationStructureError, TlasBuild, TlasInstance, TlasPackage, TraceBlasBuildEntry,
        TraceBlasGeometries, TraceBlasTriangleGeometry, TraceTlasInstance, TraceTlasPackage,
        ValidateAsActionsError,
    },
    resource,
    resource::{
        AccelerationStructure, Blas, BlasCompactState, Buffer, InvalidResourceError, Labeled,
        MissingBufferUsageError, ParentDevice, ResourceErrorIdent, StagingBuffer, Tlas,
    },
    scratch::ScratchBuffer,
    snatch::SnatchGuard,
    track::PendingTransition,
    Label,
};
use wgt::{
    math::align_to, BufferAddress, BufferUsages, BufferUses, DynamicOffset, Features, ShaderStages,
};

struct TriangleBufferStore<'a> {
    vertex_buffer: Arc<Buffer>,
    vertex_transition: Option<PendingTransition<BufferUses>>,
    index_buffer_transition: Option<(Arc<Buffer>, Option<PendingTransition<BufferUses>>)>,
    transform_buffer_transition: Option<(Arc<Buffer>, Option<PendingTransition<BufferUses>>)>,
    geometry: BlasTriangleGeometry<'a>,
    ending_blas: Option<Arc<Blas>>,
}

struct BlasStore<'a> {
    blas: Arc<Blas>,
    entries: hal::AccelerationStructureEntries<'a, dyn hal::DynBuffer>,
    scratch_buffer_offset: u64,
}

struct UnsafeTlasStore<'a> {
    tlas: Arc<Tlas>,
    entries: hal::AccelerationStructureEntries<'a, dyn hal::DynBuffer>,
    scratch_buffer_offset: u64,
}

struct TlasStore<'a> {
    internal: UnsafeTlasStore<'a>,
    range: Range<usize>,
}

impl Global {
    pub fn command_encoder_mark_acceleration_structures_built(
        &self,
        command_encoder_id: CommandEncoderId,
        blas_ids: &[BlasId],
        tlas_ids: &[TlasId],
    ) -> Result<(), EncoderStateError> {
        profiling::scope!("CommandEncoder::mark_acceleration_structures_built");

        let hub = &self.hub;

        let cmd_buf = hub
            .command_buffers
            .get(command_encoder_id.into_command_buffer_id());

        let mut cmd_buf_data = cmd_buf.data.lock();
        cmd_buf_data.record_with(
            |cmd_buf_data| -> Result<(), BuildAccelerationStructureError> {
                let device = &cmd_buf.device;
                device
                    .require_features(Features::EXPERIMENTAL_RAY_TRACING_ACCELERATION_STRUCTURE)?;

                let mut build_command = AsBuild::default();

                for blas in blas_ids {
                    let blas = hub.blas_s.get(*blas).get()?;
                    build_command.blas_s_built.push(blas);
                }

                for tlas in tlas_ids {
                    let tlas = hub.tlas_s.get(*tlas).get()?;
                    build_command.tlas_s_built.push(TlasBuild {
                        tlas,
                        dependencies: Vec::new(),
                    });
                }

                cmd_buf_data.as_actions.push(AsAction::Build(build_command));
                Ok(())
            },
        )
    }

    pub fn command_encoder_build_acceleration_structures<'a>(
        &self,
        command_encoder_id: CommandEncoderId,
        blas_iter: impl Iterator<Item = BlasBuildEntry<'a>>,
        tlas_iter: impl Iterator<Item = TlasPackage<'a>>,
    ) -> Result<(), EncoderStateError> {
        profiling::scope!("CommandEncoder::build_acceleration_structures");

        let hub = &self.hub;

        let cmd_buf = hub
            .command_buffers
            .get(command_encoder_id.into_command_buffer_id());

        let mut build_command = AsBuild::default();

        let trace_blas: Vec<TraceBlasBuildEntry> = blas_iter
            .map(|blas_entry| {
                let geometries = match blas_entry.geometries {
                    BlasGeometries::TriangleGeometries(triangle_geometries) => {
                        TraceBlasGeometries::TriangleGeometries(
                            triangle_geometries
                                .map(|tg| TraceBlasTriangleGeometry {
                                    size: tg.size.clone(),
                                    vertex_buffer: tg.vertex_buffer,
                                    index_buffer: tg.index_buffer,
                                    transform_buffer: tg.transform_buffer,
                                    first_vertex: tg.first_vertex,
                                    vertex_stride: tg.vertex_stride,
                                    first_index: tg.first_index,
                                    transform_buffer_offset: tg.transform_buffer_offset,
                                })
                                .collect(),
                        )
                    }
                };
                TraceBlasBuildEntry {
                    blas_id: blas_entry.blas_id,
                    geometries,
                }
            })
            .collect();

        let trace_tlas: Vec<TraceTlasPackage> = tlas_iter
            .map(|package: TlasPackage| {
                let instances = package
                    .instances
                    .map(|instance| {
                        instance.map(|instance| TraceTlasInstance {
                            blas_id: instance.blas_id,
                            transform: *instance.transform,
                            custom_data: instance.custom_data,
                            mask: instance.mask,
                            ray_hit_group_index: instance.ray_hit_group_index,
                        })
                    })
                    .collect();
                TraceTlasPackage {
                    tlas_id: package.tlas_id,
                    instances,
                    lowest_unmodified: package.lowest_unmodified,
                    linked_ray_tracing_pipeline: package.linked_pipeline,
                }
            })
            .collect();

        let blas_iter = trace_blas.iter().map(|blas_entry| {
            let geometries = match &blas_entry.geometries {
                TraceBlasGeometries::TriangleGeometries(triangle_geometries) => {
                    let iter = triangle_geometries.iter().map(|tg| BlasTriangleGeometry {
                        size: &tg.size,
                        vertex_buffer: tg.vertex_buffer,
                        index_buffer: tg.index_buffer,
                        transform_buffer: tg.transform_buffer,
                        first_vertex: tg.first_vertex,
                        vertex_stride: tg.vertex_stride,
                        first_index: tg.first_index,
                        transform_buffer_offset: tg.transform_buffer_offset,
                    });
                    BlasGeometries::TriangleGeometries(Box::new(iter))
                }
            };
            BlasBuildEntry {
                blas_id: blas_entry.blas_id,
                geometries,
            }
        });

        let tlas_iter = trace_tlas.iter().map(|tlas_package| {
            let instances = tlas_package.instances.iter().map(|instance| {
                instance.as_ref().map(|instance| TlasInstance {
                    blas_id: instance.blas_id,
                    transform: &instance.transform,
                    custom_data: instance.custom_data,
                    mask: instance.mask,
                    ray_hit_group_index: instance.ray_hit_group_index,
                })
            });
            TlasPackage {
                tlas_id: tlas_package.tlas_id,
                instances: Box::new(instances),
                lowest_unmodified: tlas_package.lowest_unmodified,
                linked_pipeline: tlas_package.linked_ray_tracing_pipeline,
            }
        });

        let mut cmd_buf_data = cmd_buf.data.lock();
        cmd_buf_data.record_with(|cmd_buf_data| {
            #[cfg(feature = "trace")]
            if let Some(ref mut list) = cmd_buf_data.commands {
                list.push(crate::device::trace::Command::BuildAccelerationStructures {
                    blas: trace_blas.clone(),
                    tlas: trace_tlas.clone(),
                });
            }

            let device = &cmd_buf.device;
            device.require_features(Features::EXPERIMENTAL_RAY_TRACING_ACCELERATION_STRUCTURE)?;

            let mut buf_storage = Vec::new();
            iter_blas(
                blas_iter,
                cmd_buf_data,
                &mut build_command,
                &mut buf_storage,
                hub,
            )?;

            let snatch_guard = device.snatchable_lock.read();
            let mut input_barriers = Vec::<hal::BufferBarrier<dyn hal::DynBuffer>>::new();
            let mut scratch_buffer_blas_size = 0;
            let mut blas_storage = Vec::new();
            iter_buffers(
                &mut buf_storage,
                &snatch_guard,
                &mut input_barriers,
                cmd_buf_data,
                &mut scratch_buffer_blas_size,
                &mut blas_storage,
                hub,
                device.alignments.ray_tracing_scratch_buffer_alignment,
            )?;
            let mut tlas_lock_store = Vec::<(Option<TlasPackage>, Arc<Tlas>)>::new();

            for package in tlas_iter {
                let tlas = hub.tlas_s.get(package.tlas_id).get()?;

                cmd_buf_data.trackers.tlas_s.insert_single(tlas.clone());

                tlas_lock_store.push((Some(package), tlas))
            }

            let mut scratch_buffer_tlas_size = 0;
            let mut tlas_storage = Vec::<TlasStore>::new();
            let mut instance_buffer_staging_source = Vec::<u8>::new();

            for (package, tlas) in &mut tlas_lock_store {
                let package = package.take().unwrap();

                let mut linked_pipeline = None;

                if let Some(pipeline) = package.linked_pipeline {
                    linked_pipeline = Some(hub.ray_tracing_pipelines.get(pipeline).get()?);
                }

                let scratch_buffer_offset = scratch_buffer_tlas_size;
                scratch_buffer_tlas_size += align_to(
                    tlas.size_info.build_scratch_size as u32,
                    device.alignments.ray_tracing_scratch_buffer_alignment,
                ) as u64;

                let first_byte_index = instance_buffer_staging_source.len();

                let mut dependencies = Vec::new();

            let mut instance_count = 0;
            for instance in package.instances.flatten() {
                if instance.custom_data >= (1u32 << 24u32) {
                    return Err(BuildAccelerationStructureError::TlasInvalidCustomIndex(
                        tlas.error_ident(),
                    ));
                }

                match (instance.ray_hit_group_index, linked_pipeline.as_ref()) {
                    (Some(index), Some(pipeline)) => {
                        if index >= pipeline.num_hit_groups {
                            return Err(BuildAccelerationStructureError::TooLargeHitGroupIndex(pipeline.error_ident(), tlas.error_ident(), pipeline.num_hit_groups, index));
                        }
                    }
                    (None, None) => {}
                    _ => return Err(BuildAccelerationStructureError::TlasLinkedPipelineInstanceHitGroupIndexMismatch(tlas.error_ident())),
                }

                let sbt_offset = instance.ray_hit_group_index.unwrap_or(0);

                debug_assert!(sbt_offset < (1 << 24), "SBT offset was extremely high");

                let blas = hub.blas_s.get(instance.blas_id).get()?;

                    cmd_buf_data.trackers.blas_s.insert_single(blas.clone());

                instance_buffer_staging_source.extend(device.raw().tlas_instance_to_bytes(
                    hal::TlasInstance {
                        transform: *instance.transform,
                        custom_data: instance.custom_data,
                        mask: instance.mask,
                        blas_address: blas.handle,
                        shader_binding_table_offset: sbt_offset,
                    },
                ));

                    if tlas.flags.contains(
                        wgpu_types::AccelerationStructureFlags::ALLOW_RAY_HIT_VERTEX_RETURN,
                    ) && !blas.flags.contains(
                        wgpu_types::AccelerationStructureFlags::ALLOW_RAY_HIT_VERTEX_RETURN,
                    ) {
                        return Err(
                            BuildAccelerationStructureError::TlasDependentMissingVertexReturn(
                                tlas.error_ident(),
                                blas.error_ident(),
                            ),
                        );
                    }

                    instance_count += 1;

                    dependencies.push(blas.clone());
                }

                build_command.tlas_s_built.push(TlasBuild {
                    tlas: tlas.clone(),
                    dependencies,
                });

                if instance_count > tlas.max_instance_count {
                    return Err(BuildAccelerationStructureError::TlasInstanceCountExceeded(
                        tlas.error_ident(),
                        instance_count,
                        tlas.max_instance_count,
                    ));
                }

                tlas_storage.push(TlasStore {
                    internal: UnsafeTlasStore {
                        tlas: tlas.clone(),
                        entries: hal::AccelerationStructureEntries::Instances(
                            hal::AccelerationStructureInstances {
                                buffer: Some(tlas.instance_buffer.as_ref()),
                                offset: 0,
                                count: instance_count,
                            },
                        ),
                        scratch_buffer_offset,
                    },
                    range: first_byte_index..instance_buffer_staging_source.len(),
                });
            }

            let Some(scratch_size) =
                wgt::BufferSize::new(max(scratch_buffer_blas_size, scratch_buffer_tlas_size))
            else {
                // if the size is zero there is nothing to build
                return Ok(());
            };

            let scratch_buffer = ScratchBuffer::new(device, scratch_size)?;

            let scratch_buffer_barrier = hal::BufferBarrier::<dyn hal::DynBuffer> {
                buffer: scratch_buffer.raw(),
                usage: hal::StateTransition {
                    from: BufferUses::ACCELERATION_STRUCTURE_SCRATCH,
                    to: BufferUses::ACCELERATION_STRUCTURE_SCRATCH,
                },
            };

            let mut tlas_descriptors = Vec::with_capacity(tlas_storage.len());

            for &TlasStore {
                internal:
                    UnsafeTlasStore {
                        ref tlas,
                        ref entries,
                        ref scratch_buffer_offset,
                    },
                ..
            } in &tlas_storage
            {
                if tlas.update_mode == wgt::AccelerationStructureUpdateMode::PreferUpdate {
                    log::info!("only rebuild implemented")
                }
                tlas_descriptors.push(hal::BuildAccelerationStructureDescriptor {
                    entries,
                    mode: hal::AccelerationStructureBuildMode::Build,
                    flags: tlas.flags,
                    source_acceleration_structure: None,
                    destination_acceleration_structure: tlas.try_raw(&snatch_guard)?,
                    scratch_buffer: scratch_buffer.raw(),
                    scratch_buffer_offset: *scratch_buffer_offset,
                })
            }

            let blas_present = !blas_storage.is_empty();
            let tlas_present = !tlas_storage.is_empty();

            let cmd_buf_raw = cmd_buf_data.encoder.open()?;

            let mut blas_s_compactable = Vec::new();
            let mut descriptors = Vec::new();

            for storage in &blas_storage {
                descriptors.push(map_blas(
                    storage,
                    scratch_buffer.raw(),
                    &snatch_guard,
                    &mut blas_s_compactable,
                )?);
            }

            build_blas(
                cmd_buf_raw,
                blas_present,
                tlas_present,
                input_barriers,
                &descriptors,
                scratch_buffer_barrier,
                blas_s_compactable,
            );

            if tlas_present {
                let staging_buffer = if !instance_buffer_staging_source.is_empty() {
                    let mut staging_buffer = StagingBuffer::new(
                        device,
                        wgt::BufferSize::new(instance_buffer_staging_source.len() as u64).unwrap(),
                    )?;
                    staging_buffer.write(&instance_buffer_staging_source);
                    let flushed = staging_buffer.flush();
                    Some(flushed)
                } else {
                    None
                };

                unsafe {
                    if let Some(ref staging_buffer) = staging_buffer {
                        cmd_buf_raw.transition_buffers(&[
                            hal::BufferBarrier::<dyn hal::DynBuffer> {
                                buffer: staging_buffer.raw(),
                                usage: hal::StateTransition {
                                    from: BufferUses::MAP_WRITE,
                                    to: BufferUses::COPY_SRC,
                                },
                            },
                        ]);
                    }
                }

                let mut instance_buffer_barriers = Vec::new();
                for &TlasStore {
                    internal: UnsafeTlasStore { ref tlas, .. },
                    ref range,
                } in &tlas_storage
                {
                    let size = match wgt::BufferSize::new((range.end - range.start) as u64) {
                        None => continue,
                        Some(size) => size,
                    };
                    instance_buffer_barriers.push(hal::BufferBarrier::<dyn hal::DynBuffer> {
                        buffer: tlas.instance_buffer.as_ref(),
                        usage: hal::StateTransition {
                            from: BufferUses::COPY_DST,
                            to: BufferUses::TOP_LEVEL_ACCELERATION_STRUCTURE_INPUT,
                        },
                    });
                    unsafe {
                        cmd_buf_raw.transition_buffers(&[
                            hal::BufferBarrier::<dyn hal::DynBuffer> {
                                buffer: tlas.instance_buffer.as_ref(),
                                usage: hal::StateTransition {
                                    from: BufferUses::TOP_LEVEL_ACCELERATION_STRUCTURE_INPUT,
                                    to: BufferUses::COPY_DST,
                                },
                            },
                        ]);
                        let temp = hal::BufferCopy {
                            src_offset: range.start as u64,
                            dst_offset: 0,
                            size,
                        };
                        cmd_buf_raw.copy_buffer_to_buffer(
                            // the range whose size we just checked end is at (at that point in time) instance_buffer_staging_source.len()
                            // and since instance_buffer_staging_source doesn't shrink we can un wrap this without a panic
                            staging_buffer.as_ref().unwrap().raw(),
                            tlas.instance_buffer.as_ref(),
                            &[temp],
                        );
                    }
                }

                unsafe {
                    cmd_buf_raw.transition_buffers(&instance_buffer_barriers);

                    cmd_buf_raw.build_acceleration_structures(&tlas_descriptors);

                    cmd_buf_raw.place_acceleration_structure_barrier(
                        hal::AccelerationStructureBarrier {
                            usage: hal::StateTransition {
                                from: hal::AccelerationStructureUses::BUILD_OUTPUT,
                                to: hal::AccelerationStructureUses::SHADER_INPUT,
                            },
                        },
                    );
                }

                if let Some(staging_buffer) = staging_buffer {
                    cmd_buf_data
                        .temp_resources
                        .push(TempResource::StagingBuffer(staging_buffer));
                }
            }

            cmd_buf_data
                .temp_resources
                .push(TempResource::ScratchBuffer(scratch_buffer));

            cmd_buf_data.as_actions.push(AsAction::Build(build_command));

            Ok(())
        })
    }

    /// Creates a ray tracing pass.
    ///
    /// If creation fails, an invalid pass is returned.
    /// Any operation on an invalid pass will return an error.
    ///
    /// If successful, puts the encoder into the [`Locked`] state.
    ///
    /// [`Locked`]: crate::command::CommandEncoderStatus::Locked
    pub fn command_encoder_begin_ray_tracing_pass(
        &self,
        encoder_id: CommandEncoderId,
        desc: &RayTracingPassDescriptor<'_>,
    ) -> (RayTracingPass, Option<CommandEncoderError>) {
        let hub = &self.hub;

        let mut arc_desc = ArcRayTracingPassDescriptor {
            label: desc.label.as_deref().map(Cow::Borrowed),
            timestamp_writes: None, // Handle only once we resolved the encoder.
        };

        let make_err = |e, arc_desc| (RayTracingPass::new(None, arc_desc), Some(e));

        let cmd_buf = hub.command_buffers.get(encoder_id.into_command_buffer_id());

        match cmd_buf.data.lock().lock_encoder() {
            Ok(_) => {}
            Err(e) => return make_err(e.into(), arc_desc),
        };

        arc_desc.timestamp_writes = match desc
            .timestamp_writes
            .as_ref()
            .map(|tw| {
                Self::validate_pass_timestamp_writes(&cmd_buf.device, &hub.query_sets.read(), tw)
            })
            .transpose()
        {
            Ok(ok) => ok,
            Err(e) => return make_err(e, arc_desc),
        };

        (RayTracingPass::new(Some(cmd_buf), arc_desc), None)
    }

    pub fn ray_tracing_pass_end(
        &self,
        pass: &mut RayTracingPass,
    ) -> Result<(), RayTracingPassError> {
        profiling::scope!("CommandEncoder::run_ray_tracing_pass");
        let pass_scope = PassErrorScope::Pass;

        let cmd_buf = pass
            .parent
            .as_ref()
            .ok_or(RayTracingPassErrorInner::InvalidParentEncoder)
            .map_pass_err(pass_scope)?;

        let base = pass
            .base
            .take()
            .ok_or(RayTracingPassErrorInner::PassEnded)
            .map_pass_err(pass_scope)?;

        let device = &cmd_buf.device;
        device.check_is_valid().map_pass_err(pass_scope)?;

        let mut cmd_buf_data = cmd_buf.data.lock();
        let mut cmd_buf_data_guard = cmd_buf_data.unlock_encoder().map_pass_err(pass_scope)?;
        let cmd_buf_data = &mut *cmd_buf_data_guard;

        let encoder = &mut cmd_buf_data.encoder;

        // We automatically keep extending command buffers over time, and because
        // we want to insert a command buffer _before_ what we're about to record,
        // we need to make sure to close the previous one.
        encoder.close_if_open().map_pass_err(pass_scope)?;
        let raw_encoder = encoder
            .open_pass(base.label.as_deref())
            .map_pass_err(pass_scope)?;

        let snatch_guard = device.snatchable_lock.read();

        let mut state = State {
            pipeline: None,

            general: pass::BaseState {
                device,
                raw_encoder,
                tracker: &mut cmd_buf_data.trackers,
                buffer_memory_init_actions: &mut cmd_buf_data.buffer_memory_init_actions,
                texture_memory_actions: &mut cmd_buf_data.texture_memory_actions,
                as_actions: &mut cmd_buf_data.as_actions,
                binder: Binder::new(),
                temp_offsets: Vec::new(),
                dynamic_offset_count: 0,

                pending_discard_init_fixups: SurfacesInDiscardState::new(),

                snatch_guard: &snatch_guard,
                scope: device.new_usage_scope(),

                debug_scope_depth: 0,
                string_offset: 0,
            },
            active_query: None,
        };

        let indices = &state.general.device.tracker_indices;
        state
            .general
            .tracker
            .buffers
            .set_size(indices.buffers.size());
        state
            .general
            .tracker
            .textures
            .set_size(indices.textures.size());

        let timestamp_writes: Option<hal::PassTimestampWrites<'_, dyn hal::DynQuerySet>> =
            if let Some(tw) = pass.timestamp_writes.take() {
                tw.query_set
                    .same_device_as(cmd_buf.as_ref())
                    .map_pass_err(pass_scope)?;

                let query_set = state.general.tracker.query_sets.insert_single(tw.query_set);

                // Unlike in render passes we can't delay resetting the query sets since
                // there is no auxiliary pass.
                let range = if let (Some(index_a), Some(index_b)) =
                    (tw.beginning_of_pass_write_index, tw.end_of_pass_write_index)
                {
                    Some(index_a.min(index_b)..index_a.max(index_b) + 1)
                } else {
                    tw.beginning_of_pass_write_index
                        .or(tw.end_of_pass_write_index)
                        .map(|i| i..i + 1)
                };
                // Range should always be Some, both values being None should lead to a validation error.
                // But no point in erroring over that nuance here!
                if let Some(range) = range {
                    unsafe {
                        state
                            .general
                            .raw_encoder
                            .reset_queries(query_set.raw(), range);
                    }
                }

                Some(hal::PassTimestampWrites {
                    query_set: query_set.raw(),
                    beginning_of_pass_write_index: tw.beginning_of_pass_write_index,
                    end_of_pass_write_index: tw.end_of_pass_write_index,
                })
            } else {
                None
            };

        let hal_desc = hal::RayTracingPassDescriptor {
            label: hal_label(base.label.as_deref(), device.instance_flags),
            timestamp_writes,
        };

        unsafe {
            state.general.raw_encoder.begin_ray_tracing_pass(&hal_desc);
        }

        for command in base.commands {
            match command {
                ArcRayTracingCommand::SetBindGroup {
                    index,
                    num_dynamic_offsets,
                    bind_group,
                } => {
                    let scope = PassErrorScope::SetBindGroup;
                    pass::set_bind_group(
                        &mut state.general,
                        cmd_buf,
                        &base.dynamic_offsets,
                        index,
                        num_dynamic_offsets,
                        bind_group,
                    )
                    .map_pass_err(scope)?;
                }
                ArcRayTracingCommand::SetPipeline(pipeline) => {
                    let scope = PassErrorScope::SetPipelineRayTracing;
                    set_pipeline(&mut state, cmd_buf, pipeline).map_pass_err(scope)?;
                }
                ArcRayTracingCommand::SetPushConstant {
                    stages,
                    offset,
                    size_bytes,
                    values_offset,
                } => {
                    let scope = PassErrorScope::SetPushConstant;
                    pass::set_push_constant(
                        &mut state.general,
                        &base.push_constant_data,
                        stages,
                        offset,
                        size_bytes,
                        Some(values_offset),
                        |_| {},
                    )
                    .map_err(RayTracingPassErrorInner::GeneralPass)
                    .map_pass_err(scope)?;
                }
                ArcRayTracingCommand::TraceRays(dimensions) => {
                    let scope = PassErrorScope::TraceRays { indirect: false };
                    trace_rays(&mut state, dimensions).map_pass_err(scope)?;
                }
                ArcRayTracingCommand::TraceRaysIndirect { buffer, offset } => {
                    let scope = PassErrorScope::TraceRays { indirect: true };
                    trace_rays_indirect(&mut state, buffer, offset).map_pass_err(scope)?;
                }
                ArcRayTracingCommand::PushDebugGroup { len } => {
                    pass::push_debug_group(&mut state.general, &base.string_data, len);
                }
                ArcRayTracingCommand::PopDebugGroup => {
                    let scope = PassErrorScope::PopDebugGroup;
                    pass::pop_debug_group(&mut state.general).map_pass_err(scope)?;
                }
                ArcRayTracingCommand::InsertDebugMarker { len } => {
                    pass::insert_debug_marker(&mut state.general, &base.string_data, len);
                }
                ArcRayTracingCommand::WriteTimestamp {
                    query_set,
                    query_index,
                } => {
                    let scope = PassErrorScope::WriteTimestamp;
                    pass::write_timestamp(
                        &mut state.general,
                        cmd_buf,
                        None,
                        query_set,
                        query_index,
                    )
                    .map_err(RayTracingPassErrorInner::GeneralPass)
                    .map_pass_err(scope)?;
                }
                ArcRayTracingCommand::BeginPipelineStatisticsQuery {
                    query_set,
                    query_index,
                } => {
                    let scope = PassErrorScope::BeginPipelineStatisticsQuery;
                    validate_and_begin_pipeline_statistics_query(
                        query_set,
                        state.general.raw_encoder,
                        &mut state.general.tracker.query_sets,
                        cmd_buf,
                        query_index,
                        None,
                        &mut state.active_query,
                    )
                    .map_err(|e| {
                        RayTracingPassErrorInner::GeneralPass(pass::PassError::QueryUse(e))
                    })
                    .map_pass_err(scope)?;
                }
                ArcRayTracingCommand::EndPipelineStatisticsQuery => {
                    let scope = PassErrorScope::EndPipelineStatisticsQuery;
                    end_pipeline_statistics_query(
                        state.general.raw_encoder,
                        &mut state.active_query,
                    )
                    .map_err(|e| {
                        RayTracingPassErrorInner::GeneralPass(pass::PassError::QueryUse(e))
                    })
                    .map_pass_err(scope)?;
                }
            }
        }

        unsafe {
            state.general.raw_encoder.end_ray_tracing_pass();
        }

        let State {
            general:
                pass::BaseState {
                    tracker,
                    scope,
                    pending_discard_init_fixups,
                    ..
                },
            ..
        } = state;

        // Stop the current command buffer.
        encoder.close().map_pass_err(pass_scope)?;

        // Create a new command buffer, which we will insert _before_ the body of the compute pass.
        //
        // Use that buffer to insert barriers and clear discarded images.
        let transit = encoder
            .open_pass(Some("(wgpu internal) Pre Pass"))
            .map_pass_err(pass_scope)?;
        fixup_discarded_surfaces(
            pending_discard_init_fixups.into_iter(),
            transit,
            &mut tracker.textures,
            device,
            &snatch_guard,
        );
        CommandBuffer::insert_barriers_from_scope(transit, tracker, &scope, &snatch_guard);
        // Close the command buffer, and swap it with the previous.
        encoder.close_and_swap().map_pass_err(pass_scope)?;
        cmd_buf_data_guard.mark_successful();

        Ok(())
    }

    pub fn ray_tracing_pass_set_bind_group(
        &self,
        pass: &mut RayTracingPass,
        index: u32,
        bind_group_id: Option<id::BindGroupId>,
        offsets: &[DynamicOffset],
    ) -> Result<(), RayTracingPassError> {
        let scope = PassErrorScope::SetBindGroup;
        let base = pass
            .base
            .as_mut()
            .ok_or(RayTracingPassErrorInner::PassEnded)
            .map_pass_err(scope)?; // Can't use base_mut() utility here because of borrow checker.

        let redundant = pass.current_bind_groups.set_and_check_redundant(
            bind_group_id,
            index,
            &mut base.dynamic_offsets,
            offsets,
        );

        if redundant {
            return Ok(());
        }

        let mut bind_group = None;
        if bind_group_id.is_some() {
            let bind_group_id = bind_group_id.unwrap();

            let hub = &self.hub;
            let bg = hub
                .bind_groups
                .get(bind_group_id)
                .get()
                .map_pass_err(scope)?;
            bind_group = Some(bg);
        }

        base.commands.push(ArcRayTracingCommand::SetBindGroup {
            index,
            num_dynamic_offsets: offsets.len(),
            bind_group,
        });

        Ok(())
    }

    pub fn ray_tracing_pass_set_pipeline(
        &self,
        pass: &mut RayTracingPass,
        pipeline_id: id::RayTracingPipelineId,
    ) -> Result<(), RayTracingPassError> {
        let redundant = pass.current_pipeline.set_and_check_redundant(pipeline_id);

        let scope = PassErrorScope::SetPipelineRayTracing;

        let base = pass.base_mut(scope)?;
        if redundant {
            // Do redundant early-out **after** checking whether the pass is ended or not.
            return Ok(());
        }

        let hub = &self.hub;
        let pipeline = hub
            .ray_tracing_pipelines
            .get(pipeline_id)
            .get()
            .map_pass_err(scope)?;

        base.commands
            .push(ArcRayTracingCommand::SetPipeline(pipeline));

        Ok(())
    }

    pub fn ray_tracing_pass_set_push_constants(
        &self,
        pass: &mut RayTracingPass,
        stages: ShaderStages,
        offset: u32,
        data: &[u8],
    ) -> Result<(), RayTracingPassError> {
        let scope = PassErrorScope::SetPushConstant;
        let base = pass.base_mut(scope)?;

        if offset & (wgt::PUSH_CONSTANT_ALIGNMENT - 1) != 0 {
            return Err(RayTracingPassErrorInner::PushConstantOffsetAlignment).map_pass_err(scope);
        }

        if data.len() as u32 & (wgt::PUSH_CONSTANT_ALIGNMENT - 1) != 0 {
            return Err(RayTracingPassErrorInner::PushConstantSizeAlignment).map_pass_err(scope);
        }
        let value_offset = base
            .push_constant_data
            .len()
            .try_into()
            .map_err(|_| RayTracingPassErrorInner::PushConstantOutOfMemory)
            .map_pass_err(scope)?;

        base.push_constant_data.extend(
            data.chunks_exact(wgt::PUSH_CONSTANT_ALIGNMENT as usize)
                .map(|arr| u32::from_ne_bytes([arr[0], arr[1], arr[2], arr[3]])),
        );

        base.commands.push(ArcRayTracingCommand::SetPushConstant {
            stages,
            offset,
            size_bytes: data.len() as u32,
            values_offset: value_offset,
        });

        Ok(())
    }

    pub fn ray_tracing_pass_trace_rays(
        &self,
        pass: &mut RayTracingPass,
        groups_x: u32,
        groups_y: u32,
        groups_z: u32,
    ) -> Result<(), RayTracingPassError> {
        let scope = PassErrorScope::TraceRays { indirect: false };

        let base = pass.base_mut(scope)?;
        base.commands.push(ArcRayTracingCommand::TraceRays([
            groups_x, groups_y, groups_z,
        ]));

        Ok(())
    }

    pub fn ray_tracing_pass_trace_rays_indirect(
        &self,
        pass: &mut RayTracingPass,
        buffer_id: id::BufferId,
        offset: BufferAddress,
    ) -> Result<(), RayTracingPassError> {
        let scope = PassErrorScope::TraceRays { indirect: false };

        let hub = &self.hub;
        let buffer = hub.buffers.get(buffer_id).get().map_pass_err(scope)?;

        let base = pass.base_mut(scope)?;
        base.commands
            .push(ArcRayTracingCommand::TraceRaysIndirect { buffer, offset });

        Ok(())
    }

    pub fn ray_tracing_pass_push_debug_group(
        &self,
        pass: &mut RayTracingPass,
        label: &str,
        _color: u32,
    ) -> Result<(), RayTracingPassError> {
        let base = pass.base_mut(PassErrorScope::PushDebugGroup)?;

        let bytes = label.as_bytes();
        base.string_data.extend_from_slice(bytes);

        base.commands
            .push(ArcRayTracingCommand::PushDebugGroup { len: bytes.len() });

        Ok(())
    }

    pub fn ray_tracing_pass_pop_debug_group(
        &self,
        pass: &mut RayTracingPass,
    ) -> Result<(), RayTracingPassError> {
        let base = pass.base_mut(PassErrorScope::PopDebugGroup)?;

        base.commands.push(ArcRayTracingCommand::PopDebugGroup);

        Ok(())
    }

    pub fn ray_tracing_pass_insert_debug_marker(
        &self,
        pass: &mut RayTracingPass,
        label: &str,
        _color: u32,
    ) -> Result<(), RayTracingPassError> {
        let base = pass.base_mut(PassErrorScope::InsertDebugMarker)?;

        let bytes = label.as_bytes();
        base.string_data.extend_from_slice(bytes);

        base.commands
            .push(ArcRayTracingCommand::InsertDebugMarker { len: bytes.len() });

        Ok(())
    }

    pub fn ray_tracing_pass_write_timestamp(
        &self,
        pass: &mut RayTracingPass,
        query_set_id: id::QuerySetId,
        query_index: u32,
    ) -> Result<(), RayTracingPassError> {
        let scope = PassErrorScope::WriteTimestamp;
        let base = pass.base_mut(scope)?;

        let hub = &self.hub;
        let query_set = hub.query_sets.get(query_set_id).get().map_pass_err(scope)?;

        base.commands.push(ArcRayTracingCommand::WriteTimestamp {
            query_set,
            query_index,
        });

        Ok(())
    }

    pub fn ray_tracing_pass_begin_pipeline_statistics_query(
        &self,
        pass: &mut RayTracingPass,
        query_set_id: id::QuerySetId,
        query_index: u32,
    ) -> Result<(), RayTracingPassError> {
        let scope = PassErrorScope::BeginPipelineStatisticsQuery;
        let base = pass.base_mut(scope)?;

        let hub = &self.hub;
        let query_set = hub.query_sets.get(query_set_id).get().map_pass_err(scope)?;

        base.commands
            .push(ArcRayTracingCommand::BeginPipelineStatisticsQuery {
                query_set,
                query_index,
            });

        Ok(())
    }

    pub fn ray_tracing_pass_end_pipeline_statistics_query(
        &self,
        pass: &mut RayTracingPass,
    ) -> Result<(), RayTracingPassError> {
        let scope = PassErrorScope::EndPipelineStatisticsQuery;
        let base = pass.base_mut(scope)?;
        base.commands
            .push(ArcRayTracingCommand::EndPipelineStatisticsQuery);

        Ok(())
    }
}

impl CommandBufferMutable {
    pub(crate) fn validate_acceleration_structure_actions(
        &self,
        snatch_guard: &SnatchGuard,
        command_index_guard: &mut RwLockWriteGuard<CommandIndices>,
    ) -> Result<(), ValidateAsActionsError> {
        profiling::scope!("CommandEncoder::[submission]::validate_as_actions");
        for action in &self.as_actions {
            match action {
                AsAction::Build(build) => {
                    let build_command_index = NonZeroU64::new(
                        command_index_guard.next_acceleration_structure_build_command_index,
                    )
                    .unwrap();

                    command_index_guard.next_acceleration_structure_build_command_index += 1;
                    for blas in build.blas_s_built.iter() {
                        let mut state_lock = blas.compacted_state.lock();
                        *state_lock = match *state_lock {
                            BlasCompactState::Compacted => {
                                unreachable!("Should be validated out in build.")
                            }
                            // Reset the compacted state to idle. This means any prepares, before mapping their
                            // internal buffer, will terminate.
                            _ => BlasCompactState::Idle,
                        };
                        *blas.built_index.write() = Some(build_command_index);
                    }

                    for tlas_build in build.tlas_s_built.iter() {
                        for blas in &tlas_build.dependencies {
                            if blas.built_index.read().is_none() {
                                return Err(ValidateAsActionsError::UsedUnbuiltBlas(
                                    blas.error_ident(),
                                    tlas_build.tlas.error_ident(),
                                ));
                            }
                        }
                        *tlas_build.tlas.built_index.write() = Some(build_command_index);
                        tlas_build
                            .tlas
                            .dependencies
                            .write()
                            .clone_from(&tlas_build.dependencies)
                    }
                }
                AsAction::UseTlas(tlas) => {
                    let tlas_build_index = tlas.built_index.read();
                    let dependencies = tlas.dependencies.read();

                    if (*tlas_build_index).is_none() {
                        return Err(ValidateAsActionsError::UsedUnbuiltTlas(tlas.error_ident()));
                    }
                    for blas in dependencies.deref() {
                        let blas_build_index = *blas.built_index.read();
                        if blas_build_index.is_none() {
                            return Err(ValidateAsActionsError::UsedUnbuiltBlas(
                                tlas.error_ident(),
                                blas.error_ident(),
                            ));
                        }
                        if blas_build_index.unwrap() > tlas_build_index.unwrap() {
                            return Err(ValidateAsActionsError::BlasNewerThenTlas(
                                blas.error_ident(),
                                tlas.error_ident(),
                            ));
                        }
                        blas.try_raw(snatch_guard)?;
                    }
                }
            }
        }
        Ok(())
    }
}

///iterates over the blas iterator, and it's geometry, pushing the buffers into a storage vector (and also some validation).
fn iter_blas<'a>(
    blas_iter: impl Iterator<Item = BlasBuildEntry<'a>>,
    cmd_buf_data: &mut CommandBufferMutable,
    build_command: &mut AsBuild,
    buf_storage: &mut Vec<TriangleBufferStore<'a>>,
    hub: &Hub,
) -> Result<(), BuildAccelerationStructureError> {
    let mut temp_buffer = Vec::new();
    for entry in blas_iter {
        let blas = hub.blas_s.get(entry.blas_id).get()?;
        cmd_buf_data.trackers.blas_s.insert_single(blas.clone());

        build_command.blas_s_built.push(blas.clone());

        match entry.geometries {
            BlasGeometries::TriangleGeometries(triangle_geometries) => {
                for (i, mesh) in triangle_geometries.enumerate() {
                    let size_desc = match &blas.sizes {
                        wgt::BlasGeometrySizeDescriptors::Triangles { descriptors } => descriptors,
                    };
                    if i >= size_desc.len() {
                        return Err(BuildAccelerationStructureError::IncompatibleBlasBuildSizes(
                            blas.error_ident(),
                        ));
                    }
                    let size_desc = &size_desc[i];

                    if size_desc.flags != mesh.size.flags {
                        return Err(BuildAccelerationStructureError::IncompatibleBlasFlags(
                            blas.error_ident(),
                            size_desc.flags,
                            mesh.size.flags,
                        ));
                    }

                    if size_desc.vertex_count < mesh.size.vertex_count {
                        return Err(
                            BuildAccelerationStructureError::IncompatibleBlasVertexCount(
                                blas.error_ident(),
                                size_desc.vertex_count,
                                mesh.size.vertex_count,
                            ),
                        );
                    }

                    if size_desc.vertex_format != mesh.size.vertex_format {
                        return Err(BuildAccelerationStructureError::DifferentBlasVertexFormats(
                            blas.error_ident(),
                            size_desc.vertex_format,
                            mesh.size.vertex_format,
                        ));
                    }

                    if size_desc
                        .vertex_format
                        .min_acceleration_structure_vertex_stride()
                        > mesh.vertex_stride
                    {
                        return Err(BuildAccelerationStructureError::VertexStrideTooSmall(
                            blas.error_ident(),
                            size_desc
                                .vertex_format
                                .min_acceleration_structure_vertex_stride(),
                            mesh.vertex_stride,
                        ));
                    }

                    if mesh.vertex_stride
                        % size_desc
                            .vertex_format
                            .acceleration_structure_stride_alignment()
                        != 0
                    {
                        return Err(BuildAccelerationStructureError::VertexStrideUnaligned(
                            blas.error_ident(),
                            size_desc
                                .vertex_format
                                .acceleration_structure_stride_alignment(),
                            mesh.vertex_stride,
                        ));
                    }

                    match (size_desc.index_count, mesh.size.index_count) {
                        (Some(_), None) | (None, Some(_)) => {
                            return Err(
                                BuildAccelerationStructureError::BlasIndexCountProvidedMismatch(
                                    blas.error_ident(),
                                ),
                            )
                        }
                        (Some(create), Some(build)) if create < build => {
                            return Err(
                                BuildAccelerationStructureError::IncompatibleBlasIndexCount(
                                    blas.error_ident(),
                                    create,
                                    build,
                                ),
                            )
                        }
                        _ => {}
                    }

                    if size_desc.index_format != mesh.size.index_format {
                        return Err(BuildAccelerationStructureError::DifferentBlasIndexFormats(
                            blas.error_ident(),
                            size_desc.index_format,
                            mesh.size.index_format,
                        ));
                    }

                    if size_desc.index_count.is_some() && mesh.index_buffer.is_none() {
                        return Err(BuildAccelerationStructureError::MissingIndexBuffer(
                            blas.error_ident(),
                        ));
                    }
                    let vertex_buffer = hub.buffers.get(mesh.vertex_buffer).get()?;
                    let vertex_pending = cmd_buf_data.trackers.buffers.set_single(
                        &vertex_buffer,
                        BufferUses::BOTTOM_LEVEL_ACCELERATION_STRUCTURE_INPUT,
                    );
                    let index_data = if let Some(index_id) = mesh.index_buffer {
                        let index_buffer = hub.buffers.get(index_id).get()?;
                        if mesh.first_index.is_none()
                            || mesh.size.index_count.is_none()
                            || mesh.size.index_count.is_none()
                        {
                            return Err(BuildAccelerationStructureError::MissingAssociatedData(
                                index_buffer.error_ident(),
                            ));
                        }
                        let data = cmd_buf_data.trackers.buffers.set_single(
                            &index_buffer,
                            BufferUses::BOTTOM_LEVEL_ACCELERATION_STRUCTURE_INPUT,
                        );
                        Some((index_buffer, data))
                    } else {
                        None
                    };
                    let transform_data = if let Some(transform_id) = mesh.transform_buffer {
                        if !blas
                            .flags
                            .contains(wgt::AccelerationStructureFlags::USE_TRANSFORM)
                        {
                            return Err(BuildAccelerationStructureError::UseTransformMissing(
                                blas.error_ident(),
                            ));
                        }
                        let transform_buffer = hub.buffers.get(transform_id).get()?;
                        if mesh.transform_buffer_offset.is_none() {
                            return Err(BuildAccelerationStructureError::MissingAssociatedData(
                                transform_buffer.error_ident(),
                            ));
                        }
                        let data = cmd_buf_data.trackers.buffers.set_single(
                            &transform_buffer,
                            BufferUses::BOTTOM_LEVEL_ACCELERATION_STRUCTURE_INPUT,
                        );
                        Some((transform_buffer, data))
                    } else {
                        if blas
                            .flags
                            .contains(wgt::AccelerationStructureFlags::USE_TRANSFORM)
                        {
                            return Err(BuildAccelerationStructureError::TransformMissing(
                                blas.error_ident(),
                            ));
                        }
                        None
                    };
                    temp_buffer.push(TriangleBufferStore {
                        vertex_buffer,
                        vertex_transition: vertex_pending,
                        index_buffer_transition: index_data,
                        transform_buffer_transition: transform_data,
                        geometry: mesh,
                        ending_blas: None,
                    });
                }

                if let Some(last) = temp_buffer.last_mut() {
                    last.ending_blas = Some(blas);
                    buf_storage.append(&mut temp_buffer);
                }
            }
        }
    }
    Ok(())
}

/// Iterates over the buffers generated in [iter_blas], convert the barriers into hal barriers, and the triangles into [hal::AccelerationStructureEntries] (and also some validation).
fn iter_buffers<'a, 'b>(
    buf_storage: &'a mut Vec<TriangleBufferStore<'b>>,
    snatch_guard: &'a SnatchGuard,
    input_barriers: &mut Vec<hal::BufferBarrier<'a, dyn hal::DynBuffer>>,
    cmd_buf_data: &mut CommandBufferMutable,
    scratch_buffer_blas_size: &mut u64,
    blas_storage: &mut Vec<BlasStore<'a>>,
    hub: &Hub,
    ray_tracing_scratch_buffer_alignment: u32,
) -> Result<(), BuildAccelerationStructureError> {
    let mut triangle_entries =
        Vec::<hal::AccelerationStructureTriangles<dyn hal::DynBuffer>>::new();
    for buf in buf_storage {
        let mesh = &buf.geometry;
        let vertex_buffer = {
            let vertex_buffer = buf.vertex_buffer.as_ref();
            let vertex_raw = vertex_buffer.try_raw(snatch_guard)?;
            vertex_buffer.check_usage(BufferUsages::BLAS_INPUT)?;

            if let Some(barrier) = buf
                .vertex_transition
                .take()
                .map(|pending| pending.into_hal(vertex_buffer, snatch_guard))
            {
                input_barriers.push(barrier);
            }
            if vertex_buffer.size
                < (mesh.size.vertex_count + mesh.first_vertex) as u64 * mesh.vertex_stride
            {
                return Err(BuildAccelerationStructureError::InsufficientBufferSize(
                    vertex_buffer.error_ident(),
                    vertex_buffer.size,
                    (mesh.size.vertex_count + mesh.first_vertex) as u64 * mesh.vertex_stride,
                ));
            }
            let vertex_buffer_offset = mesh.first_vertex as u64 * mesh.vertex_stride;
            cmd_buf_data.buffer_memory_init_actions.extend(
                vertex_buffer.initialization_status.read().create_action(
                    &hub.buffers.get(mesh.vertex_buffer).get()?,
                    vertex_buffer_offset
                        ..(vertex_buffer_offset
                            + mesh.size.vertex_count as u64 * mesh.vertex_stride),
                    MemoryInitKind::NeedsInitializedMemory,
                ),
            );
            vertex_raw
        };
        let index_buffer = if let Some((ref mut index_buffer, ref mut index_pending)) =
            buf.index_buffer_transition
        {
            let index_raw = index_buffer.try_raw(snatch_guard)?;
            index_buffer.check_usage(BufferUsages::BLAS_INPUT)?;

            if let Some(barrier) = index_pending
                .take()
                .map(|pending| pending.into_hal(index_buffer, snatch_guard))
            {
                input_barriers.push(barrier);
            }
            let index_stride = mesh.size.index_format.unwrap().byte_size() as u64;
            let offset = mesh.first_index.unwrap() as u64 * index_stride;
            let index_buffer_size = mesh.size.index_count.unwrap() as u64 * index_stride;

            if mesh.size.index_count.unwrap() % 3 != 0 {
                return Err(BuildAccelerationStructureError::InvalidIndexCount(
                    index_buffer.error_ident(),
                    mesh.size.index_count.unwrap(),
                ));
            }
            if index_buffer.size < mesh.size.index_count.unwrap() as u64 * index_stride + offset {
                return Err(BuildAccelerationStructureError::InsufficientBufferSize(
                    index_buffer.error_ident(),
                    index_buffer.size,
                    mesh.size.index_count.unwrap() as u64 * index_stride + offset,
                ));
            }

            cmd_buf_data.buffer_memory_init_actions.extend(
                index_buffer.initialization_status.read().create_action(
                    index_buffer,
                    offset..(offset + index_buffer_size),
                    MemoryInitKind::NeedsInitializedMemory,
                ),
            );
            Some(index_raw)
        } else {
            None
        };
        let transform_buffer = if let Some((ref mut transform_buffer, ref mut transform_pending)) =
            buf.transform_buffer_transition
        {
            if mesh.transform_buffer_offset.is_none() {
                return Err(BuildAccelerationStructureError::MissingAssociatedData(
                    transform_buffer.error_ident(),
                ));
            }
            let transform_raw = transform_buffer.try_raw(snatch_guard)?;
            transform_buffer.check_usage(BufferUsages::BLAS_INPUT)?;

            if let Some(barrier) = transform_pending
                .take()
                .map(|pending| pending.into_hal(transform_buffer, snatch_guard))
            {
                input_barriers.push(barrier);
            }

            let offset = mesh.transform_buffer_offset.unwrap();

            if offset % wgt::TRANSFORM_BUFFER_ALIGNMENT != 0 {
                return Err(
                    BuildAccelerationStructureError::UnalignedTransformBufferOffset(
                        transform_buffer.error_ident(),
                    ),
                );
            }
            if transform_buffer.size < 48 + offset {
                return Err(BuildAccelerationStructureError::InsufficientBufferSize(
                    transform_buffer.error_ident(),
                    transform_buffer.size,
                    48 + offset,
                ));
            }
            cmd_buf_data.buffer_memory_init_actions.extend(
                transform_buffer.initialization_status.read().create_action(
                    transform_buffer,
                    offset..(offset + 48),
                    MemoryInitKind::NeedsInitializedMemory,
                ),
            );
            Some(transform_raw)
        } else {
            None
        };

        let triangles = hal::AccelerationStructureTriangles {
            vertex_buffer: Some(vertex_buffer),
            vertex_format: mesh.size.vertex_format,
            first_vertex: mesh.first_vertex,
            vertex_count: mesh.size.vertex_count,
            vertex_stride: mesh.vertex_stride,
            indices: index_buffer.map(|index_buffer| {
                let index_stride = mesh.size.index_format.unwrap().byte_size() as u32;
                hal::AccelerationStructureTriangleIndices::<dyn hal::DynBuffer> {
                    format: mesh.size.index_format.unwrap(),
                    buffer: Some(index_buffer),
                    offset: mesh.first_index.unwrap() * index_stride,
                    count: mesh.size.index_count.unwrap(),
                }
            }),
            transform: transform_buffer.map(|transform_buffer| {
                hal::AccelerationStructureTriangleTransform {
                    buffer: transform_buffer,
                    offset: mesh.transform_buffer_offset.unwrap() as u32,
                }
            }),
            flags: mesh.size.flags,
        };
        triangle_entries.push(triangles);
        if let Some(blas) = buf.ending_blas.take() {
            let scratch_buffer_offset = *scratch_buffer_blas_size;
            *scratch_buffer_blas_size += align_to(
                blas.size_info.build_scratch_size as u32,
                ray_tracing_scratch_buffer_alignment,
            ) as u64;

            blas_storage.push(BlasStore {
                blas,
                entries: hal::AccelerationStructureEntries::Triangles(triangle_entries),
                scratch_buffer_offset,
            });
            triangle_entries = Vec::new();
        }
    }
    Ok(())
}

fn map_blas<'a>(
    storage: &'a BlasStore<'_>,
    scratch_buffer: &'a dyn hal::DynBuffer,
    snatch_guard: &'a SnatchGuard,
    blases_compactable: &mut Vec<(
        &'a dyn hal::DynBuffer,
        &'a dyn hal::DynAccelerationStructure,
    )>,
) -> Result<
    hal::BuildAccelerationStructureDescriptor<
        'a,
        dyn hal::DynBuffer,
        dyn hal::DynAccelerationStructure,
    >,
    BuildAccelerationStructureError,
> {
    let BlasStore {
        blas,
        entries,
        scratch_buffer_offset,
    } = storage;
    if blas.update_mode == wgt::AccelerationStructureUpdateMode::PreferUpdate {
        log::info!("only rebuild implemented")
    }
    let raw = blas.try_raw(snatch_guard)?;

    let state_lock = blas.compacted_state.lock();
    if let BlasCompactState::Compacted = *state_lock {
        return Err(BuildAccelerationStructureError::CompactedBlas(
            blas.error_ident(),
        ));
    }

    if blas
        .flags
        .contains(wgpu_types::AccelerationStructureFlags::ALLOW_COMPACTION)
    {
        blases_compactable.push((blas.compaction_buffer.as_ref().unwrap().as_ref(), raw));
    }
    Ok(hal::BuildAccelerationStructureDescriptor {
        entries,
        mode: hal::AccelerationStructureBuildMode::Build,
        flags: blas.flags,
        source_acceleration_structure: None,
        destination_acceleration_structure: raw,
        scratch_buffer,
        scratch_buffer_offset: *scratch_buffer_offset,
    })
}

fn build_blas<'a>(
    cmd_buf_raw: &mut dyn hal::DynCommandEncoder,
    blas_present: bool,
    tlas_present: bool,
    input_barriers: Vec<hal::BufferBarrier<dyn hal::DynBuffer>>,
    blas_descriptors: &[hal::BuildAccelerationStructureDescriptor<
        'a,
        dyn hal::DynBuffer,
        dyn hal::DynAccelerationStructure,
    >],
    scratch_buffer_barrier: hal::BufferBarrier<dyn hal::DynBuffer>,
    blas_s_for_compaction: Vec<(
        &'a dyn hal::DynBuffer,
        &'a dyn hal::DynAccelerationStructure,
    )>,
) {
    unsafe {
        cmd_buf_raw.transition_buffers(&input_barriers);
    }

    if blas_present {
        unsafe {
            cmd_buf_raw.place_acceleration_structure_barrier(hal::AccelerationStructureBarrier {
                usage: hal::StateTransition {
                    from: hal::AccelerationStructureUses::BUILD_INPUT,
                    to: hal::AccelerationStructureUses::BUILD_OUTPUT,
                },
            });

            cmd_buf_raw.build_acceleration_structures(blas_descriptors);
        }
    }

    if blas_present && tlas_present {
        unsafe {
            cmd_buf_raw.transition_buffers(&[scratch_buffer_barrier]);
        }
    }

    let mut source_usage = hal::AccelerationStructureUses::empty();
    let mut destination_usage = hal::AccelerationStructureUses::empty();
    for &(buf, blas) in blas_s_for_compaction.iter() {
        unsafe {
            cmd_buf_raw.transition_buffers(&[hal::BufferBarrier {
                buffer: buf,
                usage: hal::StateTransition {
                    from: BufferUses::ACCELERATION_STRUCTURE_QUERY,
                    to: BufferUses::ACCELERATION_STRUCTURE_QUERY,
                },
            }])
        }
        unsafe { cmd_buf_raw.read_acceleration_structure_compact_size(blas, buf) }
        destination_usage |= hal::AccelerationStructureUses::COPY_SRC;
    }

    if blas_present {
        source_usage |= hal::AccelerationStructureUses::BUILD_OUTPUT;
        destination_usage |= hal::AccelerationStructureUses::BUILD_INPUT
    }
    if tlas_present {
        source_usage |= hal::AccelerationStructureUses::SHADER_INPUT;
        destination_usage |= hal::AccelerationStructureUses::BUILD_OUTPUT;
    }
    unsafe {
        cmd_buf_raw.place_acceleration_structure_barrier(hal::AccelerationStructureBarrier {
            usage: hal::StateTransition {
                from: source_usage,
                to: destination_usage,
            },
        });
    }
}

pub struct RayTracingPass {
    /// All pass data & records is stored here.
    ///
    /// If this is `None`, the pass is in the 'ended' state and can no longer be used.
    /// Any attempt to record more commands will result in a validation error.
    base: Option<BasePass<ArcRayTracingCommand>>,

    /// Parent command buffer that this pass records commands into.
    ///
    /// If it is none, this pass is invalid and any operation on it will return an error.
    parent: Option<Arc<CommandBuffer>>,

    timestamp_writes: Option<ArcPassTimestampWrites>,

    // Resource binding dedupe state.
    current_bind_groups: BindGroupStateChange,
    current_pipeline: StateChange<id::RayTracingPipelineId>,
}

/// Error encountered when performing a ray tracing pass.
#[derive(Clone, Debug, Error)]
#[error("{scope}")]
pub struct RayTracingPassError {
    pub scope: PassErrorScope,
    #[source]
    pub(super) inner: RayTracingPassErrorInner,
}

/// Error encountered when performing a ray tracing pass.
#[derive(Clone, Debug, Error)]
pub enum RayTracingPassErrorInner {
    #[error(transparent)]
    EncoderState(#[from] EncoderStateError),
    #[error(transparent)]
    GeneralPass(#[from] pass::PassError),
    #[error("Parent encoder is invalid")]
    InvalidParentEncoder,
    #[error("Indirect buffer offset {0:?} is not a multiple of 4")]
    UnalignedIndirectBufferOffset(BufferAddress),
    #[error("Indirect buffer uses bytes {offset}..{end_offset} which overruns indirect buffer of size {buffer_size}")]
    IndirectBufferOverrun {
        offset: u64,
        end_offset: u64,
        buffer_size: u64,
    },
    #[error(transparent)]
    MissingBufferUsage(#[from] MissingBufferUsageError),
    #[error(transparent)]
    TraceRays(#[from] TraceRaysError),
    #[error(transparent)]
    PushConstants(#[from] PushConstantUploadError),
    #[error("Push constant offset must be aligned to 4 bytes")]
    PushConstantOffsetAlignment,
    #[error("Push constant size must be aligned to 4 bytes")]
    PushConstantSizeAlignment,
    #[error("Ran out of push constant space. Don't set 4gb of push constants per RayTracingPass.")]
    PushConstantOutOfMemory,
    #[error(transparent)]
    MissingDownlevelFlags(#[from] MissingDownlevelFlags),
    #[error("The ray tracing pass has already been ended and no further commands can be recorded")]
    PassEnded,
    #[error(transparent)]
    InvalidResource(#[from] InvalidResourceError),
}

#[derive(Clone, Debug, Error)]
#[non_exhaustive]
pub enum TraceRaysError {
    #[error("Ray tracing pipeline must be set")]
    MissingPipeline,
    #[error(transparent)]
    IncompatibleBindGroup(#[from] Box<BinderError>),
    #[error("The ray tracing pipeline linked to TLAS {0:?} did not match the set ray tracing pipeline {1:?}")]
    MismatchedRayTracingPipelines(ResourceErrorIdent, ResourceErrorIdent),
    #[error(
        "Each current ray group size dimension ({current:?}) must be less or equal to the respective one in {limit:?}"
    )]
    InvalidGroupSize { current: [u32; 3], limit: [u32; 3] },
    #[error(transparent)]
    BindingSizeTooSmall(#[from] LateMinBufferBindingSizeMismatch),
}

impl From<DeviceError> for RayTracingPassErrorInner {
    fn from(e: DeviceError) -> Self {
        Self::GeneralPass(pass::PassError::Device(e))
    }
}

#[derive(Clone, Debug, Default)]
pub struct RayTracingPassDescriptor<'a, PTW = PassTimestampWrites> {
    pub label: Label<'a>,
    /// Defines where and when timestamp values will be written for this pass.
    pub timestamp_writes: Option<PTW>,
}

/// cbindgen:ignore
type ArcRayTracingPassDescriptor<'a> = RayTracingPassDescriptor<'a, ArcPassTimestampWrites>;

impl RayTracingPass {
    /// If the parent command buffer is invalid, the returned pass will be invalid.
    fn new(parent: Option<Arc<CommandBuffer>>, desc: ArcRayTracingPassDescriptor) -> Self {
        let ArcRayTracingPassDescriptor {
            label,
            timestamp_writes,
        } = desc;

        Self {
            base: Some(BasePass::new(&label)),
            parent,
            timestamp_writes,

            current_bind_groups: BindGroupStateChange::new(),
            current_pipeline: StateChange::new(),
        }
    }

    #[inline]
    pub fn label(&self) -> Option<&str> {
        self.base.as_ref().and_then(|base| base.label.as_deref())
    }

    fn base_mut<'a>(
        &'a mut self,
        scope: PassErrorScope,
    ) -> Result<&'a mut BasePass<ArcRayTracingCommand>, RayTracingPassError> {
        self.base
            .as_mut()
            .ok_or(RayTracingPassErrorInner::PassEnded)
            .map_pass_err(scope)
    }
}

impl<E> MapPassErr<RayTracingPassError> for E
where
    E: Into<RayTracingPassErrorInner>,
{
    fn map_pass_err(self, scope: PassErrorScope) -> RayTracingPassError {
        RayTracingPassError {
            scope,
            inner: self.into(),
        }
    }
}

impl fmt::Debug for RayTracingPass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.parent {
            Some(ref cmd_buf) => {
                write!(f, "RayTracingPass {{ parent: {} }}", cmd_buf.error_ident())
            }
            None => write!(f, "RayTracingPass {{ parent: None }}"),
        }
    }
}

struct State<'scope, 'snatch_guard, 'cmd_buf, 'raw_encoder> {
    pipeline: Option<Arc<RayTracingPipeline>>,

    general: pass::BaseState<'scope, 'snatch_guard, 'cmd_buf, 'raw_encoder>,

    active_query: Option<(Arc<resource::QuerySet>, u32)>,
}

impl<'scope, 'snatch_guard, 'cmd_buf, 'raw_encoder>
    State<'scope, 'snatch_guard, 'cmd_buf, 'raw_encoder>
{
    fn is_ready(&self) -> Result<(), TraceRaysError> {
        if let Some(pipeline) = self.pipeline.as_ref() {
            self.general.binder.check_compatibility(pipeline.as_ref())?;
            self.general.binder.check_late_buffer_bindings()?;
            Ok(())
        } else {
            Err(TraceRaysError::MissingPipeline)
        }
    }
}

fn set_pipeline(
    state: &mut State,
    cmd_buf: &CommandBuffer,
    pipeline: Arc<RayTracingPipeline>,
) -> Result<(), RayTracingPassErrorInner> {
    pipeline.same_device_as(cmd_buf)?;

    state.pipeline = Some(pipeline.clone());

    let pipeline = state
        .general
        .tracker
        .ray_tracing_pipelines
        .insert_single(pipeline)
        .clone();

    unsafe {
        state
            .general
            .raw_encoder
            .set_ray_tracing_pipeline(pipeline.raw());
    }

    // Rebind resources
    pass::rebind_resources(
        &mut state.general,
        &pipeline.layout,
        &pipeline.late_sized_buffer_groups,
        || {},
    )
    .map_err(RayTracingPassErrorInner::GeneralPass)
}

fn trace_rays(state: &mut State, count: [u32; 3]) -> Result<(), RayTracingPassErrorInner> {
    state.is_ready()?;

    let groups_size_limit = state
        .general
        .device
        .limits
        .max_compute_workgroups_per_dimension;

    let count_size_limit_x =
        state.general.device.limits.max_compute_workgroup_size_x * groups_size_limit;

    let count_size_limit_y =
        state.general.device.limits.max_compute_workgroup_size_y * groups_size_limit;

    let count_size_limit_z =
        state.general.device.limits.max_compute_workgroup_size_z * groups_size_limit;

    if count[0] > count_size_limit_x
        || count[1] > count_size_limit_y
        || count[2] > count_size_limit_z
    {
        return Err(RayTracingPassErrorInner::TraceRays(
            TraceRaysError::InvalidGroupSize {
                current: count,
                limit: [count_size_limit_x, count_size_limit_y, count_size_limit_z],
            },
        ));
    }

    let pipeline = state.pipeline.as_ref().unwrap();

    let ray_gen_sbt = hal::ShaderBindingTable {
        offset: pipeline.shader_binding_data.ray_generation_offset,
        count: 1,
        table: pipeline.shader_binding_data.buffer.as_ref(),
    };

    let ray_miss_sbt = hal::ShaderBindingTable {
        offset: pipeline.shader_binding_data.ray_miss_offset,
        count: 1,
        table: pipeline.shader_binding_data.buffer.as_ref(),
    };

    let ray_hit_sbt = hal::ShaderBindingTable {
        offset: pipeline.shader_binding_data.ray_hit_offset,
        count: pipeline.num_hit_groups,
        table: pipeline.shader_binding_data.buffer.as_ref(),
    };

    unsafe {
        state
            .general
            .raw_encoder
            .trace_rays(count, ray_gen_sbt, ray_miss_sbt, ray_hit_sbt);
    }
    Ok(())
}

fn trace_rays_indirect(
    state: &mut State,
    buf: Arc<Buffer>,
    offset: BufferAddress,
) -> Result<(), RayTracingPassErrorInner> {
    state.is_ready()?;

    let pipeline = state.pipeline.as_ref().unwrap();

    let ray_gen_sbt = hal::ShaderBindingTable {
        offset: pipeline.shader_binding_data.ray_generation_offset,
        count: 1,
        table: pipeline.shader_binding_data.buffer.as_ref(),
    };

    let ray_miss_sbt = hal::ShaderBindingTable {
        offset: pipeline.shader_binding_data.ray_miss_offset,
        count: 1,
        table: pipeline.shader_binding_data.buffer.as_ref(),
    };

    let ray_hit_sbt = hal::ShaderBindingTable {
        offset: pipeline.shader_binding_data.ray_hit_offset,
        count: pipeline.num_hit_groups,
        table: pipeline.shader_binding_data.buffer.as_ref(),
    };

    unsafe {
        state.general.raw_encoder.trace_rays_indirect(
            buf.try_raw(state.general.snatch_guard).map_err(|e| {
                RayTracingPassErrorInner::GeneralPass(pass::PassError::DestroyedResource(e))
            })?,
            offset,
            ray_gen_sbt,
            ray_miss_sbt,
            ray_hit_sbt,
        );
    }
    Ok(())
}
