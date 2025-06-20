@group(0) @binding(0)
var acceleration_struct: acceleration_structure;

struct Ray {
    origin: vec3<f32>,
    direction: vec3<f32>,
}

@group(0) @binding(1)
var<storage> rays_to_trace: array<Ray>;

struct Payload {
    closest_hit_count: u32,
    any_hit_count: u32,
    miss_count: u32,
}

var<ray_payload> payload: Payload;

@ray_generation
@payload_type(Payload)
fn ray_gen(@builtin(ray_launch_id) launch_id: vec3<u32>) {
    let ray = rays_to_trace[launch_id.x];
    traceRays(acceleration_struct, RayDesc(0, 0xFFu, 0.1, 100.0, ray.origin, ray.direction), &payload);
}

var<incoming_ray_payload> incoming_payload: Payload;

@ray_closest_hit
@payload_type(Payload)
@incoming_payload(incoming_payload)
fn closest_hit() {
    incoming_payload.closest_hit_count++;
}

@ray_any_hit
@incoming_payload(incoming_payload)
fn any_hit() {
    incoming_payload.any_hit_count++;
}

@ray_miss
@payload_type(Payload)
@incoming_payload(incoming_payload)
fn miss() {
    incoming_payload.miss_count++;
}
