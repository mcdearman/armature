// Every non-text primitive is one instanced quad evaluated with a
// signed distance function. Coordinates are logical pixels; `g.scale`
// converts to physical pixels for anti-aliasing.

struct Globals {
    viewport: vec2<f32>,
    scale: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var backdrop_tex: texture_2d<f32>;
@group(1) @binding(1) var backdrop_samp: sampler;

const FILL: i32 = 0;
const SHADOW: i32 = 1;
const INNER_SHADOW: i32 = 2;
const ARC: i32 = 3;
const GRADIENT: i32 = 4;
const BACKDROP: i32 = 5;
const SEGMENT: i32 = 7;
const AREA: i32 = 8;

struct Inst {
    @location(0) rect: vec4<f32>,
    @location(1) radii: vec4<f32>,
    @location(2) color: vec4<f32>,
    @location(3) color2: vec4<f32>,
    @location(4) params: vec4<f32>,
    @location(5) params2: vec4<f32>,
    @location(6) clip: vec4<f32>,
};

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) p: vec2<f32>,
    @location(1) @interpolate(flat) rect: vec4<f32>,
    @location(2) @interpolate(flat) radii: vec4<f32>,
    @location(3) @interpolate(flat) color: vec4<f32>,
    @location(4) @interpolate(flat) color2: vec4<f32>,
    @location(5) @interpolate(flat) params: vec4<f32>,
    @location(6) @interpolate(flat) params2: vec4<f32>,
    @location(7) @interpolate(flat) clip: vec4<f32>,
};

fn shadow_rect(inst: Inst) -> vec4<f32> {
    let spread = inst.params.z;
    let off = inst.params2.xy;
    return vec4<f32>(inst.rect.xy + off - vec2<f32>(spread), inst.rect.zw + vec2<f32>(2.0 * spread));
}

@vertex
fn vs(@builtin(vertex_index) vi: u32, inst: Inst) -> VOut {
    let kind = i32(inst.params.x);
    var lo = inst.rect.xy;
    var hi = inst.rect.xy + inst.rect.zw;
    if (kind == SHADOW) {
        let s = shadow_rect(inst);
        let e = 3.0 * inst.params.y;
        lo = s.xy - vec2<f32>(e);
        hi = s.xy + s.zw + vec2<f32>(e);
    } else if (kind == SEGMENT) {
        let h = inst.params.y * 0.5;
        lo = min(inst.params2.xy, inst.params2.zw) - vec2<f32>(h);
        hi = max(inst.params2.xy, inst.params2.zw) + vec2<f32>(h);
    } else if (kind == AREA) {
        lo = vec2<f32>(inst.params2.x, min(inst.params2.y, inst.params2.w));
        hi = vec2<f32>(inst.params2.z, inst.params.y);
    }
    let aa = 1.5 / g.scale;
    lo -= vec2<f32>(aa);
    hi += vec2<f32>(aa);
    if (inst.clip.z >= 0.0) {
        lo = max(lo, inst.clip.xy);
        hi = min(hi, inst.clip.xy + inst.clip.zw);
        hi = max(hi, lo);
    }
    let corner = vec2<f32>(f32(vi & 1u), f32(vi >> 1u));
    let p = mix(lo, hi, corner);

    var out: VOut;
    out.pos = vec4<f32>(p.x / g.viewport.x * 2.0 - 1.0, 1.0 - p.y / g.viewport.y * 2.0, 0.0, 1.0);
    out.p = p;
    out.rect = inst.rect;
    out.radii = inst.radii;
    out.color = inst.color;
    out.color2 = inst.color2;
    out.params = inst.params;
    out.params2 = inst.params2;
    out.clip = inst.clip;
    return out;
}

// Corner radius for the quadrant that `q` (relative to centre) falls in.
fn pick_radius(q: vec2<f32>, r: vec4<f32>) -> f32 {
    // r = (top-left, top-right, bottom-right, bottom-left)
    if (q.y < 0.0) {
        return select(r.x, r.y, q.x > 0.0);
    }
    return select(r.w, r.z, q.x > 0.0);
}

fn sd_rrect(p: vec2<f32>, rect: vec4<f32>, radii: vec4<f32>) -> f32 {
    let half = rect.zw * 0.5;
    let q0 = p - (rect.xy + half);
    let rad = min(pick_radius(q0, radii), min(half.x, half.y));
    let q = abs(q0) - half + vec2<f32>(rad);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - rad;
}

fn coverage(d: f32) -> f32 {
    return clamp(0.5 - d * g.scale, 0.0, 1.0);
}

// --- Gaussian rounded-box shadow (after Evan Wallace) -----------------------

fn gaussian(x: f32, sigma: f32) -> f32 {
    return exp(-(x * x) / (2.0 * sigma * sigma)) / (2.5066282746 * sigma);
}

fn erf2(x: vec2<f32>) -> vec2<f32> {
    let s = sign(x);
    let a = abs(x);
    var r = 1.0 + (0.278393 + (0.230389 + 0.078108 * (a * a)) * a) * a;
    r = r * r;
    return s - s / (r * r);
}

fn box_shadow_x(x: f32, y: f32, sigma: f32, corner: f32, half: vec2<f32>) -> f32 {
    let delta = min(half.y - corner - abs(y), 0.0);
    let curved = half.x - corner + sqrt(max(0.0, corner * corner - delta * delta));
    let integral = 0.5 + 0.5 * erf2((x + vec2<f32>(-curved, curved)) * (0.70710678 / sigma));
    return integral.y - integral.x;
}

// Blurred coverage of a rounded box at point `p`.
fn box_shadow(rect: vec4<f32>, radii: vec4<f32>, p: vec2<f32>, sigma: f32) -> f32 {
    if (sigma < 0.01) {
        return coverage(sd_rrect(p, rect, radii));
    }
    let half = rect.zw * 0.5;
    let q = p - (rect.xy + half);
    let corner = min(pick_radius(q, radii), min(half.x, half.y));
    let low = q.y - half.y;
    let high = q.y + half.y;
    let start = clamp(-3.0 * sigma, low, high);
    let end = clamp(3.0 * sigma, low, high);
    let step = (end - start) / 4.0;
    var y = start + step * 0.5;
    var value = 0.0;
    for (var i = 0; i < 4; i++) {
        value += box_shadow_x(q.x, q.y - y, sigma, corner, half) * gaussian(y, sigma) * step;
        y += step;
    }
    return value;
}

fn premul(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(c.rgb * c.a, c.a);
}

const TAU: f32 = 6.28318530718;

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    if (in.clip.z >= 0.0) {
        let c = in.clip;
        if (in.p.x < c.x || in.p.y < c.y || in.p.x > c.x + c.z || in.p.y > c.y + c.w) {
            discard;
        }
    }
    let kind = i32(in.params.x);
    let p = in.p;

    if (kind == FILL) {
        let d = sd_rrect(p, in.rect, in.radii);
        let outer = coverage(d);
        let bw = in.params.y;
        if (bw <= 0.0) {
            return premul(in.color) * outer;
        }
        let inner = coverage(d + bw);
        return premul(in.color) * inner + premul(in.color2) * max(outer - inner, 0.0);
    }

    if (kind == SHADOW) {
        let sigma = in.params.y;
        let spread = in.params.z;
        let srect = vec4<f32>(in.rect.xy + in.params2.xy - vec2<f32>(spread), in.rect.zw + vec2<f32>(2.0 * spread));
        let sradii = max(in.radii + vec4<f32>(spread), vec4<f32>(0.0));
        let s = box_shadow(srect, sradii, p, sigma);
        // Like CSS, an outer shadow never paints under its own box.
        let inside = coverage(sd_rrect(p, in.rect, in.radii));
        return premul(in.color) * s * (1.0 - inside);
    }

    if (kind == INNER_SHADOW) {
        let sigma = in.params.y;
        let spread = in.params.z;
        let box_cov = coverage(sd_rrect(p, in.rect, in.radii));
        let hole = vec4<f32>(in.rect.xy + in.params2.xy + vec2<f32>(spread), in.rect.zw - vec2<f32>(2.0 * spread));
        let hradii = max(in.radii - vec4<f32>(spread), vec4<f32>(0.0));
        var lit = 0.0;
        if (hole.z > 0.0 && hole.w > 0.0) {
            lit = box_shadow(hole, hradii, p, sigma);
        }
        return premul(in.color) * box_cov * clamp(1.0 - lit, 0.0, 1.0);
    }

    if (kind == ARC) {
        let c = in.rect.xy + in.rect.zw * 0.5;
        let t = in.params.y;
        let r = min(in.rect.z, in.rect.w) * 0.5 - t * 0.5;
        let start = in.params.z;
        let sweep = in.params.w;
        let v = p - c;
        let len = length(v);
        var d: f32;
        if (sweep >= TAU - 0.0001) {
            d = abs(len - r) - t * 0.5;
        } else {
            let a = atan2(v.x, -v.y);
            let rel = fract((a - start) / TAU) * TAU;
            if (rel <= sweep) {
                d = abs(len - r) - t * 0.5;
            } else {
                let e0 = c + r * vec2<f32>(sin(start), -cos(start));
                let e1 = c + r * vec2<f32>(sin(start + sweep), -cos(start + sweep));
                d = min(length(p - e0), length(p - e1)) - t * 0.5;
            }
        }
        return premul(in.color) * coverage(d);
    }

    if (kind == GRADIENT) {
        let a = in.params2.xy;
        let b = in.params2.zw;
        let dir = b - a;
        let tt = clamp(dot(p - a, dir) / max(dot(dir, dir), 0.0001), 0.0, 1.0);
        let col = mix(premul(in.color), premul(in.color2), tt);
        return col * coverage(sd_rrect(p, in.rect, in.radii));
    }

    if (kind == BACKDROP) {
        let uv = p / g.viewport;
        let blurred = textureSampleLevel(backdrop_tex, backdrop_samp, uv, 0.0);
        let tint = premul(in.color);
        let col = tint + blurred * (1.0 - tint.a);
        return col * coverage(sd_rrect(p, in.rect, in.radii));
    }

    if (kind == SEGMENT) {
        let a = in.params2.xy;
        let b = in.params2.zw;
        let pa = p - a;
        let ba = b - a;
        let h = clamp(dot(pa, ba) / max(dot(ba, ba), 0.0001), 0.0, 1.0);
        let d = length(pa - ba * h) - in.params.y * 0.5;
        return premul(in.color) * coverage(d);
    }

    if (kind == AREA) {
        let x0 = in.params2.x;
        let x1 = in.params2.z;
        if (p.x < x0 || p.x >= x1) {
            discard;
        }
        let base = in.params.y;
        let top = mix(in.params2.y, in.params2.w, (p.x - x0) / max(x1 - x0, 0.0001));
        let cov = clamp((p.y - top) * g.scale + 0.5, 0.0, 1.0) * clamp((base - p.y) * g.scale + 0.5, 0.0, 1.0);
        let lo = in.params.z;
        let tt = clamp((p.y - lo) / max(base - lo, 0.0001), 0.0, 1.0);
        return mix(premul(in.color), premul(in.color2), tt) * cov;
    }

    discard;
}
