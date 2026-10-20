use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use rayon::prelude::*;
use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

/// Render any video as ASCII art into a new video file (or a live preview).
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Input video (anything ffmpeg can read)
    input: String,
    /// Output file, e.g. out.mp4 (omit with --preview)
    output: Option<String>,

    /// Target output width in pixels; sets the column count from the font's cell width
    #[arg(long, default_value_t = 1920)]
    width: u32,
    /// Number of character columns (overrides --width)
    #[arg(long)]
    cols: Option<u32>,
    /// Monospace font file (default: fc-match monospace)
    #[arg(long)]
    font: Option<PathBuf>,
    /// Font size in pixels
    #[arg(long, default_value_t = 10.0)]
    font_size: f32,
    /// Thicken glyph strokes by a pixel for brighter, punchier output
    #[arg(long)]
    bold: bool,

    /// Built-in character ramp
    #[arg(long, value_enum, default_value_t = Charset::Wd2)]
    charset: Charset,
    /// Custom character ramp (overrides --charset)
    #[arg(long)]
    ramp: Option<String>,
    /// Keep the ramp order as given instead of sorting glyphs by ink density
    #[arg(long)]
    no_sort: bool,

    /// Glyph colour (hex)
    #[arg(long, default_value = "#ffffff")]
    fg: String,
    /// Background colour (hex)
    #[arg(long, default_value = "#000000")]
    bg: String,
    /// Colour each glyph with the source pixel colour instead of --fg
    #[arg(long)]
    color: bool,
    /// How much glyph brightness follows the source luminance (0 = flat, 1 = full)
    #[arg(long, default_value_t = 0.3)]
    shade: f32,
    /// Colour saturation multiplier for --color
    #[arg(long, default_value_t = 1.5)]
    saturation: f32,
    /// Tint each cell's background with its glyph colour (0 = none, 1 = solid)
    #[arg(long, default_value_t = 0.0)]
    bg_fill: f32,

    /// Invert luminance
    #[arg(long)]
    invert: bool,
    /// Don't stretch each frame's luminance range to use the full ramp
    #[arg(long)]
    no_auto_levels: bool,
    /// Blend of histogram equalization into auto-levels; spreads glyphs across the tones
    /// actually present (0 = plain stretch, 1 = full equalization)
    #[arg(long, default_value_t = 0.5)]
    equalize: f32,
    /// Sharpen the downscaled image so edges survive as glyphs (0 = off)
    #[arg(long, default_value_t = 0.0)]
    sharpen: f32,
    #[arg(long, default_value_t = 1.0)]
    contrast: f32,
    #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
    brightness: f32,
    /// Gamma applied to luminance (<1 lifts shadows, >1 crushes them)
    #[arg(long, default_value_t = 1.0)]
    gamma: f32,
    /// Per-cell luminance noise each frame, makes the characters shimmer (0..1)
    #[arg(long, default_value_t = 0.0)]
    jitter: f32,

    /// DedSec-style glitch intensity: tearing, dropouts, scrambles, bursts (0..1)
    #[arg(long, default_value_t = 0.0)]
    glitch: f32,
    /// RNG seed for jitter/glitch
    #[arg(long, default_value_t = 1337)]
    seed: u64,

    /// Start time passed to ffmpeg (-ss), e.g. 0:48
    #[arg(long)]
    start: Option<String>,
    /// Duration passed to ffmpeg (-t), e.g. 10
    #[arg(long)]
    duration: Option<String>,
    /// x264 quality (lower = better)
    #[arg(long, default_value_t = 18)]
    crf: u8,
    /// Play in an ffplay window instead of writing a file
    #[arg(long)]
    preview: bool,
    /// Play GIFs once in --preview instead of looping them
    #[arg(long)]
    no_loop: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Charset {
    /// Sparse terminal look: " .:;+=01#"
    Wd2,
    Classic,
    Binary,
    Blocks,
    Dense,
}

impl Charset {
    fn ramp(self) -> &'static str {
        match self {
            Charset::Wd2 => " .:;+=01#",
            Charset::Classic => " .:-=+*#%@",
            Charset::Binary => " 01",
            Charset::Blocks => " ░▒▓█",
            Charset::Dense => {
                " .'`^\",:;Il!i><~+_-?][}{1)(|\\/tfjrxnuvczXYUJCLQ0OZmwqpdbkhao*#MW&8%B@$"
            }
        }
    }
}

struct Atlas {
    cw: usize,
    ch: usize,
    /// Coverage bitmaps (cw*ch), ordered darkest to brightest.
    glyphs: Vec<Vec<u8>>,
}

impl Atlas {
    /// Maps 8-bit luminance to a glyph index. With `by_density`, each level picks the glyph
    /// whose on-screen brightness (mean coverage, sRGB-encoded) is closest; otherwise the
    /// ramp is spread evenly.
    fn lut(&self, by_density: bool) -> [u8; 256] {
        let n = self.glyphs.len();
        let t: Vec<f32> = if by_density {
            let p: Vec<f32> = self
                .glyphs
                .iter()
                .map(|g| linear_to_srgb(g.iter().map(|&a| a as f32).sum::<f32>() / (g.len() as f32 * 255.0)))
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

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

/// Tone curve for one frame: a 1%–99% percentile stretch blended with histogram
/// equalization by `eq`.
fn level_curve(lum: &[f32], eq: f32) -> [f32; 256] {
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

fn build_atlas(font_bytes: &[u8], px: f32, ramp: &str, sort: bool, bold: bool) -> Result<Atlas> {
    let font = fontdue::Font::from_bytes(font_bytes, fontdue::FontSettings::default())
        .map_err(|e| anyhow::anyhow!("failed to load font: {e}"))?;
    let lm = font
        .horizontal_line_metrics(px)
        .context("font has no horizontal metrics")?;
    let cw = font.metrics('M', px).advance_width.round().max(1.0) as usize;

    let mut raster = Vec::new();
    for c in ramp.chars() {
        if c != ' ' && font.lookup_glyph_index(c) == 0 {
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
    if glyphs.len() > 255 {
        bail!("ramp can have at most 255 characters");
    }
    if glyphs.len() < 2 {
        bail!("ramp needs at least two characters");
    }
    if sort {
        glyphs.sort_by_key(|g| g.iter().map(|&a| a as u32).sum::<u32>());
    }
    Ok(Atlas { cw, ch, glyphs })
}

fn parse_hex(s: &str) -> Result<[f32; 3]> {
    let h = s.trim_start_matches('#');
    if h.len() != 6 {
        bail!("colour must be #rrggbb, got {s:?}");
    }
    let v = u32::from_str_radix(h, 16).with_context(|| format!("bad colour {s:?}"))?;
    Ok([(v >> 16) as f32, ((v >> 8) & 0xff) as f32, (v & 0xff) as f32])
}

fn default_font() -> Result<PathBuf> {
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

struct Probe {
    width: u32,
    height: u32,
    fps: String,
    fps_f: f64,
    duration: Option<f64>,
    is_gif: bool,
}

fn probe(input: &str) -> Result<Probe> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=width,height,r_frame_rate:format=duration,format_name"])
        .args(["-of", "default=nw=1", input])
        .output()
        .context("failed to run ffprobe")?;
    if !out.status.success() {
        bail!("ffprobe failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let (mut width, mut height, mut fps, mut duration) = (0, 0, String::new(), None);
    let mut is_gif = false;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        match line.split_once('=') {
            Some(("width", v)) => width = v.parse()?,
            Some(("height", v)) => height = v.parse()?,
            Some(("r_frame_rate", v)) => fps = v.to_string(),
            Some(("duration", v)) => duration = v.parse().ok(),
            Some(("format_name", v)) => is_gif = v == "gif",
            _ => {}
        }
    }
    if width == 0 || height == 0 {
        bail!("no video stream in {input}");
    }
    let fps_f = match fps.split_once('/') {
        Some((n, d)) => n.parse::<f64>()? / d.parse::<f64>()?,
        None => fps.parse()?,
    };
    Ok(Probe { width, height, fps, fps_f, duration, is_gif })
}

/// Exact frame count by counting packets; cheap for GIFs, which have one frame per packet.
fn count_frames(input: &str) -> Option<u64> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-count_packets"])
        .args(["-show_entries", "stream=nb_read_packets", "-of", "csv=p=0", input])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok().filter(|&n| n > 0)
}

fn seek_args(args: &Args) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(s) = &args.start {
        v.extend(["-ss".into(), s.clone()]);
    }
    if let Some(t) = &args.duration {
        v.extend(["-t".into(), t.clone()]);
    }
    v
}

fn decode_filter(args: &Args, cols: usize, rows: usize) -> String {
    let mut vf = format!("scale={cols}:{rows}:flags=area");
    if args.sharpen > 0.0 {
        vf += &format!(",unsharp=3:3:{0}:3:3:{0}", args.sharpen.min(5.0));
    }
    vf + ",format=rgb24"
}

fn spawn_decoder(args: &Args, cols: usize, rows: usize, looping: bool) -> Result<Child> {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error"])
        .args(if looping { &["-stream_loop", "-1"][..] } else { &[] })
        .args(seek_args(args))
        .args(["-i", &args.input, "-an"])
        .args(["-vf", &decode_filter(args, cols, rows)])
        .args(["-f", "rawvideo", "-"])
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to start ffmpeg decoder")
}

fn spawn_sink(args: &Args, w: usize, h: usize, fps: &str) -> Result<Child> {
    let size = format!("{w}x{h}");
    let mut cmd;
    if args.preview {
        cmd = Command::new("ffplay");
        cmd.args(["-hide_banner", "-loglevel", "error", "-autoexit", "-window_title", "asciivid"])
            .args(["-f", "rawvideo", "-pixel_format", "rgb24"])
            .args(["-video_size", &size, "-framerate", fps, "-i", "-"]);
    } else {
        let out = args.output.as_deref().context("missing output path (or use --preview)")?;
        cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-s", &size, "-r", fps, "-i", "-"])
            .args(seek_args(args))
            .args(["-i", &args.input])
            .args(["-map", "0:v", "-map", "1:a?"])
            .args(["-c:v", "libx264", "-preset", "medium", "-crf", &args.crf.to_string()])
            .args(["-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "192k", "-shortest", out]);
    }
    cmd.stdin(Stdio::piped())
        .spawn()
        .context("failed to start ffmpeg/ffplay output")
}

#[derive(Clone, Copy)]
struct Cell {
    glyph: u8,
    color: [f32; 3],
    /// How far the cell background is tinted toward `color`.
    fill: f32,
}

/// Random rectangle of cells, sized relative to the grid.
fn rand_rect(rng: &mut fastrand::Rng, cols: usize, rows: usize) -> (usize, usize, usize, usize) {
    let w = rng.usize(cols / 12 + 1..=cols / 2 + 1).min(cols);
    let h = rng.usize(1..=rows / 6 + 1).min(rows);
    (rng.usize(0..=cols - w), rng.usize(0..=rows - h), w, h)
}

struct Glitcher {
    rng: fastrand::Rng,
    intensity: f32,
    burst_left: u32,
}

impl Glitcher {
    /// Scale event probability up during a burst.
    fn p(&self, base: f32) -> f32 {
        let boost = if self.burst_left > 0 { 4.0 } else { 1.0 };
        (base * self.intensity * boost).min(1.0)
    }

    fn cells(&mut self, cells: &mut [Cell], cols: usize, rows: usize, n_glyphs: usize, fg: [f32; 3]) {
        if self.burst_left > 0 {
            self.burst_left -= 1;
        } else if self.rng.f32() < self.intensity * 0.03 {
            self.burst_left = self.rng.u32(3..=12);
        }

        // Scramble: random glyphs inside a block.
        if self.rng.f32() < self.p(0.3) {
            let (x, y, w, h) = rand_rect(&mut self.rng, cols, rows);
            for r in y..y + h {
                for c in x..x + w {
                    let cell = &mut cells[r * cols + c];
                    cell.glyph = self.rng.u8(1..n_glyphs as u8);
                    if cell.color == [0.0; 3] {
                        cell.color = fg;
                    }
                }
            }
        }
        // Dropout: blank a block, or blow it out to the densest glyph.
        if self.rng.f32() < self.p(0.4) {
            let (x, y, w, h) = rand_rect(&mut self.rng, cols, rows);
            let solid = self.rng.f32() < 0.3;
            for r in y..y + h {
                for c in x..x + w {
                    let cell = &mut cells[r * cols + c];
                    if solid {
                        cell.glyph = (n_glyphs - 1) as u8;
                        cell.color = fg;
                    } else {
                        cell.glyph = 0;
                        cell.fill = 0.0;
                    }
                }
            }
        }
        // Row roll: whole grid jumps vertically.
        if self.rng.f32() < self.p(0.05) {
            let k = self.rng.usize(1..=rows / 8 + 1) * cols;
            cells.rotate_right(k.min(cells.len()));
        }
    }

    fn pixels(&mut self, buf: &mut [u8], w: usize, h: usize, ch: usize) {
        // Tear: shift horizontal pixel bands sideways.
        if self.rng.f32() < self.p(0.6) {
            let bands = 1 + self.rng.usize(0..=(self.intensity * 6.0) as usize);
            let max_shift = ((w as f32 * 0.25 * self.intensity) as usize).max(4);
            for _ in 0..bands {
                let bh = self.rng.usize(2..=ch * 6).min(h);
                let y0 = self.rng.usize(0..=h - bh);
                let shift = self.rng.usize(1..=max_shift);
                let left = self.rng.bool();
                for y in y0..y0 + bh {
                    let row = &mut buf[y * w * 3..(y + 1) * w * 3];
                    if left {
                        row.rotate_left(shift * 3);
                    } else {
                        row.rotate_right(shift * 3);
                    }
                }
            }
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.output.is_none() && !args.preview {
        bail!("give an output path, or --preview");
    }

    let font_path = match &args.font {
        Some(p) => p.clone(),
        None => default_font()?,
    };
    let font_bytes = std::fs::read(&font_path)
        .with_context(|| format!("reading font {}", font_path.display()))?;
    let ramp = args.ramp.as_deref().unwrap_or(args.charset.ramp());
    let atlas = build_atlas(&font_bytes, args.font_size, ramp, !args.no_sort, args.bold)?;
    let (cw, ch) = (atlas.cw, atlas.ch);
    let n_glyphs = atlas.glyphs.len();

    let info = probe(&args.input)?;
    let cols = args.cols.map(|c| c as usize).unwrap_or(args.width as usize / cw).max(1);
    let rows = ((cols as f64 * info.height as f64 / info.width as f64 * cw as f64 / ch as f64)
        .round() as usize)
        .max(1);
    // yuv420p needs even dimensions; pad with background.
    let out_w = (cols * cw + 1) & !1;
    let out_h = (rows * ch + 1) & !1;

    eprintln!(
        "{} → {cols}x{rows} cells, {cw}x{ch}px glyphs ({}), {out_w}x{out_h} @ {:.3} fps",
        args.input,
        font_path.display(),
        info.fps_f
    );

    let fg = parse_hex(&args.fg)?;
    let bg = parse_hex(&args.bg)?;
    let bg_u8 = bg.map(|v| v as u8);
    let to_lin: [f32; 256] = std::array::from_fn(|v| srgb_to_linear(v as f32 / 255.0));
    let to_srgb: Vec<u8> = (0..4096).map(|i| (linear_to_srgb(i as f32 / 4095.0) * 255.0).round() as u8).collect();
    let lin = |c: [f32; 3]| c.map(|v| to_lin[v.clamp(0.0, 255.0) as usize]);
    let bg_lin = lin(bg);
    let lut = atlas.lut(!args.no_sort);
    let bg_fill = args.bg_fill.clamp(0.0, 1.0);

    // Loop GIFs in preview until the window is closed.
    let looping = args.preview && info.is_gif && !args.no_loop;
    let mut decoder = spawn_decoder(&args, cols, rows, looping)?;
    let mut sink = spawn_sink(&args, out_w, out_h, &info.fps)?;
    let mut src_out = decoder.stdout.take().unwrap();
    let mut sink_in = sink.stdin.take().unwrap();

    let mut src = vec![0u8; cols * rows * 3];
    let mut cells = vec![Cell { glyph: 0, color: fg, fill: 0.0 }; cols * rows];
    let mut lum = vec![0f32; cols * rows];
    let mut curve: Option<[f32; 256]> = None;
    let eq = args.equalize.clamp(0.0, 1.0);
    let mut out = vec![0u8; out_w * out_h * 3];
    let mut rng = fastrand::Rng::with_seed(args.seed);
    let mut glitcher = Glitcher {
        rng: fastrand::Rng::with_seed(args.seed ^ 0x9e37_79b9),
        intensity: args.glitch.clamp(0.0, 1.0),
        burst_left: 0,
    };

    let whole_file = args.start.is_none() && args.duration.is_none();
    let total_frames = (info.is_gif && whole_file)
        .then(|| count_frames(&args.input))
        .flatten()
        .or_else(|| {
            let d = args.duration.as_deref().and_then(|t| t.parse::<f64>().ok()).or(info.duration)?;
            Some((d * info.fps_f).round() as u64)
        })
        .filter(|&n| n > 0);
    let started = std::time::Instant::now();
    let mut frame = 0u64;

    loop {
        match src_out.read_exact(&mut src) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e).context("reading decoded frames"),
        }

        for (l, px) in lum.iter_mut().zip(src.chunks_exact(3)) {
            *l = (0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32) / 255.0;
        }
        if !args.no_auto_levels {
            let next = level_curve(&lum, eq);
            // Ease toward the new curve so levels don't pump frame to frame.
            curve = Some(match curve {
                Some(c) => std::array::from_fn(|i| c[i] + (next[i] - c[i]) * 0.15),
                None => next,
            });
        }

        for ((cell, px), &l) in cells.iter_mut().zip(src.chunks_exact(3)).zip(&lum) {
            let [r, g, b] = [px[0] as f32, px[1] as f32, px[2] as f32];
            let mut l = match &curve {
                Some(c) => c[(l * 255.0) as usize],
                None => l,
            };
            l = ((l - 0.5) * args.contrast + 0.5 + args.brightness).clamp(0.0, 1.0);
            l = l.powf(args.gamma);
            if args.invert {
                l = 1.0 - l;
            }
            if args.jitter > 0.0 {
                l = (l + (rng.f32() - 0.5) * args.jitter).clamp(0.0, 1.0);
            }
            cell.glyph = lut[(l * 255.0).round() as usize];
            let base = if args.color {
                // Normalise so dark source colours still read through the glyph shading.
                let m = r.max(g).max(b).max(1.0);
                let c = [r / m * 255.0, g / m * 255.0, b / m * 255.0];
                let grey = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
                c.map(|v| (grey + (v - grey) * args.saturation).clamp(0.0, 255.0))
            } else {
                fg
            };
            let k = 1.0 - args.shade + args.shade * l;
            cell.color = base.map(|v| v * k);
            cell.fill = bg_fill * l;
        }

        if glitcher.intensity > 0.0 {
            glitcher.cells(&mut cells, cols, rows, n_glyphs, fg);
        }

        out.par_chunks_mut(out_w * 3 * ch).enumerate().for_each(|(r, band)| {
            for px in band.chunks_exact_mut(3) {
                px.copy_from_slice(&bg_u8);
            }
            if r >= rows {
                return;
            }
            let enc = |v: f32| to_srgb[(v.clamp(0.0, 1.0) * 4095.0) as usize];
            for c in 0..cols {
                let cell = cells[r * cols + c];
                let filled = cell.fill > 0.0;
                if cell.glyph == 0 && !filled {
                    continue;
                }
                // Blend in linear light so antialiased strokes keep their brightness.
                let fg_lin = lin(cell.color);
                let cell_bg = if filled {
                    lin(std::array::from_fn(|k| bg[k] + (cell.color[k] - bg[k]) * cell.fill))
                } else {
                    bg_lin
                };
                let glyph = &atlas.glyphs[cell.glyph as usize];
                for gy in 0..ch {
                    let row = gy * out_w * 3;
                    if row >= band.len() {
                        break;
                    }
                    for gx in 0..cw {
                        let a = glyph[gy * cw + gx];
                        if a == 0 && !filled {
                            continue;
                        }
                        let a = a as f32 / 255.0;
                        let i = row + (c * cw + gx) * 3;
                        for k in 0..3 {
                            band[i + k] = enc(cell_bg[k] + (fg_lin[k] - cell_bg[k]) * a);
                        }
                    }
                }
            }
        });

        if glitcher.intensity > 0.0 {
            glitcher.pixels(&mut out, out_w, out_h, ch);
        }

        if let Err(e) = sink_in.write_all(&out) {
            if e.kind() == ErrorKind::BrokenPipe {
                break; // preview window closed
            }
            return Err(e).context("writing to encoder");
        }

        frame += 1;
        if frame % 15 == 0 {
            let fps = frame as f64 / started.elapsed().as_secs_f64();
            match total_frames {
                Some(t) if looping => eprint!(
                    "\rframe {}/{t} (loop {}) {fps:.1} fps   ",
                    (frame - 1) % t + 1,
                    (frame - 1) / t + 1
                ),
                Some(t) => eprint!("\rframe {frame}/{t} ({:.0}%) {fps:.1} fps   ", frame as f64 / t as f64 * 100.0),
                None => eprint!("\rframe {frame} {fps:.1} fps   "),
            }
        }
    }

    drop(sink_in);
    let _ = decoder.kill();
    let status = sink.wait()?;
    eprintln!("\rdone: {frame} frames in {:.1}s          ", started.elapsed().as_secs_f64());
    if !status.success() && !args.preview {
        bail!("encoder exited with {status}");
    }
    Ok(())
}
