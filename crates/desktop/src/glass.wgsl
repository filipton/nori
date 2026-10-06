// The compositor's shaders: a quad placed in pixels, a plain copy (the page), a premultiplied copy (the
// sidebar's rows, the player's controls), a blur pyramid of the page (each level half the last, made with a
// 13-tap downsample so nothing is skipped), the glass, and the lyrics' focus.
//
// Blur levels: level 0 is the page itself, level k the pyramid's mip k - 1 (1/2^k of the page). A level is
// read with a cubic B-spline over four bilinear taps, so a small level drawn large stays smooth rather than
// blocky; a fractional level mixes the two levels beside it, never the sharp page with a far blur.
//
// The glass, after macOS 26's glass layers: the backdrop blurred and made vivid, dimmed only where it is
// brighter than white controls can stand on (judged over a wider blur, so the picture keeps its own
// contrast), its black lifted a little; the view bent outwards towards the rim; and two highlights on
// opposite corners taking the colour of what lies behind them.

struct U {
    rect: vec4<f32>,      // where the quad goes, target pixels (x, y, w, h)
    view: vec4<f32>,      // target size (w, h), source size (w, h)
    shape: vec4<f32>,     // corner radius, bevel band, refraction, dispersion (pixels, pixels, pixels, ratio)
    face: vec4<f32>,      // glass: black's lift, the brightest it lets through (luma), saturation, blur level
    light: vec4<f32>,     // glass: rim, spill, its reach (pixels), glow level; focus: band top, height, pixels per blur step, scale
    gather: vec4<f32>,    // glass: how far past the edge it gathers light (pixels), how much colour it keeps
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;
@group(0) @binding(3) var pyramid: texture_2d<f32>;

struct V {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> V {
    let corner = vec2<f32>(f32(i & 1u), f32((i >> 1u) & 1u));
    let px = u.rect.xy + corner * u.rect.zw;
    var o: V;
    o.pos = vec4<f32>(px.x / u.view.x * 2.0 - 1.0, 1.0 - px.y / u.view.y * 2.0, 0.0, 1.0);
    o.uv = corner;
    return o;
}

@fragment
fn fs_copy(v: V) -> @location(0) vec4<f32> {
    return textureSampleLevel(src, smp, v.uv, 0.0);
}

// One pyramid level from the one above it: thirteen taps (Jimenez), every source texel counted.
@fragment
fn fs_down(v: V) -> @location(0) vec4<f32> {
    let t = 1.0 / vec2<f32>(textureDimensions(src));
    let s = v.uv;
    let a = textureSampleLevel(src, smp, s + t * vec2<f32>(-2.0, -2.0), 0.0);
    let b = textureSampleLevel(src, smp, s + t * vec2<f32>(0.0, -2.0), 0.0);
    let c = textureSampleLevel(src, smp, s + t * vec2<f32>(2.0, -2.0), 0.0);
    let d = textureSampleLevel(src, smp, s + t * vec2<f32>(-2.0, 0.0), 0.0);
    let e = textureSampleLevel(src, smp, s, 0.0);
    let f = textureSampleLevel(src, smp, s + t * vec2<f32>(2.0, 0.0), 0.0);
    let g = textureSampleLevel(src, smp, s + t * vec2<f32>(-2.0, 2.0), 0.0);
    let h = textureSampleLevel(src, smp, s + t * vec2<f32>(0.0, 2.0), 0.0);
    let i = textureSampleLevel(src, smp, s + t * vec2<f32>(2.0, 2.0), 0.0);
    let j = textureSampleLevel(src, smp, s + t * vec2<f32>(-1.0, -1.0), 0.0);
    let k = textureSampleLevel(src, smp, s + t * vec2<f32>(1.0, -1.0), 0.0);
    let l = textureSampleLevel(src, smp, s + t * vec2<f32>(-1.0, 1.0), 0.0);
    let m = textureSampleLevel(src, smp, s + t * vec2<f32>(1.0, 1.0), 0.0);
    return e * 0.125 + (a + c + g + i) * 0.03125 + (b + d + f + h) * 0.0625 + (j + k + l + m) * 0.125;
}

// Mip `lod` of the pyramid at `at`, through a cubic B-spline: four bilinear taps.
fn smooth_mip(at: vec2<f32>, lod: i32) -> vec4<f32> {
    let size = vec2<f32>(textureDimensions(pyramid, lod));
    let p = at * size - 0.5;
    let i = floor(p);
    let f = p - i;
    let f2 = f * f;
    let f3 = f2 * f;
    let w0 = (1.0 - 3.0 * f + 3.0 * f2 - f3) / 6.0;
    let w1 = (4.0 - 6.0 * f2 + 3.0 * f3) / 6.0;
    let w2 = (1.0 + 3.0 * f + 3.0 * f2 - 3.0 * f3) / 6.0;
    let w3 = f3 / 6.0;
    let g0 = w0 + w1;
    let g1 = w2 + w3;
    let h0 = (i - 0.5 + w1 / g0) / size;
    let h1 = (i + 1.5 + w3 / g1) / size;
    let l = f32(lod);
    return (textureSampleLevel(pyramid, smp, vec2<f32>(h0.x, h0.y), l) * g0.x + textureSampleLevel(pyramid, smp, vec2<f32>(h1.x, h0.y), l) * g1.x) * g0.y
        + (textureSampleLevel(pyramid, smp, vec2<f32>(h0.x, h1.y), l) * g0.x + textureSampleLevel(pyramid, smp, vec2<f32>(h1.x, h1.y), l) * g1.x) * g1.y;
}

// The page at `at` (0..1) blurred to `level`: 0 sharp, each level about twice the blur of the one before.
fn blurred(at: vec2<f32>, level: f32) -> vec4<f32> {
    let top = f32(textureNumLevels(pyramid));
    let l = clamp(level, 0.0, top);
    if (l < 1.0) {
        return mix(textureSampleLevel(src, smp, at, 0.0), smooth_mip(at, 0), l);
    }
    let k = min(floor(l), top - 1.0);
    let below = smooth_mip(at, i32(k) - 1);
    if (k >= top - 1.0 || l == k) {
        return below;
    }
    return mix(below, smooth_mip(at, i32(k)), l - k);
}

// Half a step of 8-bit colour of noise, so a smooth gradient does not band.
fn dither(p: vec2<f32>) -> vec3<f32> {
    let n = fract(sin(dot(p, vec2<f32>(12.9898, 78.233))) * 43758.5453);
    return vec3<f32>((n - 0.5) / 255.0);
}

fn sd_round_rect(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half + vec2<f32>(r);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

fn field(p: vec2<f32>) -> f32 {
    let half = u.rect.zw * 0.5;
    return sd_round_rect(p - (u.rect.xy + half), half, u.shape.x);
}

fn look(p: vec2<f32>) -> vec3<f32> {
    return blurred(clamp(p / u.view.xy, vec2<f32>(0.0), vec2<f32>(1.0)), u.face.w).rgb;
}

// The page blurred so far that only its light is left: what the glass takes from beside it.
fn glow(p: vec2<f32>) -> vec3<f32> {
    return blurred(clamp(p / u.view.xy, vec2<f32>(0.0), vec2<f32>(1.0)), u.light.w).rgb;
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn saturate_by(c: vec3<f32>, s: f32) -> vec3<f32> {
    return mix(vec3<f32>(luma(c)), c, s);
}

@fragment
fn fs_glass(v: V) -> @location(0) vec4<f32> {
    let p = u.rect.xy + v.uv * u.rect.zw;
    let d = field(p);
    let cover = clamp(0.5 - d, 0.0, 1.0);
    if (cover <= 0.0) {
        discard;
    }
    // The outward normal: the field's gradient.
    let e = 1.0;
    var n = vec2<f32>(field(p + vec2<f32>(e, 0.0)) - field(p - vec2<f32>(e, 0.0)), field(p + vec2<f32>(0.0, e)) - field(p - vec2<f32>(0.0, e)));
    let nl = length(n);
    n = select(vec2<f32>(0.0), n / nl, nl > 0.0001);
    // 0 at the rim, 1 past the bevel. The bevel is a squircle's shoulder: the lens bends most at the very
    // edge and lets go smoothly inwards.
    let inside = clamp(-d / u.shape.y, 0.0, 1.0);
    let bend = 1.0 - pow(1.0 - pow(1.0 - inside, 4.0), 0.25);
    let off = n * u.shape.z * bend;
    // Outwards: the rim shows what lies beyond it; blue bends a little further than red.
    let seen = vec3<f32>(look(p + off * (1.0 - u.shape.w)).r, look(p + off).g, look(p + off * (1.0 + u.shape.w)).b);
    // Vivid, dimmed where what lies around is too bright, and black lifted.
    let vivid = clamp(saturate_by(seen, u.face.z), vec3<f32>(0.0), vec3<f32>(1.0));
    let around = luma(blurred(clamp(p / u.view.xy, vec2<f32>(0.0), vec2<f32>(1.0)), u.face.w + 2.0).rgb);
    var col = vivid * min(1.0, u.face.y / max(around, 0.001));
    col = col * (1.0 - u.face.x) + vec3<f32>(u.face.x);
    // The light of what lies beside the pane washes in: gathered along a band past the nearest edge (out to
    // `gather.x`), the colourful parts counting for more than a plain dark ground, strongest at the edge and
    // fading inwards over `light.z`.
    if (u.light.y > 0.0) {
        let depth = max(-d, 0.0);
        var acc = vec3<f32>(0.0);
        var wsum = 0.0001;
        for (var i = 0; i < 5; i = i + 1) {
            let t = (f32(i) + 0.5) / 5.0;
            let c = glow(p + n * (depth + 8.0 + t * u.gather.x));
            let hi = max(c.r, max(c.g, c.b));
            let lo = min(c.r, min(c.g, c.b));
            let w = (1.0 - 0.6 * t) * (0.25 + 3.0 * (hi - lo) + hi);
            acc += c * w;
            wsum += w;
        }
        let beyond = max(saturate_by(acc / wsum, u.gather.y), vec3<f32>(0.0));
        let spill = clamp(u.light.y * exp(-depth / max(u.light.z, 1.0)), 0.0, 1.0);
        col = mix(col, beyond * min(1.0, u.face.y / max(luma(beyond), 0.001)) + vec3<f32>(u.face.x), spill);
    }
    // Two highlights on opposite corners (up-left and down-right), along the rim, in the vivid colour of
    // what they sit over.
    let rim = pow(1.0 - inside, 3.0);
    let corner = abs(dot(n, vec2<f32>(-0.7071, -0.7071)));
    let lit = pow(corner, 2.0);
    let hue = clamp(saturate_by(seen, 2.55) + 0.45 * seen + vec3<f32>(0.05), vec3<f32>(0.0), vec3<f32>(1.0));
    col += mix(vec3<f32>(1.0), hue, 0.5) * (rim * (0.35 + 0.65 * lit) * u.light.x);
    col += dither(p);
    return vec4<f32>(col * cover, cover);
}

// Focus: the lyrics beside the artwork in Now Playing. Within `rect`, the page sharp in a band around the line
// sung (`light.x` its top, `light.y` its height, pixels), each line further off blurred a step more (`light.z`
// pixels a step, as a line and its gap), up to five points' blur, as Music draws the lines not sung.
@fragment
fn fs_focus(v: V) -> @location(0) vec4<f32> {
    let p = u.rect.xy + v.uv * u.rect.zw;
    let away = max(max(u.light.x - p.y, p.y - (u.light.x + u.light.y)), 0.0);
    // Points of blur: a point at the band's edge, ramping in over its first 16, then one more a line.
    let points = min(min(away / (16.0 * u.light.w), 1.0) + away / u.light.z, 5.0);
    // The level whose blur that is: level k blurs about 0.6 * 2^k pixels.
    let sigma = points * u.light.w;
    let level = select(0.0, log2(max(sigma, 0.6) / 0.6), sigma > 0.0);
    let c = blurred(p / u.view.xy, level);
    return vec4<f32>(c.rgb + dither(p), c.a);
}
