// One invocation per output pixel. Mirrors CpuRenderer: blend glyph coverage between the
// cell background and glyph colour in linear light, then sRGB-encode.

struct Params {
    cols: u32,
    rows: u32,
    cw: u32,
    ch: u32,
    out_w: u32,
    out_h: u32,
    _pad0: u32,
    _pad1: u32,
    // sRGB 0..255; w unused.
    bg: vec4<f32>,
}

const EMPTY: u32 = 255u;

@group(0) @binding(0) var<uniform> p: Params;
// Glyph coverage bytes, packed four per word: glyph * cw * ch + y * cw + x.
@group(0) @binding(1) var<storage, read> atlas: array<u32>;
// Per cell: rgb colour (sRGB 0..255) and background fill.
@group(0) @binding(2) var<storage, read> cells: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> glyphs: array<u32>;
// Per output row: tear shift to the left, already wrapped to 0..out_w.
@group(0) @binding(4) var<storage, read> shifts: array<u32>;
@group(0) @binding(5) var<storage, read_write> out: array<u32>;

// Matches the CPU's 256-entry table, which truncates to an integer level first.
fn to_lin(c: vec3<f32>) -> vec3<f32> {
    let v = floor(clamp(c, vec3(0.0), vec3(255.0))) / 255.0;
    return select(pow((v + 0.055) / 1.055, vec3(2.4)), v / 12.92, v <= vec3(0.04045));
}

// Matches the CPU's 4096-entry table.
fn to_srgb(c: vec3<f32>) -> vec3<f32> {
    let v = floor(clamp(c, vec3(0.0), vec3(1.0)) * 4095.0) / 4095.0;
    let s = select(1.055 * pow(v, vec3(1.0 / 2.4)) - 0.055, v * 12.92, v <= vec3(0.0031308));
    return round(s * 255.0);
}

fn pack(c: vec3<f32>) -> u32 {
    let b = vec3<u32>(c);
    return b.x | (b.y << 8u) | (b.z << 16u) | (255u << 24u);
}

fn coverage(glyph: u32, x: u32, y: u32) -> f32 {
    if glyph == EMPTY {
        return 0.0;
    }
    let k = (glyph * p.ch + y) * p.cw + x;
    return f32((atlas[k / 4u] >> ((k % 4u) * 8u)) & 255u) / 255.0;
}

fn shade(x: u32, y: u32) -> u32 {
    let bg = floor(p.bg.rgb);
    let c = x / p.cw;
    let r = y / p.ch;
    if c >= p.cols || r >= p.rows {
        return pack(bg);
    }
    let i = r * p.cols + c;
    let cell = cells[i];
    let filled = cell.w > 0.0;
    let a = coverage(glyphs[i], x % p.cw, y % p.ch);
    if a == 0.0 && !filled {
        return pack(bg);
    }
    var cell_bg = to_lin(p.bg.rgb);
    if filled {
        cell_bg = to_lin(p.bg.rgb + (cell.rgb - p.bg.rgb) * cell.w);
    }
    return pack(to_srgb(cell_bg + (to_lin(cell.rgb) - cell_bg) * a));
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.out_w || id.y >= p.out_h {
        return;
    }
    let sx = (id.x + shifts[id.y]) % p.out_w;
    out[id.y * p.out_w + id.x] = shade(sx, id.y);
}
