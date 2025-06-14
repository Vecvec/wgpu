struct Payload {
    payload: u32,
}

@ray_generation
fn empty_ray_gen() {}

var<ray_payload> unused_payload: Payload;

@ray_generation
@payload_type(Payload)
fn ray_gen_with_unused_payload_type() {}

@ray_generation
@payload_type(Payload)
fn ray_gen() {
    payload.payload = 5;
}

@ray_closest_hit
@incoming_payload(incoming_payload)
fn closest_hit() {
    incoming_payload.payload++;
}

@ray_closest_hit
@incoming_payload(incoming_payload)
@payload_type(Payload)
fn closest_hit_same_payload() {
    incoming_payload.payload++;
}

@ray_closest_hit
@incoming_payload(incoming_payload)
@payload_type(Payload)
fn closest_hit_different_payload() {
    payload.payload = 3;
    incoming_payload.payload++;
}

var<incoming_ray_payload> incoming_payload: Payload;
var<ray_payload> payload: Payload;

@ray_any_hit
@incoming_payload(incoming_payload)
fn any_hit() {
    incoming_payload.payload++;
}