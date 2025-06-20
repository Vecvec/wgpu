struct Payload {
    payload: u32,
}

@ray_generation
fn empty_ray_gen() {}

var<ray_payload> unused_payload: Payload;

@ray_generation
@payload_type(Payload)
fn ray_gen_with_unused_payload_type(
    @builtin(ray_launch_size) launch_size: vec3<u32>,
    @builtin(ray_launch_id) launch_id: vec3<u32>,
) {}

@ray_generation
@payload_type(Payload)
fn ray_gen() {
    payload.payload = 5;
    traceRays(acc_struct, RayDesc(), &payload);
}

@group(0) @binding(0)
var acc_struct: acceleration_structure;

@ray_closest_hit
@incoming_payload(incoming_payload)
fn closest_hit(
    @builtin(ray_launch_size) launch_size: vec3<u32>,
    @builtin(ray_launch_id) launch_id: vec3<u32>,
    @builtin(world_ray_origin) world_origin: vec3<f32>,
    @builtin(world_ray_direction) world_direction: vec3<f32>,
    @builtin(t_min) t_min: f32,
    @builtin(t) t: f32,
    @builtin(object_ray_origin) object_origin: vec3<f32>,
    @builtin(object_ray_direction) object_direction: vec3<f32>,
    @builtin(object_to_world) object_to_world: mat4x3<f32>,
    @builtin(world_to_object) world_to_object: mat4x3<f32>,
    @builtin(instance_custom_data) data: u32,
    @builtin(hit_kind) kind: u32,
    @builtin(incoming_flags) flags: u32,
    @builtin(instance_index) instance_idx: u32,
    @builtin(geometry_index) geometry_idx: u32,
    @builtin(primitive_index) primitive_index: u32,
) {
    incoming_payload.payload++;
}

@ray_closest_hit
@incoming_payload(incoming_payload)
@payload_type(Payload)
fn closest_hit_same_payload() {
    incoming_payload.payload++;
    traceRays(acc_struct, RayDesc(), &incoming_payload);
}

@ray_closest_hit
@incoming_payload(incoming_payload)
@payload_type(Payload)
fn closest_hit_different_payload() {
    payload.payload = 3;
    incoming_payload.payload++;
    traceRays(acc_struct, RayDesc(), &payload);
}

var<incoming_ray_payload> incoming_payload: Payload;
var<ray_payload> payload: Payload;

@ray_any_hit
@incoming_payload(incoming_payload)
fn any_hit(
    @builtin(ray_launch_size) launch_size: vec3<u32>,
    @builtin(ray_launch_id) launch_id: vec3<u32>,
    @builtin(world_ray_origin) world_origin: vec3<f32>,
    @builtin(world_ray_direction) world_direction: vec3<f32>,
    @builtin(t_min) t_min: f32,
    @builtin(t) t: f32,
    @builtin(object_ray_origin) object_origin: vec3<f32>,
    @builtin(object_ray_direction) object_direction: vec3<f32>,
    @builtin(object_to_world) object_to_world: mat4x3<f32>,
    @builtin(world_to_object) world_to_object: mat4x3<f32>,
    @builtin(instance_custom_data) data: u32,
    @builtin(hit_kind) kind: u32,
    @builtin(incoming_flags) flags: u32,
    @builtin(instance_index) instance_idx: u32,
    @builtin(geometry_index) geometry_idx: u32,
    @builtin(primitive_index) primitive_index: u32,
) {
    if (data == 0) {
        return;
    } else if (data == 1) {
        discard_hit;
    } else if (data == 2) {
        accept_hit_end_search;
    }
    incoming_payload.payload++;
}

@ray_miss
@incoming_payload(incoming_payload)
fn miss(
    @builtin(ray_launch_size) launch_size: vec3<u32>,
    @builtin(ray_launch_id) launch_id: vec3<u32>,
    @builtin(world_ray_origin) world_origin: vec3<f32>,
    @builtin(world_ray_direction) world_direction: vec3<f32>,
    @builtin(t_min) t_min: f32,
    @builtin(t_max) t: f32,
    @builtin(incoming_flags) flags: u32,
) {
    incoming_payload.payload = 0;
}