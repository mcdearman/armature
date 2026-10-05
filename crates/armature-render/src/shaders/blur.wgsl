// Dual Kawase blur (Marius Bjørge, "Bandwidth-efficient rendering", 2015).
// Each downsample halves resolution; each upsample doubles it back.

struct Params {
    half_pixel: vec2<f32>,
    offset: f32,
    _pad: f32,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> u: Params;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VOut {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var out: VOut;
    out.pos = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_down(in: VOut) -> @location(0) vec4<f32> {
    let hp = u.half_pixel * u.offset;
    var sum = textureSample(src, samp, in.uv) * 4.0;
    sum += textureSample(src, samp, in.uv - hp);
    sum += textureSample(src, samp, in.uv + hp);
    sum += textureSample(src, samp, in.uv + vec2<f32>(hp.x, -hp.y));
    sum += textureSample(src, samp, in.uv - vec2<f32>(hp.x, -hp.y));
    return sum / 8.0;
}

@fragment
fn fs_up(in: VOut) -> @location(0) vec4<f32> {
    let hp = u.half_pixel * u.offset;
    var sum = textureSample(src, samp, in.uv + vec2<f32>(-hp.x * 2.0, 0.0));
    sum += textureSample(src, samp, in.uv + vec2<f32>(-hp.x, hp.y)) * 2.0;
    sum += textureSample(src, samp, in.uv + vec2<f32>(0.0, hp.y * 2.0));
    sum += textureSample(src, samp, in.uv + vec2<f32>(hp.x, hp.y)) * 2.0;
    sum += textureSample(src, samp, in.uv + vec2<f32>(hp.x * 2.0, 0.0));
    sum += textureSample(src, samp, in.uv + vec2<f32>(hp.x, -hp.y)) * 2.0;
    sum += textureSample(src, samp, in.uv + vec2<f32>(0.0, -hp.y * 2.0));
    sum += textureSample(src, samp, in.uv + vec2<f32>(-hp.x, -hp.y)) * 2.0;
    return sum / 12.0;
}
