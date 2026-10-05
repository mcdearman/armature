// Copies the sRGB-encoded, premultiplied canvas to the window surface.

override TO_LINEAR: bool = false;
override UNPREMULTIPLY: bool = false;

@group(0) @binding(0) var src: texture_2d<f32>;

struct VOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VOut {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var out: VOut;
    out.pos = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return out;
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    var c = textureLoad(src, vec2<i32>(in.pos.xy), 0);
    if (TO_LINEAR || UNPREMULTIPLY) {
        var rgb = c.rgb;
        if (c.a > 0.0) {
            rgb = rgb / c.a;
        }
        if (TO_LINEAR) {
            rgb = to_linear(rgb);
        }
        if (!UNPREMULTIPLY) {
            rgb = rgb * c.a;
        }
        c = vec4<f32>(rgb, c.a);
    }
    return c;
}
