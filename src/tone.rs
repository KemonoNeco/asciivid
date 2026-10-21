//! Colour-space helpers and the per-frame luminance tone curve.

pub fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

pub fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

/// Rec. 709 luma of an 8-bit RGB pixel, 0..1.
pub fn luma(px: &[u8]) -> f32 {
    ((0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32) / 255.0).min(1.0)
}

/// Mean luminance of the outermost ring of cells.
pub fn border_mean(lum: &[f32], cols: usize, rows: usize) -> f32 {
    let edge = |i: usize| {
        let (r, c) = (i / cols, i % cols);
        r == 0 || r == rows - 1 || c == 0 || c == cols - 1
    };
    let (sum, n) = lum
        .iter()
        .enumerate()
        .filter(|&(i, _)| edge(i))
        .fold((0.0, 0), |(s, n), (_, &l)| (s + l, n + 1));
    sum / n.max(1) as f32
}

/// Tone curve for one frame: a 1%–99% percentile stretch blended with histogram
/// equalization by `eq`.
pub fn level_curve(lum: &[f32], eq: f32) -> [f32; 256] {
    let mut hist = [0u32; 256];
    for &l in lum {
        hist[(l * 255.0) as usize] += 1;
    }
    let total = lum.len() as f32;
    let mut cdf = [0f32; 256];
    let mut acc = 0.0;
    for (c, &n) in cdf.iter_mut().zip(&hist) {
        acc += n as f32;
        *c = acc / total;
    }
    let find = |q: f32| cdf.iter().position(|&c| c >= q).unwrap_or(255) as f32 / 255.0;
    let (mut lo, mut hi) = (find(0.01), find(0.99));
    // Don't blow a near-flat frame up into noise.
    let mid = (lo + hi) / 2.0;
    lo = lo.min(mid - 0.15).max(0.0);
    hi = hi.max(mid + 0.15).min(1.0);
    let (c_lo, c_hi) = (cdf[(lo * 255.0) as usize], cdf[(hi * 255.0) as usize].max(1e-6));
    std::array::from_fn(|i| {
        let l = i as f32 / 255.0;
        let stretch = (l - lo) / (hi - lo);
        let equal = (cdf[i] - c_lo) / (c_hi - c_lo).max(1e-6);
        (stretch + (equal - stretch) * eq).clamp(0.0, 1.0)
    })
}

/// Auto-levels that ease toward each new frame's curve so levels don't pump frame to frame.
#[derive(Default)]
pub struct Levels {
    curve: Option<[f32; 256]>,
}

impl Levels {
    pub fn update(&mut self, lum: &[f32], eq: f32) -> &[f32; 256] {
        let next = level_curve(lum, eq);
        self.curve.insert(match self.curve {
            Some(c) => std::array::from_fn(|i| c[i] + (next[i] - c[i]) * 0.15),
            None => next,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_roundtrip() {
        for i in 0..=255 {
            let v = i as f32 / 255.0;
            assert!((linear_to_srgb(srgb_to_linear(v)) - v).abs() < 1e-5);
        }
    }

    #[test]
    fn white_luma_stays_in_range() {
        assert!(luma(&[255, 255, 255]) <= 1.0);
        assert_eq!((luma(&[255, 255, 255]) * 255.0) as usize, 255);
    }

    #[test]
    fn curve_is_monotonic_and_stretches() {
        let lum: Vec<f32> = (0..1000).map(|i| 0.2 + 0.4 * i as f32 / 1000.0).collect();
        for eq in [0.0, 0.5, 1.0] {
            let c = level_curve(&lum, eq);
            assert!(c.windows(2).all(|w| w[1] >= w[0]));
            assert!(c[(0.21 * 255.0) as usize] < 0.1);
            assert!(c[(0.59 * 255.0) as usize] > 0.9);
        }
    }

    #[test]
    fn flat_frame_is_not_blown_up() {
        let c = level_curve(&[0.5; 100], 0.0);
        // A ±0.15 floor around the midpoint keeps a single tone from spanning the whole ramp.
        assert!(c[(0.45 * 255.0) as usize] > 0.2);
    }
}
