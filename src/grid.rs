//! Turning a downscaled source frame into a grid of glyph cells.

use crate::noise::{self, Stream};
use crate::params::Params;
use rayon::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    /// Index into the atlas, or `atlas::EMPTY`.
    pub glyph: u8,
    /// Glyph colour, sRGB 0..255.
    pub color: [f32; 3],
    /// How far the cell background is tinted toward `color`.
    pub fill: f32,
}

/// Per-frame tone settings resolved from `Params`.
pub struct Mapper {
    pub lut: [u8; 256],
    pub fg: [f32; 3],
    pub color: bool,
    pub shade: f32,
    pub saturation: f32,
    pub bg_fill: f32,
    pub contrast: f32,
    pub brightness: f32,
    pub gamma: f32,
    pub jitter: f32,
    pub seed: u64,
}

impl Mapper {
    pub fn new(p: &Params, lut: [u8; 256], fg: [f32; 3]) -> Mapper {
        Mapper {
            lut,
            fg,
            color: p.color,
            shade: p.shade,
            saturation: p.saturation,
            bg_fill: p.bg_fill(),
            contrast: p.contrast,
            brightness: p.brightness,
            gamma: p.gamma,
            jitter: p.jitter,
            seed: p.seed,
        }
    }

    /// Fills `cells` from the RGB24 `src` and its luminance `lum`. `frame` is the absolute
    /// frame index, which seeds the jitter.
    pub fn map(
        &self,
        cells: &mut [Cell],
        src: &[u8],
        lum: &[f32],
        curve: Option<&[f32; 256]>,
        invert: bool,
        frame: u64,
    ) {
        cells.par_iter_mut().enumerate().for_each(|(i, cell)| {
            let px = &src[i * 3..i * 3 + 3];
            let mut l = match curve {
                Some(c) => c[(lum[i] * 255.0) as usize],
                None => lum[i],
            };
            l = ((l - 0.5) * self.contrast + 0.5 + self.brightness).clamp(0.0, 1.0);
            l = l.powf(self.gamma).clamp(0.0, 1.0);
            if invert {
                l = 1.0 - l;
            }
            if self.jitter > 0.0 {
                let n = noise::unit(self.seed, Stream::Jitter, frame, i as u64);
                l = (l + (n - 0.5) * self.jitter).clamp(0.0, 1.0);
            }
            cell.glyph = self.lut[(l * 255.0).round() as usize];
            let base = if self.color {
                let [r, g, b] = [px[0] as f32, px[1] as f32, px[2] as f32];
                // Normalise so dark source colours still read through the glyph shading.
                let m = r.max(g).max(b).max(1.0);
                let c = [r / m * 255.0, g / m * 255.0, b / m * 255.0];
                let grey = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
                c.map(|v| (grey + (v - grey) * self.saturation).clamp(0.0, 255.0))
            } else {
                self.fg
            };
            let k = 1.0 - self.shade + self.shade * l;
            cell.color = base.map(|v| v * k);
            cell.fill = self.bg_fill * l;
        });
    }
}
