use anyhow::Result;
use asciivid::ffmpeg::Seek;
use asciivid::params::{Charset, Invert, PRESETS, Params};
use asciivid::pipeline::{self, Job};
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Render any video as ASCII art into a new video file (or a live preview).
///
/// Look options start from the defaults, then a --preset, then any flags given here.
#[derive(Parser)]
#[command(version, after_help = presets_help())]
struct Args {
    /// Input video, GIF or image (anything ffmpeg can read)
    #[arg(required_unless_present = "print_preset")]
    input: Option<String>,
    /// Output file; the extension picks the format: mp4/mov/mkv (H.264), webm (VP9),
    /// gif, png/jpg (first frame). Omit with --preview
    output: Option<String>,

    /// Built-in preset name or path to a TOML file
    #[arg(long)]
    preset: Option<String>,
    /// Print the effective look settings as TOML (save it as your own preset) and exit
    #[arg(long)]
    print_preset: bool,

    /// Target output width in pixels; sets the column count from the font's cell width [default: 1920]
    #[arg(long)]
    width: Option<u32>,
    /// Number of character columns (overrides --width)
    #[arg(long)]
    cols: Option<u32>,
    /// Monospace font file [default: fc-match monospace]
    #[arg(long)]
    font: Option<PathBuf>,
    /// Font size in pixels [default: 10]
    #[arg(long)]
    font_size: Option<f32>,
    /// Thicken glyph strokes by a pixel for brighter, punchier output
    #[arg(long)]
    bold: bool,

    /// Built-in character ramp [default: wd2]
    #[arg(long, value_enum)]
    charset: Option<Charset>,
    /// Custom character ramp (overrides --charset)
    #[arg(long)]
    ramp: Option<String>,
    /// Keep the ramp order as given instead of sorting glyphs by ink density
    #[arg(long)]
    no_sort: bool,

    /// Glyph colour (hex) [default: #ffffff]
    #[arg(long)]
    fg: Option<String>,
    /// Background colour (hex) [default: #000000]
    #[arg(long)]
    bg: Option<String>,
    /// Colour each glyph with the source pixel colour instead of --fg
    #[arg(long)]
    color: bool,
    /// How much glyph brightness follows the source luminance, 0..1 (0 = flat) [default: 0.3]
    #[arg(long)]
    shade: Option<f32>,
    /// Colour saturation multiplier for --color [default: 1.5]
    #[arg(long)]
    saturation: Option<f32>,
    /// Tint each cell's background with its glyph colour, 0..1 [default: 0.3 with --color, else 0]
    #[arg(long)]
    bg_fill: Option<f32>,

    /// Invert luminance. `auto` inverts light-background stills and GIFs in mono mode so the
    /// background stays dark [default: auto]
    #[arg(long, value_enum)]
    invert: Option<Invert>,
    /// Don't stretch each frame's luminance range to use the full ramp
    #[arg(long)]
    no_auto_levels: bool,
    /// Blend of histogram equalization into auto-levels, 0..1: spreads glyphs across the tones
    /// actually present (0 = plain stretch) [default: 0.5]
    #[arg(long)]
    equalize: Option<f32>,
    /// Sharpen the downscaled image so edges survive as glyphs, 0..5 [default: 0]
    #[arg(long)]
    sharpen: Option<f32>,
    /// Luminance contrast around mid-grey [default: 1]
    #[arg(long)]
    contrast: Option<f32>,
    /// Luminance offset, -1..1 [default: 0]
    #[arg(long, allow_hyphen_values = true)]
    brightness: Option<f32>,
    /// Gamma applied to luminance; <1 lifts shadows, >1 crushes them [default: 1]
    #[arg(long)]
    gamma: Option<f32>,
    /// Per-cell luminance noise each frame, makes the characters shimmer, 0..1 [default: 0]
    #[arg(long)]
    jitter: Option<f32>,

    /// DedSec-style glitch intensity: tearing, dropouts, scrambles, bursts, 0..1 [default: 0]
    #[arg(long)]
    glitch: Option<f32>,
    /// Seed for jitter and glitches [default: 1337]
    #[arg(long)]
    seed: Option<u64>,

    /// Start time, e.g. 48, 0:48 or 1:02:03.5
    #[arg(long)]
    start: Option<String>,
    /// Duration, in the same format as --start
    #[arg(long)]
    duration: Option<String>,
    /// Encoder quality, lower is better: x264 0..51, VP9 0..63
    #[arg(long, default_value_t = 18, value_parser = clap::value_parser!(u8).range(0..=63))]
    crf: u8,
    /// Play in an ffplay window instead of writing a file
    #[arg(long)]
    preview: bool,
    /// In --preview, play GIFs and stills once instead of looping them
    #[arg(long)]
    no_loop: bool,
    /// Composite frames on the GPU (wgpu) instead of the CPU
    #[arg(long)]
    gpu: bool,
}

fn presets_help() -> String {
    let names: Vec<_> = PRESETS.iter().map(|(n, _)| *n).collect();
    format!("Presets: {}", names.join(", "))
}

impl Args {
    /// The look: defaults, then the preset, then flags.
    fn params(&self) -> Result<Params> {
        let mut p = match &self.preset {
            Some(name) => Params::preset(name)?,
            None => Params::default(),
        };
        macro_rules! set {
            ($($field:ident),*) => {$(
                if let Some(v) = &self.$field {
                    p.$field = v.clone().into();
                }
            )*};
        }
        set!(width, font_size, charset, fg, bg, shade, saturation, invert, equalize, sharpen);
        set!(contrast, brightness, gamma, jitter, glitch, seed, cols, font, ramp, bg_fill);
        p.bold |= self.bold;
        p.color |= self.color;
        p.sort &= !self.no_sort;
        p.auto_levels &= !self.no_auto_levels;
        Ok(p)
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let params = args.params()?;
    if args.print_preset {
        params.validate()?;
        print!("{}", params.to_toml());
        return Ok(());
    }

    // First Ctrl-C finishes cleanly (the output file stays playable); a second one bails.
    let stop = Arc::new(AtomicBool::new(false));
    let handler_stop = stop.clone();
    ctrlc::set_handler(move || {
        if handler_stop.swap(true, Ordering::Relaxed) {
            eprintln!("\ninterrupted again; exiting now");
            std::process::exit(130);
        }
    })?;

    let job = Job {
        input: args.input.clone().expect("clap requires input"),
        output: args.output.clone(),
        seek: Seek { start: args.start.clone(), duration: args.duration.clone() },
        crf: args.crf,
        preview: args.preview,
        loop_preview: !args.no_loop,
        gpu: args.gpu,
    };
    let summary = pipeline::run(&params, &job, &stop)?;
    if summary.interrupted {
        std::process::exit(130);
    }
    Ok(())
}
