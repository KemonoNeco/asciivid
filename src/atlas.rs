//! Rasterized glyph cells for a character ramp.

use crate::tone::linear_to_srgb;
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Command;

/// Glyph index meaning "draw nothing", for cells blanked by a glitch. Ramps are capped at
/// 255 characters so it never collides with a real glyph.
pub const EMPTY: u8 = u8::MAX;

pub struct Atlas {
    pub cw: usize,
    pub ch: usize,
    /// Coverage bitmaps (cw*ch). Ordered darkest to brightest unless built with `sort = false`.
    pub glyphs: Vec<Vec<u8>>,
    /// Glyphs with no ink at all, which rendering can skip.
    pub blank: Vec<bool>,
}

impl Atlas {
    pub fn build(font_bytes: &[u8], px: f32, ramp: &str, sort: bool, bold: bool) -> Result<Atlas> {
        let n = ramp.chars().count();
        if n > EMPTY as usize {
            bail!("ramp can have at most {} characters", EMPTY);
        }
        if n < 2 {
            bail!("ramp needs at least two characters");
        }
        let font = fontdue::Font::from_bytes(font_bytes, fontdue::FontSettings::default())
            .map_err(|e| anyhow::anyhow!("failed to load font: {e}"))?;
        let lm = font
            .horizontal_line_metrics(px)
            .context("font has no horizontal metrics")?;
        let cw = font.metrics('M', px).advance_width.round().max(1.0) as usize;

        let mut raster = Vec::new();
        for c in ramp.chars() {
            if !c.is_whitespace() && font.lookup_glyph_index(c) == 0 {
                bail!("font has no glyph for {c:?}; pick another --font or --ramp");
            }
            raster.push(font.rasterize(c, px));
        }
        // Size rows to the ramp's actual ink (plus a pixel of air) rather than the font's full
        // line height, so glyphs fill their cells instead of leaving blank bands between rows.
        let inked = || raster.iter().map(|(m, _)| m).filter(|m| m.height > 0);
        let line = (lm.ascent - lm.descent).ceil().max(1.0) as i32;
        let top = inked().map(|m| m.ymin + m.height as i32).max().unwrap_or(line);
        let bottom = inked().map(|m| m.ymin).min().unwrap_or(0);
        let ch = (top - bottom + 1).clamp(1, line) as usize;
        let baseline = top.min(lm.ascent.round() as i32);

        let mut glyphs = Vec::new();
        for (m, bmp) in raster {
            let mut cell = vec![0u8; cw * ch];
            let x0 = m.xmin;
            let y0 = baseline - (m.ymin + m.height as i32);
            for gy in 0..m.height {
                let y = y0 + gy as i32;
                if y < 0 || y >= ch as i32 {
                    continue;
                }
                for gx in 0..m.width {
                    let x = x0 + gx as i32;
                    if x < 0 || x >= cw as i32 {
                        continue;
                    }
                    cell[y as usize * cw + x as usize] = bmp[gy * m.width + gx];
                }
            }
            if bold {
                // Synthetic bold: smear each row one pixel right.
                for row in cell.chunks_exact_mut(cw) {
                    for x in (1..cw).rev() {
                        row[x] = row[x].max(row[x - 1]);
                    }
                }
            }
            glyphs.push(cell);
        }
        if sort {
            glyphs.sort_by_key(|g| ink(g));
        }
        let blank: Vec<bool> = glyphs.iter().map(|g| ink(g) == 0).collect();
        if blank.iter().all(|&b| b) {
            bail!("ramp has no visible characters");
        }
        Ok(Atlas { cw, ch, glyphs, blank })
    }

    pub fn glyph_count(&self) -> usize {
        self.glyphs.len()
    }

    /// Whether drawing `glyph` puts any ink down.
    pub fn draws(&self, glyph: u8) -> bool {
        glyph != EMPTY && !self.blank[glyph as usize]
    }

    /// Index of the glyph with the most ink.
    pub fn densest(&self) -> u8 {
        (0..self.glyph_count()).max_by_key(|&i| ink(&self.glyphs[i])).unwrap() as u8
    }

    /// Indices of every glyph that draws something.
    pub fn inked(&self) -> Vec<u8> {
        (0..self.glyph_count() as u8).filter(|&i| self.draws(i)).collect()
    }

    /// Maps 8-bit luminance to a glyph index. With `by_density`, each level picks the glyph
    /// whose on-screen brightness (mean coverage, sRGB-encoded) is closest; otherwise the
    /// ramp is spread evenly.
    pub fn lut(&self, by_density: bool) -> [u8; 256] {
        let n = self.glyph_count();
        let t: Vec<f32> = if by_density {
            let p: Vec<f32> = self
                .glyphs
                .iter()
                .map(|g| linear_to_srgb(ink(g) as f32 / (g.len() as f32 * 255.0)))
                .collect();
            let (lo, hi) = (p[0], p[n - 1].max(p[0] + 1e-6));
            p.iter().map(|v| (v - lo) / (hi - lo)).collect()
        } else {
            (0..n).map(|i| i as f32 / (n - 1) as f32).collect()
        };
        std::array::from_fn(|l| {
            let l = l as f32 / 255.0;
            (0..n).min_by(|&a, &b| (t[a] - l).abs().total_cmp(&(t[b] - l).abs())).unwrap() as u8
        })
    }
}

fn ink(g: &[u8]) -> u32 {
    g.iter().map(|&a| a as u32).sum()
}

/// The system's default monospace font, via fontconfig.
pub fn default_font() -> Result<PathBuf> {
    let out = Command::new("fc-match")
        .args(["-f", "%{file}", "monospace"])
        .output()
        .context("fc-match not found; pass --font")?;
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    if !path.is_file() {
        bail!("fc-match gave no usable monospace font; pass --font");
    }
    Ok(path)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The system monospace font, or None to skip font-dependent tests.
    pub fn font() -> Option<Vec<u8>> {
        std::fs::read(default_font().ok()?).ok()
    }

    #[test]
    fn ramp_without_space_has_no_blank_glyphs() {
        let Some(f) = font() else { return };
        let a = Atlas::build(&f, 10.0, "01", true, false).unwrap();
        assert!(a.blank.iter().all(|&b| !b));
        assert!(a.draws(0) && a.draws(1) && !a.draws(EMPTY));
    }

    #[test]
    fn sorted_ramp_puts_space_first_and_lut_spans_it() {
        let Some(f) = font() else { return };
        let a = Atlas::build(&f, 10.0, "#. ", true, false).unwrap();
        assert!(a.blank[0] && !a.blank[1]);
        assert_eq!(a.densest(), 2);
        let lut = a.lut(true);
        assert_eq!((lut[0], lut[255]), (0, 2));
        assert!(lut.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn unsorted_ramp_keeps_order() {
        let Some(f) = font() else { return };
        let a = Atlas::build(&f, 10.0, "# ", false, false).unwrap();
        assert!(!a.blank[0] && a.blank[1]);
        assert_eq!(a.densest(), 0);
    }

    #[test]
    fn rejects_bad_ramps() {
        let Some(f) = font() else { return };
        assert!(Atlas::build(&f, 10.0, "#", true, false).is_err());
        assert!(Atlas::build(&f, 10.0, "   ", true, false).is_err());
        let long: String = std::iter::repeat_n('#', 256).collect();
        assert!(Atlas::build(&f, 10.0, &long, true, false).is_err());
    }
}
