@workgroup_size(1)
@compute
fn main() {
    textureStore(tex, vec2<u32>(), vec4<f32>());
}

@group(0) @binding(0)
var tex: texture_storage_2d<f32, write>;