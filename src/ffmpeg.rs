//! Spawning the ffmpeg decoder and the ffmpeg/ffplay sink.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// What kind of file the output extension asks for, which decides the codecs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OutputKind {
    /// mp4/mov/mkv/m4v: H.264 + AAC.
    H264,
    /// VP9 + Opus.
    Webm,
    /// Palette-optimised looping GIF, no audio.
    Gif,
    /// A single frame.
    Image,
    /// Unknown extension: let ffmpeg pick its defaults for the container.
    Other,
}

impl OutputKind {
    pub fn from_path(path: &str) -> OutputKind {
        let ext = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "mp4" | "m4v" | "mov" | "mkv" => OutputKind::H264,
            "webm" => OutputKind::Webm,
            "gif" => OutputKind::Gif,
            "png" | "jpg" | "jpeg" | "bmp" | "tif" | "tiff" | "webp" => OutputKind::Image,
            _ => OutputKind::Other,
        }
    }

    fn has_audio(self) -> bool {
        matches!(self, OutputKind::H264 | OutputKind::Webm | OutputKind::Other)
    }
}

pub struct Seek {
    pub start: Option<String>,
    pub duration: Option<String>,
}

impl Seek {
    fn args(&self) -> Vec<&str> {
        let mut v = Vec::new();
        if let Some(s) = &self.start {
            v.extend(["-ss", s]);
        }
        if let Some(t) = &self.duration {
            v.extend(["-t", t]);
        }
        v
    }
}

/// Children get their own process group so a terminal Ctrl-C reaches only us, and we shut
/// them down in order: the decoder can't spray broken-pipe errors at a reader that's gone,
/// and the encoder always gets to finish the file.
fn isolate(cmd: &mut Command) -> &mut Command {
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(cmd, 0);
    cmd
}

fn decode_filter(cols: usize, rows: usize, sharpen: f32) -> String {
    let mut vf = format!("scale={cols}:{rows}:flags=area");
    if sharpen > 0.0 {
        vf += &format!(",unsharp=3:3:{0}:3:3:{0}", sharpen.min(5.0));
    }
    vf + ",format=rgb24"
}

/// Decodes the first video stream, scaled to one pixel per cell, as raw RGB24 on stdout.
pub fn spawn_decoder(
    input: &str,
    seek: &Seek,
    cols: usize,
    rows: usize,
    sharpen: f32,
    looping: bool,
) -> Result<Child> {
    isolate(&mut Command::new("ffmpeg"))
        // -nostdin: in its own process group, reading the terminal would stop it (SIGTTIN).
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(if looping { &["-stream_loop", "-1"][..] } else { &[] })
        .args(seek.args())
        .args(["-i", input, "-map", "0:v:0"])
        .args(["-vf", &decode_filter(cols, rows, sharpen)])
        .args(["-f", "rawvideo", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to start ffmpeg; is it installed?")
}

pub struct Sink<'a> {
    pub input: &'a str,
    pub seek: &'a Seek,
    pub width: usize,
    pub height: usize,
    pub fps: &'a str,
    pub crf: u8,
}

/// ffplay window reading raw RGBA frames on stdin.
pub fn spawn_preview(s: &Sink) -> Result<Child> {
    let size = format!("{}x{}", s.width, s.height);
    isolate(&mut Command::new("ffplay"))
        // -nostats: otherwise ffplay clears the terminal line on exit, wiping our progress.
        .args(["-hide_banner", "-loglevel", "error", "-nostats", "-autoexit", "-window_title", "asciivid"])
        .args(["-f", "rawvideo", "-pixel_format", "rgba"])
        .args(["-video_size", &size, "-framerate", s.fps, "-i", "-"])
        .stdin(Stdio::piped())
        .spawn()
        .context("failed to start ffplay; is it installed?")
}

/// ffmpeg encoder reading raw RGBA frames on stdin, muxing in the source audio where the
/// output format carries it.
pub fn spawn_encoder(s: &Sink, output: &str) -> Result<Child> {
    let kind = OutputKind::from_path(output);
    let size = format!("{}x{}", s.width, s.height);
    let crf = s.crf.to_string();
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-s", &size, "-r", s.fps, "-i", "-"]);
    if kind.has_audio() {
        cmd.args(s.seek.args())
            .args(["-i", s.input])
            .args(["-map", "0:v", "-map", "1:a:0?", "-shortest"]);
    }
    match kind {
        OutputKind::H264 => {
            cmd.args(["-c:v", "libx264", "-preset", "medium", "-crf", &crf, "-pix_fmt", "yuv420p"])
                .args(["-c:a", "aac", "-b:a", "192k"]);
        }
        OutputKind::Webm => {
            cmd.args(["-c:v", "libvpx-vp9", "-crf", &crf, "-b:v", "0", "-row-mt", "1"])
                .args(["-pix_fmt", "yuv420p", "-c:a", "libopus", "-b:a", "160k"]);
        }
        OutputKind::Gif => {
            cmd.args(["-filter_complex", "split[a][b];[a]palettegen[p];[b][p]paletteuse=dither=none"])
                .args(["-loop", "0"]);
        }
        OutputKind::Image => {
            cmd.args(["-frames:v", "1", "-update", "1"]);
        }
        OutputKind::Other => {}
    }
    isolate(cmd.arg(output))
        .stdin(Stdio::piped())
        .spawn()
        .context("failed to start ffmpeg encoder")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_kinds() {
        assert_eq!(OutputKind::from_path("a/b.MP4"), OutputKind::H264);
        assert_eq!(OutputKind::from_path("x.mkv"), OutputKind::H264);
        assert_eq!(OutputKind::from_path("x.webm"), OutputKind::Webm);
        assert_eq!(OutputKind::from_path("x.gif"), OutputKind::Gif);
        assert_eq!(OutputKind::from_path("x.jpeg"), OutputKind::Image);
        assert_eq!(OutputKind::from_path("x.avi"), OutputKind::Other);
        assert_eq!(OutputKind::from_path("noext"), OutputKind::Other);
    }
}
