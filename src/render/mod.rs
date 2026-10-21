//! Compositing glyph cells into output frames.

mod cpu;
#[cfg(feature = "gpu")]
mod gpu;

pub use cpu::CpuRenderer;
#[cfg(feature = "gpu")]
pub use gpu::GpuRenderer;

use crate::glitch::Tear;
use crate::grid::Cell;
use anyhow::Result;

/// Bytes per output pixel. Frames are RGBA8 so the GPU can write one `u32` per pixel.
pub const BPP: usize = 4;

#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub cols: usize,
    pub rows: usize,
    /// Glyph cell size in pixels.
    pub cw: usize,
    pub ch: usize,
    /// Output frame size; at least `cols*cw` x `rows*ch`, padded to even for yuv420p.
    pub out_w: usize,
    pub out_h: usize,
}

impl Layout {
    pub fn new(cols: usize, rows: usize, cw: usize, ch: usize) -> Layout {
        Layout { cols, rows, cw, ch, out_w: (cols * cw + 1) & !1, out_h: (rows * ch + 1) & !1 }
    }

    pub fn frame_bytes(&self) -> usize {
        self.out_w * self.out_h * BPP
    }
}

pub trait Renderer {
    /// Draws `cells` into `out` (RGBA8, `layout.frame_bytes()` long), then applies `tears`.
    fn render(&mut self, cells: &[Cell], tears: &[Tear], out: &mut [u8]) -> Result<()>;
}

#[cfg(all(test, feature = "gpu"))]
mod tests {
    use super::*;
    use crate::atlas::{Atlas, EMPTY};

    /// The GPU path should match the CPU one to within sRGB rounding.
    #[test]
    fn gpu_matches_cpu() {
        let Some(font) = crate::atlas::tests::font() else { return };
        let atlas = Atlas::build(&font, 10.0, " .:+#", true, false).unwrap();
        let layout = Layout::new(23, 9, atlas.cw, atlas.ch);
        let bg = [10.0, 20.0, 40.0];
        let Ok(mut gpu) = GpuRenderer::new(&atlas, layout, bg) else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let mut cpu = CpuRenderer::new(atlas, layout, bg);
        let mut rng = fastrand::Rng::with_seed(3);
        let cells: Vec<Cell> = (0..layout.cols * layout.rows)
            .map(|_| Cell {
                glyph: if rng.u8(..8) == 0 { EMPTY } else { rng.u8(..5) },
                color: [rng.f32() * 255.0, rng.f32() * 255.0, rng.f32() * 255.0],
                fill: if rng.bool() { rng.f32() } else { 0.0 },
            })
            .collect();
        let tears = [Tear { y0: 3, height: 20, shift: 17 }, Tear { y0: 15, height: 30, shift: -40 }];
        let (mut a, mut b) = (vec![0; layout.frame_bytes()], vec![0; layout.frame_bytes()]);
        cpu.render(&cells, &tears, &mut a).unwrap();
        gpu.render(&cells, &tears, &mut b).unwrap();
        let worst = a.iter().zip(&b).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
        assert!(worst <= 2, "max channel difference {worst}");
    }
}
