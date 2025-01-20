@group(0) @binding(0)
var acc_struct: acceleration_structure;

@ray_gen
fn ray_gen() {
    var colour = vec4<f32>();
    /* trace ray behaviour (pseudocode)
    fn traceRay<T>(acc_struct: acceleration_structure, desc: RayDesc, colour: ptr<private, T>) {
        for (var i = 0u; i < acc_struct.num_objects; i++) {
            if (intersected object) {
                 *colour = any_hit()
            }
        }
    }
    */
    traceRay(acc_struct, RayDesc(), &colour);
    trace();
    return;
}

fn trace() {
    var colour = vec4<f32>();
    traceRay(acc_struct, RayDesc(), &colour);
}

@ray_any
fn discard_any_hit(@builtin(payload) colour: ptr<ray_tracing, vec4<f32>>, @builtin(intersection) intersection: TriRayIntersection) {
    *colour = vec4<f32>();
    discard_hit;
}
/* in glsl `@builtin(payload) colour: ptr<private, vec4<f32>>` is
`layout(location = 0) rayPayloadInEXT vec4<f32> colour;`
but in hlsl it is `inout vec4<f32>> colour`
in wgsl a pointer seems to best represent both
*/
@ray_any
fn any_hit(@builtin(payload) colour: ptr<ray_tracing, vec4<f32>>, @builtin(intersection) intersection: TriRayIntersection, @builtin(ray_t) t: f32) {
    *colour = vec4<f32>();
    return;
}

@ray_closest
fn closest_hit(@builtin(payload) colour: ptr<ray_tracing, vec4<f32>>, @builtin(intersection) intersection: TriRayIntersection, @builtin(ray_t) t: f32) {
    *colour = vec4<f32>(1.0);
    return;
}

@ray_miss
fn miss(@builtin(payload) colour: ptr<ray_tracing, vec4<f32>>) {
    *colour = vec4<f32>();
    return;
}

@intersection
fn intersect_return() {
    let has_accepted = ReportIntersection(0.5, 5u, 0u);
    return;
}

struct Container {
    inner: u32,
}

@intersection
fn intersect_struct() {
    let has_accepted = ReportIntersection(0.5, 5u, Container(0u));
    return;
}