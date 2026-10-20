use super::{BPP, Layout, Renderer};
use crate::atlas::Atlas;
use crate::glitch::{Tear, row_shift};
use crate::grid::Cell;
use crate::tone::{linear_to_srgb, srgb_to_linear};
use anyhow::Result;
use rayon::prelude::*;

pub struct CpuRenderer {
    atlas: Atlas,
    layout: Layout,
    bg: [f32; 3],
    to_lin: [f32; 256],
    to_srgb: Vec<u8>,
}

impl CpuRenderer {
    pub fn new(atlas: Atlas, layout: Layout, bg: [f32; 3]) -> CpuRenderer {
        CpuRenderer {
            atlas,
            layout,
            bg,
            to_lin: std::array::from_fn(|v| srgb_to_linear(v as f32 / 255.0)),
            to_srgb: (0..4096).map(|i| (linear_to_srgb(i as f32 / 4095.0) * 255.0).round() as u8).collect(),
        }
    }

    fn lin(&self, c: [f32; 3]) -> [f32; 3] {
        c.map(|v| self.to_lin[v.clamp(0.0, 255.0) as usize])
    }
}

impl Renderer for CpuRenderer {
    fn render(&mut self, cells: &[Cell], tears: &[Tear], out: &mut [u8]) -> Result<()> {
        let Layout { cols, rows, cw, ch, out_w, .. } = self.layout;
        let bg = self.bg;
        let bg_px = [bg[0] as u8, bg[1] as u8, bg[2] as u8, 255];
        let bg_lin = self.lin(bg);
        let this = &*self;
        out.par_chunks_mut(out_w * BPP * ch).enumerate().for_each(|(r, band)| {
            for px in band.as_chunks_mut::<BPP>().0 {
                px.copy_from_slice(&bg_px);
            }
            if r >= rows {
                return;
            }
            let enc = |v: f32| this.to_srgb[(v.clamp(0.0, 1.0) * 4095.0) as usize];
            for c in 0..cols {
                let cell = cells[r * cols + c];
                let filled = cell.fill > 0.0;
                let glyph = this.atlas.draws(cell.glyph).then(|| &this.atlas.glyphs[cell.glyph as usize]);
                if glyph.is_none() && !filled {
                    continue;
                }
                // Blend in linear light so antialiased strokes keep their brightness.
                let fg_lin = this.lin(cell.color);
                let cell_bg = if filled {
                    this.lin(std::array::from_fn(|k| bg[k] + (cell.color[k] - bg[k]) * cell.fill))
                } else {
                    bg_lin
                };
                for gy in 0..ch {
                    let row = gy * out_w * BPP;
                    if row >= band.len() {
                        break;
                    }
                    for gx in 0..cw {
                        let a = glyph.map_or(0, |g| g[gy * cw + gx]);
                        if a == 0 && !filled {
                            continue;
                        }
                        let a = a as f32 / 255.0;
                        let i = row + (c * cw + gx) * BPP;
                        for k in 0..3 {
                            band[i + k] = enc(cell_bg[k] + (fg_lin[k] - cell_bg[k]) * a);
                        }
                    }
                }
            }
        });

        if !tears.is_empty() {
            out.par_chunks_mut(out_w * BPP).enumerate().for_each(|(y, row)| {
                let s = row_shift(tears, y, out_w);
                if s > 0 {
                    row.rotate_left(s * BPP);
                }
            });
        }
        Ok(())
    }
}
