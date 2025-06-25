struct Payload {
    payload: u32,
}

struct RayDesc {
    flags: u32,
    cull_mask: u32,
    tmin: f32,
    tmax: f32,
    origin: vec3<f32>,
    dir: vec3<f32>,
}

var<ray_payload> payload: Payload;
@group(0) @binding(0) 
var acc_struct: acceleration_structure;
var<incoming_ray_payload> incoming_payload: Payload;

@ray_generation 
fn empty_ray_gen() {
    return;
}

@ray_generation @payload_type(Payload)
fn ray_gen_with_unused_payload_type(@builtin(ray_launch_size) launch_size: vec3<u32>, @builtin(ray_launch_id) launch_id: vec3<u32>) {
    return;
}

@ray_generation @payload_type(Payload)
fn ray_gen() {
    payload.payload = 5u;
    traceRays(acc_struct, RayDesc(), (&payload));
    return;
}

@ray_closest_hit @incoming_payload(incoming_payload) 
fn closest_hit(@builtin(ray_launch_size) launch_size_1: vec3<u32>, @builtin(ray_launch_id) launch_id_1: vec3<u32>, @builtin(world_ray_origin) world_origin: vec3<f32>, @builtin(world_ray_direction) world_direction: vec3<f32>, @builtin(t_min) t_min: f32, @builtin(t) t: f32, @builtin(object_ray_origin) object_origin: vec3<f32>, @builtin(object_ray_direction) object_direction: vec3<f32>, @builtin(object_to_world) object_to_world: mat4x3<f32>, @builtin(world_to_object) world_to_object: mat4x3<f32>, @builtin(instance_custom_data) data: u32, @builtin(hit_kind) kind: u32, @builtin(incoming_flags) flags: u32, @builtin(instance_index) instance_idx: u32, @builtin(geometry_index) geometry_idx: u32, @builtin(primitive_index) primitive_index: u32) {
    let _e19 = incoming_payload.payload;
    incoming_payload.payload = (_e19 + 1u);
    return;
}

@ray_closest_hit @payload_type(Payload)@incoming_payload(incoming_payload) 
fn closest_hit_same_payload() {
    let _e3 = incoming_payload.payload;
    incoming_payload.payload = (_e3 + 1u);
    traceRays(acc_struct, RayDesc(), (&incoming_payload));
    return;
}

@ray_closest_hit @payload_type(Payload)@incoming_payload(incoming_payload) 
fn closest_hit_different_payload() {
    payload.payload = 3u;
    let _e6 = incoming_payload.payload;
    incoming_payload.payload = (_e6 + 1u);
    traceRays(acc_struct, RayDesc(), (&payload));
    return;
}

@ray_any_hit @incoming_payload(incoming_payload) 
fn any_hit(@builtin(ray_launch_size) launch_size_2: vec3<u32>, @builtin(ray_launch_id) launch_id_2: vec3<u32>, @builtin(world_ray_origin) world_origin_1: vec3<f32>, @builtin(world_ray_direction) world_direction_1: vec3<f32>, @builtin(t_min) t_min_1: f32, @builtin(t) t_1: f32, @builtin(object_ray_origin) object_origin_1: vec3<f32>, @builtin(object_ray_direction) object_direction_1: vec3<f32>, @builtin(object_to_world) object_to_world_1: mat4x3<f32>, @builtin(world_to_object) world_to_object_1: mat4x3<f32>, @builtin(instance_custom_data) data_1: u32, @builtin(hit_kind) kind_1: u32, @builtin(incoming_flags) flags_1: u32, @builtin(instance_index) instance_idx_1: u32, @builtin(geometry_index) geometry_idx_1: u32, @builtin(primitive_index) primitive_index_1: u32) {
    if (data_1 == 0u) {
        return;
    } else {
        if (data_1 == 1u) {
            discard_hit;
        } else {
            if (data_1 == 2u) {
                accept_hit_end_search;
            }
        }
    }
    let _e25 = incoming_payload.payload;
    incoming_payload.payload = (_e25 + 1u);
    return;
}

@ray_miss @incoming_payload(incoming_payload) 
fn miss(@builtin(ray_launch_size) launch_size_3: vec3<u32>, @builtin(ray_launch_id) launch_id_3: vec3<u32>, @builtin(world_ray_origin) world_origin_2: vec3<f32>, @builtin(world_ray_direction) world_direction_2: vec3<f32>, @builtin(t_min) t_min_2: f32, @builtin(t_max) t_2: f32, @builtin(incoming_flags) flags_2: u32) {
    incoming_payload.payload = 0u;
    return;
}
