// The compositor's shaders: a quad placed in pixels, a plain copy (the page), a premultiplied copy (the
// sidebar's rows, the player's controls), a separable blur (the page, a quarter size, for the glass to
// look through), and the glass itself.
//
// The glass follows the way Liquid Glass reads (and the published shader work that recreates it): the pane
// is a signed distance field; through its body the page shows blurred and tinted; towards its rim the view
// bends outwards (the distance to the edge sets how far, the field's gradient which way), so the rim
// carries what lies just beyond it, colours a little apart; and the rim catches light from above.

struct U {
    rect: vec4<f32>,      // where the quad goes, target pixels (x, y, w, h)
    view: vec4<f32>,      // target size (w, h), source size (w, h)
    shape: vec4<f32>,     // corner radius, bevel band, refraction, dispersion (pixels, pixels, pixels, ratio)
    tint: vec4<f32>,      // the glass's own colour and how much of it
    light: vec4<f32>,     // rim light, lift over dark ground; blur: direction (x, y); glass: spill, its reach (pixels)
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;
@group(0) @binding(3) var blurred: texture_2d<f32>;

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

// Nine taps of a Gaussian along `light.zw`, on linear filtering's free in-between samples.
@fragment
fn fs_blur(v: V) -> @location(0) vec4<f32> {
    let step = u.light.zw / u.view.zw;
    var c = textureSampleLevel(src, smp, v.uv, 0.0) * 0.2270270270;
    c += textureSampleLevel(src, smp, v.uv + step * 1.3846153846, 0.0) * 0.3162162162;
    c += textureSampleLevel(src, smp, v.uv - step * 1.3846153846, 0.0) * 0.3162162162;
    c += textureSampleLevel(src, smp, v.uv + step * 3.2307692308, 0.0) * 0.0702702703;
    c += textureSampleLevel(src, smp, v.uv - step * 3.2307692308, 0.0) * 0.0702702703;
    return c;
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
    return textureSampleLevel(blurred, smp, clamp(p / u.view.xy, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0).rgb;
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
    // 0 at the rim, 1 past the bevel: the lens is all in the bevel, strongest at the very edge.
    let inside = clamp(-d / u.shape.y, 0.0, 1.0);
    let bend = (1.0 - inside) * (1.0 - inside);
    let off = n * u.shape.z * bend;
    // Outwards: the rim shows what lies beyond it; blue bends a little further than red.
    let col0 = vec3<f32>(look(p + off * (1.0 - u.shape.w)).r, look(p + off).g, look(p + off * (1.0 + u.shape.w)).b);
    // Glass over a dark page lifts it a little; its own colour on top.
    var col = col0 * (1.0 - u.light.y) + vec3<f32>(u.light.y);
    col = mix(col, u.tint.rgb, u.tint.a);
    // What lies just past the nearest edge washes in, strongest at the edge and fading inwards: the pane
    // takes the colour of what is beside it, not only at its rim.
    let depth = max(-d, 0.0);
    let beyond = look(p + n * (depth + 8.0 * u.shape.y / max(u.shape.y, 1.0) + 6.0));
    let spill = u.light.z * exp(-depth / max(u.light.w, 1.0));
    col = mix(col, beyond * 1.08 + vec3<f32>(0.015), clamp(spill, 0.0, 1.0));
    // The rim lit from above, brighter where what it carries is bright.
    let rim = pow(1.0 - inside, 3.0);
    let lit = 0.5 + 0.5 * dot(n, normalize(vec2<f32>(-0.35, -1.0)));
    let seen = dot(col0, vec3<f32>(0.2126, 0.7152, 0.0722));
    col += vec3<f32>(rim * lit * u.light.x * (0.4 + 0.6 * seen));
    return vec4<f32>(col * cover, cover);
}
