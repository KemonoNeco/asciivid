//! Stateless seeded randomness. Every value is a pure function of (seed, stream, frame, index),
//! so a frame comes out the same however it's reached: a full render, a `--start` slice, or
//! cells processed in any order across threads.

/// Independent random streams, so jitter and the glitch passes never share values.
#[derive(Clone, Copy)]
pub enum Stream {
    Jitter = 1,
    Burst = 2,
    Cells = 3,
    Tears = 4,
}

const GOLDEN: u64 = 0x9e37_79b9_7f4a_7c15;

/// SplitMix64 finaliser.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

pub fn hash(seed: u64, stream: Stream, frame: u64, index: u64) -> u64 {
    [stream as u64, frame, index]
        .into_iter()
        .fold(mix(seed), |h, v| mix(h ^ mix(v.wrapping_add(GOLDEN))))
}

/// Uniform float in [0, 1).
pub fn unit(seed: u64, stream: Stream, frame: u64, index: u64) -> f32 {
    (hash(seed, stream, frame, index) >> 40) as f32 / (1u64 << 24) as f32
}

/// A generator for one frame's worth of draws from `stream`.
pub fn rng(seed: u64, stream: Stream, frame: u64) -> fastrand::Rng {
    fastrand::Rng::with_seed(hash(seed, stream, frame, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_independent() {
        assert_eq!(hash(1, Stream::Jitter, 5, 9), hash(1, Stream::Jitter, 5, 9));
        assert_ne!(hash(1, Stream::Jitter, 5, 9), hash(1, Stream::Cells, 5, 9));
        assert_ne!(hash(1, Stream::Jitter, 5, 9), hash(1, Stream::Jitter, 9, 5));
        assert_ne!(hash(1, Stream::Jitter, 5, 9), hash(2, Stream::Jitter, 5, 9));
    }

    #[test]
    fn unit_is_roughly_uniform() {
        let n = 100_000;
        let vals: Vec<f32> = (0..n).map(|i| unit(7, Stream::Jitter, 0, i)).collect();
        assert!(vals.iter().all(|v| (0.0..1.0).contains(v)));
        let mean = vals.iter().sum::<f32>() / n as f32;
        assert!((mean - 0.5).abs() < 0.01, "mean {mean}");
    }
}
