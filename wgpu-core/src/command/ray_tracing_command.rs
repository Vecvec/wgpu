use alloc::sync::Arc;

use crate::{
    binding_model::BindGroup,
    pipeline::RayTracingPipeline,
    resource::{Buffer, QuerySet},
};

/// Equivalent to `RayTracingCommand` but the Ids resolved into resource Arcs.
#[derive(Clone, Debug)]
pub enum ArcRayTracingCommand {
    SetBindGroup {
        index: u32,
        num_dynamic_offsets: usize,
        bind_group: Option<Arc<BindGroup>>,
    },

    SetPipeline(Arc<RayTracingPipeline>),

    /// Set a range of push constants to values stored in `push_constant_data`.
    SetPushConstant {
        /// Which stages we are setting push constant values for.
        stages: wgt::ShaderStages,

        /// The byte offset within the push constant storage to write to. This
        /// must be a multiple of four.
        offset: u32,

        /// The number of bytes to write. This must be a multiple of four.
        size_bytes: u32,

        /// Index in `push_constant_data` of the start of the data
        /// to be written.
        ///
        /// Note: this is not a byte offset like `offset`. Rather, it is the
        /// index of the first `u32` element in `push_constant_data` to read.
        values_offset: u32,
    },

    TraceRays([u32; 3]),

    TraceRaysIndirect {
        buffer: Arc<Buffer>,
        offset: wgt::BufferAddress,
    },

    PushDebugGroup {
        len: usize,
    },

    PopDebugGroup,

    InsertDebugMarker {
        len: usize,
    },

    WriteTimestamp {
        query_set: Arc<QuerySet>,
        query_index: u32,
    },

    BeginPipelineStatisticsQuery {
        query_set: Arc<QuerySet>,
        query_index: u32,
    },

    EndPipelineStatisticsQuery,
}
