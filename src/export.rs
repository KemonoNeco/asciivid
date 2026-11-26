//! `--export-atlas`: the glyph atlas the hypr-shaders ASCII effect samples.
//!
//! The PNG is one row of `SLOTS` cells, each `cw` x `ch`: slot `s` holds the glyph for
//! luminance `s / (SLOTS - 1)`, white with coverage in alpha. A `.json` sidecar next to it
//! gives `{cw, ch, slots}`.

use anyhow::{Context, Result};
use asciivid::atlas::{self, Atlas};
use asciivid::params::Params;
use std::path::Path;

const SLOTS: usize = 256;

pub fn export_atlas(p: &Params, png: &Path) -> Result<()> {
    let font_path = match &p.font {
        Some(f) => f.clone(),
        None => atlas::default_font()?,
    };
    let font = std::fs::read(&font_path).with_context(|| format!("reading font {}", font_path.display()))?;
    let atlas = Atlas::build(&font, p.font_size, p.ramp(), p.sort, p.bold)?;
    let lut = atlas.lut(p.sort);
    let (cw, ch) = (atlas.cw, atlas.ch);
    let w = SLOTS * cw;

    let mut rgba = vec![0u8; w * ch * 4];
    for (s, &g) in lut.iter().enumerate() {
        let glyph = &atlas.glyphs[g as usize];
        for y in 0..ch {
            for x in 0..cw {
                let i = (y * w + s * cw + x) * 4;
                rgba[i..i + 4].copy_from_slice(&[255, 255, 255, glyph[y * cw + x]]);
            }
        }
    }

    if let Some(dir) = png.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    // Sidecar first, PNG last: readers treat the PNG's appearance as "atlas ready".
    let json = png.with_extension("json");
    write_atomic(&json, serde_json::json!({ "cw": cw, "ch": ch, "slots": SLOTS }).to_string().as_bytes())?;
    write_atomic(png, &encode_png(w as u32, ch as u32, &rgba))?;
    Ok(())
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

/// Minimal RGBA8 PNG with stored (uncompressed) deflate blocks; atlases are small.
fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(rgba.len() + h as usize);
    for row in rgba.chunks_exact(w as usize * 4) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(0xffff).peekable();
    while let Some(b) = blocks.next() {
        z.push(blocks.peek().is_none() as u8);
        let n = b.len() as u16;
        z.extend_from_slice(&n.to_le_bytes());
        z.extend_from_slice(&(!n).to_le_bytes());
        z.extend_from_slice(b);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, data) in [(b"IHDR", &ihdr[..]), (b"IDAT", &z[..]), (b"IEND", &[][..])] {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}
