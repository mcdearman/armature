// Draws one image as a textured quad. The texture holds sRGB-encoded,
// straight-alpha pixels; the canvas wants them premultiplied.

struct Globals {
    viewport: vec2<f32>,
    scale: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct Inst {
    @location(0) rect: vec4<f32>,
    // The part of the image to show: u0, v0, u1, v1.
    @location(1) uv: vec4<f32>,
    @location(2) clip: vec4<f32>,
    // x: opacity. y: quarter turns clockwise, 0 to 3.
    @location(3) params: vec4<f32>,
};

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) opacity: f32,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, inst: Inst) -> VOut {
    var lo = inst.rect.xy;
    var hi = inst.rect.xy + inst.rect.zw;
    // Clipping moves the quad's corners, and the texture coordinates with
    // them, so nothing outside the clip is ever shaded.
    if (inst.clip.z >= 0.0) {
        lo = max(lo, inst.clip.xy);
        hi = min(hi, inst.clip.xy + inst.clip.zw);
        hi = max(hi, lo);
    }
    let corner = vec2<f32>(f32(vi & 1u), f32(vi >> 1u));
    let p = mix(lo, hi, corner);
    let t = (p - inst.rect.xy) / max(inst.rect.zw, vec2<f32>(0.0001));

    var out: VOut;
    out.pos = vec4<f32>(p.x / g.viewport.x * 2.0 - 1.0, 1.0 - p.y / g.viewport.y * 2.0, 0.0, 1.0);
    // Turning the picture clockwise means reading it turned the other way.
    var s = t;
    let turns = i32(inst.params.y) % 4;
    if (turns == 1) {
        s = vec2<f32>(t.y, 1.0 - t.x);
    } else if (turns == 2) {
        s = vec2<f32>(1.0 - t.x, 1.0 - t.y);
    } else if (turns == 3) {
        s = vec2<f32>(1.0 - t.y, t.x);
    }
    out.uv = mix(inst.uv.xy, inst.uv.zw, s);
    out.opacity = inst.params.x;
    return out;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv);
    return vec4<f32>(c.rgb * c.a, c.a) * in.opacity;
}
