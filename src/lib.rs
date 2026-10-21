//! Render videos as ASCII art.
//!
//! ffmpeg decodes each frame scaled down to one pixel per character cell ([`ffmpeg`]). Each
//! cell's luminance goes through a tone curve ([`tone`]) and picks a glyph from a ramp
//! rasterized from a real font ([`atlas`], [`grid`]). Glitch passes ([`glitch`]) then mangle
//! the grid, a renderer ([`render`], CPU or wgpu) composites the glyphs, and the frames are
//! piped back into ffmpeg. [`pipeline::run`] ties it together.
//!
//! All randomness is a pure function of the seed and frame number ([`noise`]), so any slice
//! of a video renders exactly as it would in a full pass.

pub mod atlas;
pub mod ffmpeg;
pub mod glitch;
pub mod grid;
pub mod noise;
pub mod params;
pub mod pipeline;
pub mod probe;
pub mod render;
pub mod tone;
