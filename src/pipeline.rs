//! The decode → map → glitch → render → encode loop.

use crate::atlas::{self, Atlas};
use crate::ffmpeg::{self, OutputKind, Seek, Sink};
use crate::glitch::Glitcher;
use crate::grid::{Cell, Mapper};
use crate::params::{Invert, Params, parse_hex};
use crate::probe::{self, parse_time};
use crate::render::{CpuRenderer, Layout, Renderer};
use crate::tone::{Levels, border_mean, luma};
use anyhow::{Context, Result, bail, ensure};
use rayon::prelude::*;
use std::io::{ErrorKind, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// One render: where frames come from and where they go. The look lives in `Params`.
pub struct Job {
    pub input: String,
    /// Output file; `None` with `preview`.
    pub output: Option<String>,
    pub seek: Seek,
    pub crf: u8,
    /// Play in an ffplay window instead of writing a file.
    pub preview: bool,
    /// Loop GIFs (and hold stills) in preview until the window is closed.
    pub loop_preview: bool,
    pub gpu: bool,
}

pub struct Summary {
    pub frames: u64,
    /// Stopped by Ctrl-C; any output file is still complete up to that frame.
    pub interrupted: bool,
}

/// Why the frame loop ended, which decides how the children are shut down.
enum End {
    /// The decoder ran out of frames.
    Eof,
    /// Ctrl-C.
    Stopped,
    /// The sink went away: ffplay window closed, or the encoder failed.
    SinkClosed,
    /// Wrote every frame the output wants (a single image).
    Done,
}

pub fn run(p: &Params, job: &Job, stop: &AtomicBool) -> Result<Summary> {
    p.validate()?;
    match (&job.output, job.preview) {
        (None, false) => bail!("give an output path, or --preview"),
        (Some(_), true) => bail!("--preview plays in a window and writes no file; drop the output path"),
        _ => {}
    }
    let start = job.seek.start.as_deref().map(parse_time).transpose().context("--start")?;
    let duration = job.seek.duration.as_deref().map(parse_time).transpose().context("--duration")?;
    ensure!(start.is_none_or(|s| s >= 0.0), "--start can't be negative");
    ensure!(duration.is_none_or(|d| d > 0.0), "--duration must be positive");

    let font_path = match &p.font {
        Some(f) => f.clone(),
        None => atlas::default_font()?,
    };
    let font_bytes = std::fs::read(&font_path).with_context(|| format!("reading font {}", font_path.display()))?;
    let atlas = Atlas::build(&font_bytes, p.font_size, p.ramp(), p.sort, p.bold)?;

    let info = probe::probe(&job.input)?;
    let cols = p.cols.map_or(p.width as usize / atlas.cw, |c| c as usize).max(1);
    let rows = ((cols as f64 * info.height / info.width * atlas.cw as f64 / atlas.ch as f64).round() as usize).max(1);
    let layout = Layout::new(cols, rows, atlas.cw, atlas.ch);

    let fg = parse_hex(&p.fg)?;
    let bg = parse_hex(&p.bg)?;
    let mapper = Mapper::new(p, atlas.lut(p.sort), fg);
    let glitcher = Glitcher::new(p.glitch, p.seed, &atlas);
    let (mut renderer, renderer_name) = make_renderer(atlas, layout, bg, job.gpu)?;

    eprintln!(
        "{} → {cols}x{rows} cells, {}x{}px glyphs ({}), {}x{} @ {:.3} fps, {renderer_name}",
        job.input,
        layout.cw,
        layout.ch,
        font_path.display(),
        layout.out_w,
        layout.out_h,
        info.fps_f
    );

    // Auto decides from the first frame, so it's limited to stills and GIFs where that frame
    // is representative (videos fade in and change scenes). It's also mono-only: inverting
    // colour output would leave bright regions sparse.
    let mut invert_choice = match p.invert {
        Invert::On => Some(true),
        Invert::Auto if !p.color && (info.is_gif || info.is_still) => None,
        Invert::Auto | Invert::Off => Some(false),
    };
    let kind = job.output.as_deref().map(OutputKind::from_path);
    let max_frames = (kind == Some(OutputKind::Image)).then_some(1);
    let loop_gif = job.preview && job.loop_preview && info.is_gif;
    // A still has one frame; in preview, keep re-rendering it so jitter and glitches animate.
    let hold_still = job.preview && job.loop_preview && info.is_still;

    let total_frames = max_frames.or_else(|| {
        if info.is_gif && start.is_none() && duration.is_none() {
            return probe::count_frames(&job.input);
        }
        let available = info.duration.map(|d| (d - start.unwrap_or(0.0)).max(0.0));
        let d = match (duration, available) {
            (Some(a), Some(b)) => a.min(b),
            (a, b) => a.or(b)?,
        };
        Some((d * info.fps_f).round() as u64).filter(|&n| n > 0)
    });
    // Absolute index of the first frame, so jitter and glitches match a full render.
    let first_frame = (start.unwrap_or(0.0) * info.fps_f).round() as u64;

    let mut decoder = ffmpeg::spawn_decoder(&job.input, &job.seek, cols, rows, p.sharpen, loop_gif)?;
    let sink = Sink {
        input: &job.input,
        seek: &job.seek,
        width: layout.out_w,
        height: layout.out_h,
        fps: &info.fps,
        crf: job.crf,
    };
    let sink = match &job.output {
        Some(out) => ffmpeg::spawn_encoder(&sink, out),
        None => ffmpeg::spawn_preview(&sink),
    };
    let mut sink = match sink {
        Ok(s) => s,
        Err(e) => {
            reap(&mut decoder);
            return Err(e);
        }
    };
    let mut src_out = decoder.stdout.take().unwrap();
    let mut sink_in = sink.stdin.take().unwrap();

    let started = Instant::now();
    let mut frame = 0u64;
    let mut frame_loop = || -> Result<End> {
        let mut src = vec![0u8; cols * rows * 3];
        let mut lum = vec![0f32; cols * rows];
        let mut cells = vec![Cell { glyph: 0, color: fg, fill: 0.0 }; cols * rows];
        let mut levels = Levels::default();
        let mut out = vec![0u8; layout.frame_bytes()];
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(End::Stopped);
            }
            if max_frames.is_some_and(|n| frame >= n) {
                return Ok(End::Done);
            }
            if !(hold_still && frame > 0) {
                match src_out.read_exact(&mut src) {
                    Ok(()) => {}
                    Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(End::Eof),
                    Err(e) => return Err(e).context("reading decoded frames"),
                }
                lum.par_iter_mut().zip(src.par_chunks_exact(3)).for_each(|(l, px)| *l = luma(px));
            }
            let index = first_frame + frame;

            let invert = *invert_choice.get_or_insert_with(|| {
                let light = border_mean(&lum, cols, rows) > 0.7;
                if light {
                    eprintln!("light background: inverting (--invert off to keep it)");
                }
                light
            });
            let curve = p.auto_levels.then(|| *levels.update(&lum, p.equalize));
            mapper.map(&mut cells, &src, &lum, curve.as_ref(), invert, index);
            let tears = if glitcher.active() {
                glitcher.cells(index, &mut cells, cols, rows, fg);
                glitcher.tears(index, layout.out_w, layout.out_h, layout.ch)
            } else {
                Vec::new()
            };
            renderer.render(&cells, &tears, &mut out)?;

            if let Err(e) = sink_in.write_all(&out) {
                if e.kind() == ErrorKind::BrokenPipe {
                    return Ok(End::SinkClosed);
                }
                return Err(e).context("writing to encoder");
            }
            frame += 1;
            if frame.is_multiple_of(15) {
                progress(frame, total_frames, loop_gif, started);
            }
        }
    };
    let end = frame_loop();
    drop(frame_loop);
    let summary = |interrupted| Summary { frames: frame, interrupted };
    finish(end, decoder, src_out, sink, sink_in, job.preview, started, frame).map(summary)
}

#[cfg(feature = "gpu")]
fn make_renderer(atlas: Atlas, layout: Layout, bg: [f32; 3], gpu: bool) -> Result<(Box<dyn Renderer>, String)> {
    if gpu {
        let r = crate::render::GpuRenderer::new(&atlas, layout, bg).context("--gpu")?;
        let name = format!("GPU: {}", r.name);
        return Ok((Box::new(r), name));
    }
    Ok((Box::new(CpuRenderer::new(atlas, layout, bg)), "CPU".into()))
}

#[cfg(not(feature = "gpu"))]
fn make_renderer(atlas: Atlas, layout: Layout, bg: [f32; 3], gpu: bool) -> Result<(Box<dyn Renderer>, String)> {
    ensure!(!gpu, "this build has no GPU support; rebuild with the `gpu` feature");
    Ok((Box::new(CpuRenderer::new(atlas, layout, bg)), "CPU".into()))
}

fn progress(frame: u64, total: Option<u64>, looping: bool, started: Instant) {
    let fps = frame as f64 / started.elapsed().as_secs_f64();
    match total {
        Some(t) if looping => eprint!(
            "\rframe {}/{t} (loop {}) {fps:.1} fps   ",
            (frame - 1) % t + 1,
            (frame - 1) / t + 1
        ),
        Some(t) => eprint!(
            "\rframe {frame}/{t} ({:.0}%) {fps:.1} fps   ",
            (frame as f64 / t as f64 * 100.0).min(100.0)
        ),
        None => eprint!("\rframe {frame} {fps:.1} fps   "),
    }
}

/// Kill a child and collect it, ignoring errors (it may already be gone).
fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Shuts the children down in the right order for how the loop ended. Returns whether the
/// run was interrupted.
#[allow(clippy::too_many_arguments)]
fn finish(
    end: Result<End>,
    mut decoder: Child,
    src_out: ChildStdout,
    mut sink: Child,
    sink_in: ChildStdin,
    preview: bool,
    started: Instant,
    frames: u64,
) -> Result<bool> {
    let end = match end {
        Ok(end) => end,
        Err(e) => {
            drop(sink_in);
            reap(&mut decoder);
            reap(&mut sink);
            eprintln!();
            return Err(e);
        }
    };
    // The decoder's exit status only means something if it ended on its own.
    let decoded = match end {
        End::Eof => Some(decoder.wait()?),
        _ => {
            reap(&mut decoder);
            None
        }
    };
    drop(src_out);
    // Closing stdin is the encoder's signal to flush and finish the file.
    drop(sink_in);
    if preview && matches!(end, End::Stopped) {
        let _ = sink.kill();
    }
    let sunk = sink.wait()?;

    let secs = started.elapsed().as_secs_f64();
    let interrupted = matches!(end, End::Stopped);
    let verb = if interrupted { "stopped" } else { "done" };
    eprintln!("\r{verb}: {frames} frames in {secs:.1}s          ");
    if let Some(status) = decoded.filter(|s| !s.success()) {
        bail!("ffmpeg decoder exited with {status}; see its error above");
    }
    if !preview && !sunk.success() {
        bail!("ffmpeg encoder exited with {sunk}; see its error above");
    }
    Ok(interrupted)
}
