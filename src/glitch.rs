//! DedSec-style glitches, derived purely from (seed, frame) so any frame can be rendered on
//! its own and still match a full render.

use crate::atlas::{Atlas, EMPTY};
use crate::grid::Cell;
use crate::noise::{self, Stream};

/// A horizontal band of output pixels rotated sideways. Positive `shift` moves content left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tear {
    pub y0: usize,
    pub height: usize,
    pub shift: isize,
}

pub struct Glitcher {
    pub intensity: f32,
    pub seed: u64,
    /// Glyphs a scramble may pick (ones that draw something).
    inked: Vec<u8>,
    densest: u8,
}

/// Longest burst, in frames. Bursts last 3..=MAX_BURST frames.
const MAX_BURST: u64 = 12;

/// Random rectangle of cells, sized relative to the grid.
fn rand_rect(rng: &mut fastrand::Rng, cols: usize, rows: usize) -> (usize, usize, usize, usize) {
    let w = rng.usize(cols / 12 + 1..=cols / 2 + 1).min(cols);
    let h = rng.usize(1..=rows / 6 + 1).min(rows);
    (rng.usize(0..=cols - w), rng.usize(0..=rows - h), w, h)
}

impl Glitcher {
    pub fn new(intensity: f32, seed: u64, atlas: &Atlas) -> Glitcher {
        Glitcher { intensity, seed, inked: atlas.inked(), densest: atlas.densest() }
    }

    pub fn active(&self) -> bool {
        self.intensity > 0.0
    }

    /// Whether `frame` falls inside a burst. Any frame may start one, so look back over the
    /// frames whose burst could still be running.
    pub fn burst(&self, frame: u64) -> bool {
        (0..MAX_BURST.min(frame + 1)).any(|d| {
            let mut r = noise::rng(self.seed, Stream::Burst, frame - d);
            r.f32() < self.intensity * 0.03 && r.u64(3..=MAX_BURST) > d
        })
    }

    /// Scale event probability up during a burst.
    fn p(&self, base: f32, burst: bool) -> f32 {
        let boost = if burst { 4.0 } else { 1.0 };
        (base * self.intensity * boost).min(1.0)
    }

    /// Cell-level glitches: scrambles, dropouts and vertical rolls.
    pub fn cells(&self, frame: u64, cells: &mut [Cell], cols: usize, rows: usize, fg: [f32; 3]) {
        let burst = self.burst(frame);
        let mut rng = noise::rng(self.seed, Stream::Cells, frame);

        // Scramble: random glyphs inside a block.
        if rng.f32() < self.p(0.3, burst) {
            let (x, y, w, h) = rand_rect(&mut rng, cols, rows);
            for r in y..y + h {
                for c in x..x + w {
                    let cell = &mut cells[r * cols + c];
                    cell.glyph = self.inked[rng.usize(..self.inked.len())];
                    if cell.color == [0.0; 3] {
                        cell.color = fg;
                    }
                }
            }
        }
        // Dropout: blank a block, or blow it out to the densest glyph.
        if rng.f32() < self.p(0.4, burst) {
            let (x, y, w, h) = rand_rect(&mut rng, cols, rows);
            let solid = rng.f32() < 0.3;
            for r in y..y + h {
                for c in x..x + w {
                    let cell = &mut cells[r * cols + c];
                    if solid {
                        cell.glyph = self.densest;
                        cell.color = fg;
                    } else {
                        cell.glyph = EMPTY;
                        cell.fill = 0.0;
                    }
                }
            }
        }
        // Row roll: whole grid jumps vertically.
        if rng.f32() < self.p(0.05, burst) {
            let k = rng.usize(1..=rows / 8 + 1) * cols;
            cells.rotate_right(k.min(cells.len()));
        }
    }

    /// Pixel-level tears for a `w`x`h` output with `ch`-pixel rows of glyphs.
    pub fn tears(&self, frame: u64, w: usize, h: usize, ch: usize) -> Vec<Tear> {
        let burst = self.burst(frame);
        let mut rng = noise::rng(self.seed, Stream::Tears, frame);
        if rng.f32() >= self.p(0.6, burst) {
            return Vec::new();
        }
        let bands = 1 + rng.usize(0..=(self.intensity * 6.0) as usize);
        let max_shift = ((w as f32 * 0.25 * self.intensity) as usize).max(4).min(w);
        (0..bands)
            .map(|_| {
                let height = rng.usize(2..=ch * 6).min(h);
                let y0 = rng.usize(0..=h - height);
                let shift = rng.usize(1..=max_shift) as isize;
                Tear { y0, height, shift: if rng.bool() { shift } else { -shift } }
            })
            .collect()
    }
}

/// Total horizontal shift for output row `y`, in pixels, wrapped to `0..w`.
pub fn row_shift(tears: &[Tear], y: usize, w: usize) -> usize {
    let s: isize = tears.iter().filter(|t| (t.y0..t.y0 + t.height).contains(&y)).map(|t| t.shift).sum();
    s.rem_euclid(w as isize) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glitcher(intensity: f32) -> Glitcher {
        Glitcher { intensity, seed: 1337, inked: vec![1, 2], densest: 2 }
    }

    #[test]
    fn same_frame_same_glitch() {
        let g = glitcher(0.8);
        let blank = vec![Cell { glyph: 0, color: [0.0; 3], fill: 0.0 }; 40 * 20];
        for frame in [0, 7, 1000] {
            let (mut a, mut b) = (blank.clone(), blank.clone());
            g.cells(frame, &mut a, 40, 20, [255.0; 3]);
            g.cells(frame, &mut b, 40, 20, [255.0; 3]);
            assert_eq!(a, b);
            assert_eq!(g.tears(frame, 400, 300, 10), g.tears(frame, 400, 300, 10));
        }
    }

    #[test]
    fn bursts_last_several_frames() {
        let g = glitcher(1.0);
        let active: Vec<bool> = (0..5000).map(|f| g.burst(f)).collect();
        assert!(active.iter().any(|&b| b) && !active.iter().all(|&b| b));
        // Every burst run is at least 3 frames, unless it touches frame 0.
        let mut run = 0;
        for (f, &b) in active.iter().enumerate() {
            if b {
                run += 1;
            } else {
                assert!(run == 0 || run >= 3 || f == run, "short burst ending at {f}");
                run = 0;
            }
        }
        assert!(!glitcher(0.0).burst(42));
    }

    #[test]
    fn tears_stay_in_bounds() {
        let g = glitcher(1.0);
        for frame in 0..500 {
            for t in g.tears(frame, 8, 6, 11) {
                assert!(t.y0 + t.height <= 6 && t.shift != 0 && t.shift.unsigned_abs() <= 8);
            }
        }
    }

    #[test]
    fn shifts_compose_like_rotations() {
        let tears = [
            Tear { y0: 0, height: 4, shift: 3 },
            Tear { y0: 2, height: 4, shift: -5 },
        ];
        assert_eq!(row_shift(&tears, 0, 10), 3);
        assert_eq!(row_shift(&tears, 3, 10), 8);
        assert_eq!(row_shift(&tears, 5, 10), 5);
        assert_eq!(row_shift(&tears, 9, 10), 0);
    }
}
