use thiserror::Error;
use wgt::{BufferAddress, DynamicOffset};

use alloc::{borrow::Cow, boxed::Box, sync::Arc, vec::Vec};
use core::{fmt, str};

use crate::command::{pass, EncoderStateError};
use crate::{
    binding_model::{LateMinBufferBindingSizeMismatch, PushConstantUploadError},
    command::{
        bind::{Binder, BinderError},
        compute_command::ArcComputeCommand,
        end_pipeline_statistics_query,
        memory_init::{fixup_discarded_surfaces, SurfacesInDiscardState},
        validate_and_begin_pipeline_statistics_query, ArcPassTimestampWrites, BasePass,
        BindGroupStateChange, CommandBuffer, CommandEncoderError, MapPassErr, PassErrorScope,
        PassTimestampWrites, StateChange,
    },
    device::{DeviceError, MissingDownlevelFlags},
    global::Global,
    hal_label, id,
    init_tracker::MemoryInitKind,
    pipeline::ComputePipeline,
    resource::{
        self, Buffer, InvalidResourceError, Labeled, MissingBufferUsageError, ParentDevice,
    },
    Label,
};

pub struct ComputePass {
    /// All pass data & records is stored here.
    ///
    /// If this is `None`, the pass is in the 'ended' state and can no longer be used.
    /// Any attempt to record more commands will result in a validation error.
    base: Option<BasePass<ArcComputeCommand>>,

    /// Parent command buffer that this pass records commands into.
    ///
    /// If it is none, this pass is invalid and any operation on it will return an error.
    parent: Option<Arc<CommandBuffer>>,

    timestamp_writes: Option<ArcPassTimestampWrites>,

    // Resource binding dedupe state.
    current_bind_groups: BindGroupStateChange,
    current_pipeline: StateChange<id::ComputePipelineId>,
}

impl ComputePass {
    /// If the parent command buffer is invalid, the returned pass will be invalid.
    fn new(parent: Option<Arc<CommandBuffer>>, desc: ArcComputePassDescriptor) -> Self {
        let ArcComputePassDescriptor {
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
    ) -> Result<&'a mut BasePass<ArcComputeCommand>, ComputePassError> {
        self.base
            .as_mut()
            .ok_or(ComputePassErrorInner::PassEnded)
            .map_pass_err(scope)
    }
}

impl fmt::Debug for ComputePass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.parent {
            Some(ref cmd_buf) => write!(f, "ComputePass {{ parent: {} }}", cmd_buf.error_ident()),
            None => write!(f, "ComputePass {{ parent: None }}"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ComputePassDescriptor<'a, PTW = PassTimestampWrites> {
    pub label: Label<'a>,
    /// Defines where and when timestamp values will be written for this pass.
    pub timestamp_writes: Option<PTW>,
}

/// cbindgen:ignore
type ArcComputePassDescriptor<'a> = ComputePassDescriptor<'a, ArcPassTimestampWrites>;

#[derive(Clone, Debug, Error)]
#[non_exhaustive]
pub enum DispatchError {
    #[error("Compute pipeline must be set")]
    MissingPipeline,
    #[error(transparent)]
    IncompatibleBindGroup(#[from] Box<BinderError>),
    #[error(
        "Each current dispatch group size dimension ({current:?}) must be less or equal to {limit}"
    )]
    InvalidGroupSize { current: [u32; 3], limit: u32 },
    #[error(transparent)]
    BindingSizeTooSmall(#[from] LateMinBufferBindingSizeMismatch),
}

/// Error encountered when performing a compute pass.
#[derive(Clone, Debug, Error)]
pub enum ComputePassErrorInner {
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
    Dispatch(#[from] DispatchError),
    #[error(transparent)]
    PushConstants(#[from] PushConstantUploadError),
    #[error("Push constant offset must be aligned to 4 bytes")]
    PushConstantOffsetAlignment,
    #[error("Push constant size must be aligned to 4 bytes")]
    PushConstantSizeAlignment,
    #[error("Ran out of push constant space. Don't set 4gb of push constants per ComputePass.")]
    PushConstantOutOfMemory,
    #[error(transparent)]
    MissingDownlevelFlags(#[from] MissingDownlevelFlags),
    #[error("The compute pass has already been ended and no further commands can be recorded")]
    PassEnded,
    #[error(transparent)]
    InvalidResource(#[from] InvalidResourceError),
}

impl From<DeviceError> for ComputePassErrorInner {
    fn from(e: DeviceError) -> Self {
        Self::GeneralPass(pass::PassError::Device(e))
    }
}

/// Error encountered when performing a compute pass.
#[derive(Clone, Debug, Error)]
#[error("{scope}")]
pub struct ComputePassError {
    pub scope: PassErrorScope,
    #[source]
    pub(super) inner: ComputePassErrorInner,
}

impl<E> MapPassErr<ComputePassError> for E
where
    E: Into<ComputePassErrorInner>,
{
    fn map_pass_err(self, scope: PassErrorScope) -> ComputePassError {
        ComputePassError {
            scope,
            inner: self.into(),
        }
    }
}

struct State<'scope, 'snatch_guard, 'cmd_buf, 'raw_encoder> {
    pipeline: Option<Arc<ComputePipeline>>,

    general: pass::BaseState<'scope, 'snatch_guard, 'cmd_buf, 'raw_encoder>,

    active_query: Option<(Arc<resource::QuerySet>, u32)>,

    push_constants: Vec<u32>,
}

impl<'scope, 'snatch_guard, 'cmd_buf, 'raw_encoder>
    State<'scope, 'snatch_guard, 'cmd_buf, 'raw_encoder>
{
    fn is_ready(&self) -> Result<(), DispatchError> {
        if let Some(pipeline) = self.pipeline.as_ref() {
            self.general.binder.check_compatibility(pipeline.as_ref())?;
            self.general.binder.check_late_buffer_bindings()?;
            Ok(())
        } else {
            Err(DispatchError::MissingPipeline)
        }
    }
}

// Running the compute pass.

impl Global {
    /// Creates a compute pass.
    ///
    /// If creation fails, an invalid pass is returned.
    /// Any operation on an invalid pass will return an error.
    ///
    /// If successful, puts the encoder into the [`Locked`] state.
    ///
    /// [`Locked`]: crate::command::CommandEncoderStatus::Locked
    pub fn command_encoder_begin_compute_pass(
        &self,
        encoder_id: id::CommandEncoderId,
        desc: &ComputePassDescriptor<'_>,
    ) -> (ComputePass, Option<CommandEncoderError>) {
        let hub = &self.hub;

        let mut arc_desc = ArcComputePassDescriptor {
            label: desc.label.as_deref().map(Cow::Borrowed),
            timestamp_writes: None, // Handle only once we resolved the encoder.
        };

        let make_err = |e, arc_desc| (ComputePass::new(None, arc_desc), Some(e));

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

        (ComputePass::new(Some(cmd_buf), arc_desc), None)
    }

    /// Note that this differs from [`Self::compute_pass_end`], it will
    /// create a new pass, replay the commands and end the pass.
    ///
    /// # Panics
    /// On any error.
    #[doc(hidden)]
    #[cfg(any(feature = "serde", feature = "replay"))]
    pub fn compute_pass_end_with_unresolved_commands(
        &self,
        encoder_id: id::CommandEncoderId,
        base: BasePass<super::ComputeCommand>,
        timestamp_writes: Option<&PassTimestampWrites>,
    ) {
        #[cfg(feature = "trace")]
        {
            let cmd_buf = self
                .hub
                .command_buffers
                .get(encoder_id.into_command_buffer_id());
            let mut cmd_buf_data = cmd_buf.data.lock();
            let cmd_buf_data = cmd_buf_data.get_inner();

            if let Some(ref mut list) = cmd_buf_data.commands {
                list.push(crate::device::trace::Command::RunComputePass {
                    base: BasePass {
                        label: base.label.clone(),
                        commands: base.commands.clone(),
                        dynamic_offsets: base.dynamic_offsets.clone(),
                        string_data: base.string_data.clone(),
                        push_constant_data: base.push_constant_data.clone(),
                    },
                    timestamp_writes: timestamp_writes.cloned(),
                });
            }
        }

        let BasePass {
            label,
            commands,
            dynamic_offsets,
            string_data,
            push_constant_data,
        } = base;

        let (mut compute_pass, encoder_error) = self.command_encoder_begin_compute_pass(
            encoder_id,
            &ComputePassDescriptor {
                label: label.as_deref().map(Cow::Borrowed),
                timestamp_writes: timestamp_writes.cloned(),
            },
        );
        if let Some(err) = encoder_error {
            panic!("{:?}", err);
        };

        compute_pass.base = Some(BasePass {
            label,
            commands: super::ComputeCommand::resolve_compute_command_ids(&self.hub, &commands)
                .unwrap(),
            dynamic_offsets,
            string_data,
            push_constant_data,
        });

        self.compute_pass_end(&mut compute_pass).unwrap();
    }

    pub fn compute_pass_end(&self, pass: &mut ComputePass) -> Result<(), ComputePassError> {
        profiling::scope!("CommandEncoder::run_compute_pass");
        let pass_scope = PassErrorScope::Pass;

        let cmd_buf = pass
            .parent
            .as_ref()
            .ok_or(ComputePassErrorInner::InvalidParentEncoder)
            .map_pass_err(pass_scope)?;

        let base = pass
            .base
            .take()
            .ok_or(ComputePassErrorInner::PassEnded)
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

            push_constants: Vec::new(),
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

        let hal_desc = hal::ComputePassDescriptor {
            label: hal_label(base.label.as_deref(), device.instance_flags),
            timestamp_writes,
        };

        unsafe {
            state.general.raw_encoder.begin_compute_pass(&hal_desc);
        }

        for command in base.commands {
            match command {
                ArcComputeCommand::SetBindGroup {
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
                ArcComputeCommand::SetPipeline(pipeline) => {
                    let scope = PassErrorScope::SetPipelineCompute;
                    set_pipeline(&mut state, cmd_buf, pipeline).map_pass_err(scope)?;
                }
                ArcComputeCommand::SetPushConstant {
                    offset,
                    size_bytes,
                    values_offset,
                } => {
                    let scope = PassErrorScope::SetPushConstant;
                    pass::set_push_constant(
                        &mut state.general,
                        &base.push_constant_data,
                        wgt::ShaderStages::COMPUTE,
                        offset,
                        size_bytes,
                        Some(values_offset),
                        |data_slice| {
                            let offset_in_elements =
                                (offset / wgt::PUSH_CONSTANT_ALIGNMENT) as usize;
                            let size_in_elements =
                                (size_bytes / wgt::PUSH_CONSTANT_ALIGNMENT) as usize;
                            state.push_constants[offset_in_elements..][..size_in_elements]
                                .copy_from_slice(data_slice);
                        },
                    )
                    .map_err(ComputePassErrorInner::GeneralPass)
                    .map_pass_err(scope)?;
                }
                ArcComputeCommand::Dispatch(groups) => {
                    let scope = PassErrorScope::Dispatch { indirect: false };
                    dispatch(&mut state, groups).map_pass_err(scope)?;
                }
                ArcComputeCommand::DispatchIndirect { buffer, offset } => {
                    let scope = PassErrorScope::Dispatch { indirect: true };
                    dispatch_indirect(&mut state, cmd_buf, buffer, offset).map_pass_err(scope)?;
                }
                ArcComputeCommand::PushDebugGroup { color: _, len } => {
                    pass::push_debug_group(&mut state.general, &base.string_data, len);
                }
                ArcComputeCommand::PopDebugGroup => {
                    let scope = PassErrorScope::PopDebugGroup;
                    pass::pop_debug_group(&mut state.general).map_pass_err(scope)?;
                }
                ArcComputeCommand::InsertDebugMarker { color: _, len } => {
                    pass::insert_debug_marker(&mut state.general, &base.string_data, len);
                }
                ArcComputeCommand::WriteTimestamp {
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
                    .map_err(ComputePassErrorInner::GeneralPass)
                    .map_pass_err(scope)?;
                }
                ArcComputeCommand::BeginPipelineStatisticsQuery {
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
                    .map_err(|e| ComputePassErrorInner::GeneralPass(pass::PassError::QueryUse(e)))
                    .map_pass_err(scope)?;
                }
                ArcComputeCommand::EndPipelineStatisticsQuery => {
                    let scope = PassErrorScope::EndPipelineStatisticsQuery;
                    end_pipeline_statistics_query(
                        state.general.raw_encoder,
                        &mut state.active_query,
                    )
                    .map_err(|e| ComputePassErrorInner::GeneralPass(pass::PassError::QueryUse(e)))
                    .map_pass_err(scope)?;
                }
            }
        }

        unsafe {
            state.general.raw_encoder.end_compute_pass();
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
}

fn set_pipeline(
    state: &mut State,
    cmd_buf: &CommandBuffer,
    pipeline: Arc<ComputePipeline>,
) -> Result<(), ComputePassErrorInner> {
    pipeline
        .same_device_as(cmd_buf)
        .map_err(pass::PassError::Device)?;

    state.pipeline = Some(pipeline.clone());

    let pipeline = state
        .general
        .tracker
        .compute_pipelines
        .insert_single(pipeline)
        .clone();

    unsafe {
        state
            .general
            .raw_encoder
            .set_compute_pipeline(pipeline.raw());
    }

    // Rebind resources
    pass::rebind_resources(
        &mut state.general,
        &pipeline.layout,
        &pipeline.late_sized_buffer_groups,
        || {
            // This only needs to be here for compute pipelines because they use push constants for
            // validating indirect draws.
            state.push_constants.clear();
            // Note that can only be one range for each stage. See the `MoreThanOnePushConstantRangePerStage` error.
            if let Some(push_constant_range) =
                pipeline.layout.push_constant_ranges.iter().find_map(|pcr| {
                    pcr.stages
                        .contains(wgt::ShaderStages::COMPUTE)
                        .then_some(pcr.range.clone())
                })
            {
                // Note that non-0 range start doesn't work anyway https://github.com/gfx-rs/wgpu/issues/4502
                let len = push_constant_range.len() / wgt::PUSH_CONSTANT_ALIGNMENT as usize;
                state.push_constants.extend(core::iter::repeat_n(0, len));
            }
        },
    )
    .map_err(ComputePassErrorInner::GeneralPass)
}

fn dispatch(state: &mut State, groups: [u32; 3]) -> Result<(), ComputePassErrorInner> {
    state.is_ready()?;

    let groups_size_limit = state
        .general
        .device
        .limits
        .max_compute_workgroups_per_dimension;

    if groups[0] > groups_size_limit
        || groups[1] > groups_size_limit
        || groups[2] > groups_size_limit
    {
        return Err(ComputePassErrorInner::Dispatch(
            DispatchError::InvalidGroupSize {
                current: groups,
                limit: groups_size_limit,
            },
        ));
    }

    unsafe {
        state.general.raw_encoder.dispatch(groups);
    }
    Ok(())
}

fn dispatch_indirect(
    state: &mut State,
    cmd_buf: &CommandBuffer,
    buffer: Arc<Buffer>,
    offset: u64,
) -> Result<(), ComputePassErrorInner> {
    buffer
        .same_device_as(cmd_buf)
        .map_err(pass::PassError::Device)?;

    state.is_ready()?;

    state
        .general
        .device
        .require_downlevel_flags(wgt::DownlevelFlags::INDIRECT_EXECUTION)?;

    buffer.check_usage(wgt::BufferUsages::INDIRECT)?;
    buffer
        .check_destroyed(state.general.snatch_guard)
        .map_err(pass::PassError::DestroyedResource)?;

    if offset % 4 != 0 {
        return Err(ComputePassErrorInner::UnalignedIndirectBufferOffset(offset));
    }

    let end_offset = offset + size_of::<wgt::DispatchIndirectArgs>() as u64;
    if end_offset > buffer.size {
        return Err(ComputePassErrorInner::IndirectBufferOverrun {
            offset,
            end_offset,
            buffer_size: buffer.size,
        });
    }

    let stride = 3 * 4; // 3 integers, x/y/z group size
    state.general.buffer_memory_init_actions.extend(
        buffer.initialization_status.read().create_action(
            &buffer,
            offset..(offset + stride),
            MemoryInitKind::NeedsInitializedMemory,
        ),
    );

    if let Some(ref indirect_validation) = state.general.device.indirect_validation {
        let params =
            indirect_validation
                .dispatch
                .params(&state.general.device.limits, offset, buffer.size);

        unsafe {
            state
                .general
                .raw_encoder
                .set_compute_pipeline(params.pipeline);
        }

        unsafe {
            state.general.raw_encoder.set_push_constants(
                params.pipeline_layout,
                wgt::ShaderStages::COMPUTE,
                0,
                &[params.offset_remainder as u32 / 4],
            );
        }

        unsafe {
            state.general.raw_encoder.set_bind_group(
                params.pipeline_layout,
                0,
                Some(params.dst_bind_group),
                &[],
            );
        }
        unsafe {
            state.general.raw_encoder.set_bind_group(
                params.pipeline_layout,
                1,
                Some(
                    buffer
                        .indirect_validation_bind_groups
                        .get(state.general.snatch_guard)
                        .unwrap()
                        .dispatch
                        .as_ref(),
                ),
                &[params.aligned_offset as u32],
            );
        }

        state
            .general
            .scope
            .buffers
            .merge_single(&buffer, wgt::BufferUses::STORAGE_READ_ONLY)
            .map_err(pass::PassError::ResourceUsageCompatibility)?;

        unsafe {
            state
                .general
                .raw_encoder
                .transition_buffers(&[hal::BufferBarrier {
                    buffer: params.dst_buffer,
                    usage: hal::StateTransition {
                        from: wgt::BufferUses::INDIRECT,
                        to: wgt::BufferUses::STORAGE_READ_WRITE,
                    },
                }]);
        }

        unsafe {
            state.general.raw_encoder.dispatch([1, 1, 1]);
        }

        // reset state
        {
            let pipeline = state.pipeline.as_ref().unwrap();

            unsafe {
                state
                    .general
                    .raw_encoder
                    .set_compute_pipeline(pipeline.raw());
            }

            if !state.push_constants.is_empty() {
                unsafe {
                    state.general.raw_encoder.set_push_constants(
                        pipeline.layout.raw(),
                        wgt::ShaderStages::COMPUTE,
                        0,
                        &state.push_constants,
                    );
                }
            }

            for (i, e) in state.general.binder.list_valid() {
                let group = e.group.as_ref().unwrap();
                let raw_bg = group
                    .try_raw(state.general.snatch_guard)
                    .map_err(pass::PassError::DestroyedResource)?;
                unsafe {
                    state.general.raw_encoder.set_bind_group(
                        pipeline.layout.raw(),
                        i as u32,
                        Some(raw_bg),
                        &e.dynamic_offsets,
                    );
                }
            }
        }

        unsafe {
            state
                .general
                .raw_encoder
                .transition_buffers(&[hal::BufferBarrier {
                    buffer: params.dst_buffer,
                    usage: hal::StateTransition {
                        from: wgt::BufferUses::STORAGE_READ_WRITE,
                        to: wgt::BufferUses::INDIRECT,
                    },
                }]);
        }
        unsafe {
            state
                .general
                .raw_encoder
                .dispatch_indirect(params.dst_buffer, 0);
        }
    } else {
        state
            .general
            .scope
            .buffers
            .merge_single(&buffer, wgt::BufferUses::INDIRECT)
            .map_err(pass::PassError::ResourceUsageCompatibility)?;

        let buf_raw = buffer
            .try_raw(state.general.snatch_guard)
            .map_err(pass::PassError::DestroyedResource)?;
        unsafe {
            state.general.raw_encoder.dispatch_indirect(buf_raw, offset);
        }
    }

    Ok(())
}

// Recording a compute pass.
impl Global {
    pub fn compute_pass_set_bind_group(
        &self,
        pass: &mut ComputePass,
        index: u32,
        bind_group_id: Option<id::BindGroupId>,
        offsets: &[DynamicOffset],
    ) -> Result<(), ComputePassError> {
        let scope = PassErrorScope::SetBindGroup;
        let base = pass
            .base
            .as_mut()
            .ok_or(ComputePassErrorInner::PassEnded)
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

        base.commands.push(ArcComputeCommand::SetBindGroup {
            index,
            num_dynamic_offsets: offsets.len(),
            bind_group,
        });

        Ok(())
    }

    pub fn compute_pass_set_pipeline(
        &self,
        pass: &mut ComputePass,
        pipeline_id: id::ComputePipelineId,
    ) -> Result<(), ComputePassError> {
        let redundant = pass.current_pipeline.set_and_check_redundant(pipeline_id);

        let scope = PassErrorScope::SetPipelineCompute;

        let base = pass.base_mut(scope)?;
        if redundant {
            // Do redundant early-out **after** checking whether the pass is ended or not.
            return Ok(());
        }

        let hub = &self.hub;
        let pipeline = hub
            .compute_pipelines
            .get(pipeline_id)
            .get()
            .map_pass_err(scope)?;

        base.commands.push(ArcComputeCommand::SetPipeline(pipeline));

        Ok(())
    }

    pub fn compute_pass_set_push_constants(
        &self,
        pass: &mut ComputePass,
        offset: u32,
        data: &[u8],
    ) -> Result<(), ComputePassError> {
        let scope = PassErrorScope::SetPushConstant;
        let base = pass.base_mut(scope)?;

        if offset & (wgt::PUSH_CONSTANT_ALIGNMENT - 1) != 0 {
            return Err(ComputePassErrorInner::PushConstantOffsetAlignment).map_pass_err(scope);
        }

        if data.len() as u32 & (wgt::PUSH_CONSTANT_ALIGNMENT - 1) != 0 {
            return Err(ComputePassErrorInner::PushConstantSizeAlignment).map_pass_err(scope);
        }
        let value_offset = base
            .push_constant_data
            .len()
            .try_into()
            .map_err(|_| ComputePassErrorInner::PushConstantOutOfMemory)
            .map_pass_err(scope)?;

        base.push_constant_data.extend(
            data.chunks_exact(wgt::PUSH_CONSTANT_ALIGNMENT as usize)
                .map(|arr| u32::from_ne_bytes([arr[0], arr[1], arr[2], arr[3]])),
        );

        base.commands.push(ArcComputeCommand::SetPushConstant {
            offset,
            size_bytes: data.len() as u32,
            values_offset: value_offset,
        });

        Ok(())
    }

    pub fn compute_pass_dispatch_workgroups(
        &self,
        pass: &mut ComputePass,
        groups_x: u32,
        groups_y: u32,
        groups_z: u32,
    ) -> Result<(), ComputePassError> {
        let scope = PassErrorScope::Dispatch { indirect: false };

        let base = pass.base_mut(scope)?;
        base.commands
            .push(ArcComputeCommand::Dispatch([groups_x, groups_y, groups_z]));

        Ok(())
    }

    pub fn compute_pass_dispatch_workgroups_indirect(
        &self,
        pass: &mut ComputePass,
        buffer_id: id::BufferId,
        offset: BufferAddress,
    ) -> Result<(), ComputePassError> {
        let hub = &self.hub;
        let scope = PassErrorScope::Dispatch { indirect: true };
        let base = pass.base_mut(scope)?;

        let buffer = hub.buffers.get(buffer_id).get().map_pass_err(scope)?;

        base.commands
            .push(ArcComputeCommand::DispatchIndirect { buffer, offset });

        Ok(())
    }

    pub fn compute_pass_push_debug_group(
        &self,
        pass: &mut ComputePass,
        label: &str,
        color: u32,
    ) -> Result<(), ComputePassError> {
        let base = pass.base_mut(PassErrorScope::PushDebugGroup)?;

        let bytes = label.as_bytes();
        base.string_data.extend_from_slice(bytes);

        base.commands.push(ArcComputeCommand::PushDebugGroup {
            color,
            len: bytes.len(),
        });

        Ok(())
    }

    pub fn compute_pass_pop_debug_group(
        &self,
        pass: &mut ComputePass,
    ) -> Result<(), ComputePassError> {
        let base = pass.base_mut(PassErrorScope::PopDebugGroup)?;

        base.commands.push(ArcComputeCommand::PopDebugGroup);

        Ok(())
    }

    pub fn compute_pass_insert_debug_marker(
        &self,
        pass: &mut ComputePass,
        label: &str,
        color: u32,
    ) -> Result<(), ComputePassError> {
        let base = pass.base_mut(PassErrorScope::InsertDebugMarker)?;

        let bytes = label.as_bytes();
        base.string_data.extend_from_slice(bytes);

        base.commands.push(ArcComputeCommand::InsertDebugMarker {
            color,
            len: bytes.len(),
        });

        Ok(())
    }

    pub fn compute_pass_write_timestamp(
        &self,
        pass: &mut ComputePass,
        query_set_id: id::QuerySetId,
        query_index: u32,
    ) -> Result<(), ComputePassError> {
        let scope = PassErrorScope::WriteTimestamp;
        let base = pass.base_mut(scope)?;

        let hub = &self.hub;
        let query_set = hub.query_sets.get(query_set_id).get().map_pass_err(scope)?;

        base.commands.push(ArcComputeCommand::WriteTimestamp {
            query_set,
            query_index,
        });

        Ok(())
    }

    pub fn compute_pass_begin_pipeline_statistics_query(
        &self,
        pass: &mut ComputePass,
        query_set_id: id::QuerySetId,
        query_index: u32,
    ) -> Result<(), ComputePassError> {
        let scope = PassErrorScope::BeginPipelineStatisticsQuery;
        let base = pass.base_mut(scope)?;

        let hub = &self.hub;
        let query_set = hub.query_sets.get(query_set_id).get().map_pass_err(scope)?;

        base.commands
            .push(ArcComputeCommand::BeginPipelineStatisticsQuery {
                query_set,
                query_index,
            });

        Ok(())
    }

    pub fn compute_pass_end_pipeline_statistics_query(
        &self,
        pass: &mut ComputePass,
    ) -> Result<(), ComputePassError> {
        let scope = PassErrorScope::EndPipelineStatisticsQuery;
        let base = pass.base_mut(scope)?;
        base.commands
            .push(ArcComputeCommand::EndPipelineStatisticsQuery);

        Ok(())
    }
}
