// RGBA (or BGRA) to tightly packed NV12: a W*H luma plane, then W*H/2 bytes of interleaved
// Cb, Cr. BT.709 matrix, TV range, on the sRGB-encoded values. Chroma is left-sited, the H.264
// default, so the stream needs no chroma-location tag. Each invocation writes one u32.
// Standalone WGSL: no imports or shader defs, so it builds unchanged without naga_oil.

struct Params {
    // Top-left of the crop in the source texture.
    origin: vec2<u32>,
    // Encoded size; both even.
    size: vec2<u32>,
    // Workgroups along x in this dispatch, to flatten the 2D grid.
    groups_x: u32,
    // 1 when the source view is sRGB: textureLoad decoded it, so encode it back.
    reencode: u32,
    // Word index where the CbCr plane starts: W * H / 4.
    chroma_word: u32,
    _pad: u32,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn encode_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        return c * 12.92;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

// The sRGB-encoded R'G'B' of crop pixel `p`, in 0..1.
fn pixel(p: vec2<u32>) -> vec3<f32> {
    let c = textureLoad(source, vec2<i32>(params.origin + p), 0).rgb;
    if params.reencode == 1u {
        return vec3<f32>(encode_srgb(c.r), encode_srgb(c.g), encode_srgb(c.b));
    }
    return c;
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn to_byte(v: f32) -> u32 {
    return u32(clamp(round(v), 0.0, 255.0));
}

fn word_index(group: vec3<u32>, local: vec3<u32>) -> u32 {
    return (group.y * params.groups_x + group.x) * 256u + local.x;
}

@compute @workgroup_size(256)
fn luma_main(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_id) local: vec3<u32>) {
    let word = word_index(group, local);
    let width = params.size.x;
    let total = width * params.size.y;
    if word * 4u >= total {
        return;
    }
    var packed = 0u;
    for (var k = 0u; k < 4u; k++) {
        let i = word * 4u + k;
        let p = vec2<u32>(i % width, i / width);
        packed |= to_byte(16.0 + 219.0 * luma(pixel(p))) << (8u * k);
    }
    output[word] = packed;
}

// Left-sited chroma for sample (cx, cy): [1, 2, 1] / 4 across luma columns 2cx-1, 2cx, 2cx+1
// (clamped at the left edge), averaged over rows 2cy and 2cy+1. Returns (Cb, Cr) bytes.
fn chroma(c: vec2<u32>) -> vec2<u32> {
    let x = 2u * c.x;
    let left = select(x - 1u, 0u, x == 0u);
    var sum = vec3<f32>(0.0);
    for (var dy = 0u; dy < 2u; dy++) {
        let y = 2u * c.y + dy;
        sum += pixel(vec2<u32>(left, y)) + 2.0 * pixel(vec2<u32>(x, y)) + pixel(vec2<u32>(x + 1u, y));
    }
    let rgb = sum / 8.0;
    let y = luma(rgb);
    let cb = 128.0 + 224.0 * (rgb.b - y) / 1.8556;
    let cr = 128.0 + 224.0 * (rgb.r - y) / 1.5748;
    return vec2<u32>(to_byte(cb), to_byte(cr));
}

@compute @workgroup_size(256)
fn chroma_main(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_id) local: vec3<u32>) {
    let word = word_index(group, local);
    let half_width = params.size.x / 2u;
    let samples = half_width * (params.size.y / 2u);
    // Each word holds samples 2w and 2w + 1, each with its own coordinates, so a word may span
    // two chroma rows. The last word may be half padding.
    if 2u * word >= samples {
        return;
    }
    var packed = 0u;
    for (var j = 0u; j < 2u; j++) {
        let s = 2u * word + j;
        if s < samples {
            let c = chroma(vec2<u32>(s % half_width, s / half_width));
            packed |= (c.x << (16u * j)) | (c.y << (16u * j + 8u));
        }
    }
    output[params.chroma_word + word] = packed;
}
