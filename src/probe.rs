//! What ffprobe says about the input, and ffmpeg's time syntax.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::process::Command;

pub struct Probe {
    /// Display size: after applying rotation and the pixel aspect ratio, which is how
    /// ffmpeg hands the decoded frames over.
    pub width: f64,
    pub height: f64,
    /// Frame rate as ffmpeg spells it, e.g. "30000/1001".
    pub fps: String,
    pub fps_f: f64,
    pub duration: Option<f64>,
    pub is_gif: bool,
    pub is_still: bool,
}

#[derive(Deserialize)]
struct Json {
    #[serde(default)]
    streams: Vec<JsonStream>,
    format: JsonFormat,
}

#[derive(Deserialize)]
struct JsonStream {
    width: u32,
    height: u32,
    r_frame_rate: String,
    sample_aspect_ratio: Option<String>,
    #[serde(default)]
    side_data_list: Vec<JsonSideData>,
    #[serde(default)]
    tags: JsonTags,
}

#[derive(Deserialize)]
struct JsonSideData {
    rotation: Option<f64>,
}

#[derive(Deserialize, Default)]
struct JsonTags {
    /// Pre-display-matrix rotation tag, still written by some muxers.
    rotate: Option<String>,
}

#[derive(Deserialize)]
struct JsonFormat {
    format_name: String,
    duration: Option<String>,
}

pub fn probe(input: &str) -> Result<Probe> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-of", "json"])
        .args([
            "-show_entries",
            "stream=width,height,r_frame_rate,sample_aspect_ratio\
             :stream_side_data=rotation:stream_tags=rotate:format=duration,format_name",
        ])
        .args(["-i", input])
        .output()
        .context("failed to run ffprobe; is ffmpeg installed?")?;
    if !out.status.success() {
        bail!("ffprobe failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    parse(&out.stdout).with_context(|| format!("reading ffprobe output for {input}"))
}

fn parse(json: &[u8]) -> Result<Probe> {
    let j: Json = serde_json::from_slice(json)?;
    let Some(s) = j.streams.into_iter().next() else { bail!("no video stream") };
    if s.width == 0 || s.height == 0 {
        bail!("video stream has no size");
    }
    let rotation = s
        .side_data_list
        .iter()
        .find_map(|d| d.rotation)
        .or_else(|| s.tags.rotate.as_deref()?.parse().ok())
        .unwrap_or(0.0);
    let sar = s
        .sample_aspect_ratio
        .as_deref()
        .and_then(|r| ratio(r, ':'))
        .filter(|r| r.is_finite() && *r > 0.0)
        .unwrap_or(1.0);
    let (mut width, mut height) = (s.width as f64 * sar, s.height as f64);
    if (rotation.rem_euclid(180.0) - 90.0).abs() < 1.0 {
        (width, height) = (height, width);
    }
    let fps_f = ratio(&s.r_frame_rate, '/').filter(|f| f.is_finite() && *f > 0.0);
    let Some(fps_f) = fps_f else { bail!("unusable frame rate {:?}", s.r_frame_rate) };
    let format = j.format.format_name;
    Ok(Probe {
        width,
        height,
        fps: s.r_frame_rate,
        fps_f,
        duration: j.format.duration.and_then(|d| d.parse().ok()),
        is_gif: format == "gif",
        is_still: format == "image2" || format.ends_with("_pipe"),
    })
}

fn ratio(s: &str, sep: char) -> Option<f64> {
    match s.split_once(sep) {
        Some((n, d)) => Some(n.parse::<f64>().ok()? / d.parse::<f64>().ok()?),
        None => s.parse().ok(),
    }
}

/// Exact frame count by counting packets; cheap for GIFs, which have one frame per packet.
pub fn count_frames(input: &str) -> Option<u64> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-count_packets"])
        .args(["-show_entries", "stream=nb_read_packets", "-of", "csv=p=0", "-i", input])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok().filter(|&n| n > 0)
}

/// Parses an ffmpeg duration: `[-][HH:]MM:SS[.m...]` or `[-]S+[.m...][s|ms|us]`.
pub fn parse_time(s: &str) -> Result<f64> {
    let bad = || anyhow::anyhow!("invalid time {s:?}; use e.g. 48, 1.5, 0:48 or 1:02:03.5");
    let (neg, body) = match s.trim().strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.trim()),
    };
    let num = |p: &str| -> Result<f64> {
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return Err(bad());
        }
        p.parse::<f64>().map_err(|_| bad())
    };
    let secs = if body.contains(':') {
        let parts: Vec<&str> = body.split(':').collect();
        if parts.len() > 3 {
            return Err(bad());
        }
        let (whole, last) = parts.split_at(parts.len() - 1);
        if whole.iter().any(|p| p.contains('.')) {
            return Err(bad());
        }
        whole.iter().try_fold(0.0, |acc, p| Ok::<_, anyhow::Error>(acc * 60.0 + num(p)? * 60.0))? + num(last[0])?
    } else if let Some(v) = body.strip_suffix("ms") {
        num(v)? / 1e3
    } else if let Some(v) = body.strip_suffix("us") {
        num(v)? / 1e6
    } else {
        num(body.strip_suffix('s').unwrap_or(body))?
    };
    Ok(if neg { -secs } else { secs })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe_json(stream: &str, format: &str) -> Probe {
        parse(format!(r#"{{"streams":[{stream}],"format":{format}}}"#).as_bytes()).unwrap()
    }

    #[test]
    fn rotation_swaps_dimensions() {
        let p = probe_json(
            r#"{"width":640,"height":360,"r_frame_rate":"30/1","side_data_list":[{"rotation":-90}]}"#,
            r#"{"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"3.0"}"#,
        );
        assert_eq!((p.width, p.height), (360.0, 640.0));
        assert_eq!(p.duration, Some(3.0));
        let p = probe_json(
            r#"{"width":640,"height":360,"r_frame_rate":"30/1","tags":{"rotate":"270"}}"#,
            r#"{"format_name":"mov"}"#,
        );
        assert_eq!((p.width, p.height), (360.0, 640.0));
        let p = probe_json(
            r#"{"width":640,"height":360,"r_frame_rate":"30/1","side_data_list":[{"rotation":180}]}"#,
            r#"{"format_name":"mov"}"#,
        );
        assert_eq!((p.width, p.height), (640.0, 360.0));
    }

    #[test]
    fn sample_aspect_ratio_widens() {
        let p = probe_json(
            r#"{"width":480,"height":360,"r_frame_rate":"25/1","sample_aspect_ratio":"4:3"}"#,
            r#"{"format_name":"mov"}"#,
        );
        assert_eq!((p.width, p.height), (640.0, 360.0));
        let p = probe_json(
            r#"{"width":480,"height":360,"r_frame_rate":"25/1","sample_aspect_ratio":"0:1"}"#,
            r#"{"format_name":"mov"}"#,
        );
        assert_eq!(p.width, 480.0);
    }

    #[test]
    fn formats() {
        let s = r#"{"width":10,"height":10,"r_frame_rate":"25/1"}"#;
        assert!(probe_json(s, r#"{"format_name":"gif"}"#).is_gif);
        assert!(probe_json(s, r#"{"format_name":"png_pipe"}"#).is_still);
        assert!(parse(br#"{"streams":[{"width":10,"height":10,"r_frame_rate":"0/0"}],"format":{"format_name":"x"}}"#).is_err());
        assert!(parse(br#"{"streams":[],"format":{"format_name":"mp3"}}"#).is_err());
    }

    #[test]
    fn times() {
        for (s, v) in [
            ("48", 48.0),
            ("1.5", 1.5),
            ("0:48", 48.0),
            ("1:02:03.5", 3723.5),
            ("90s", 90.0),
            ("250ms", 0.25),
            ("-2", -2.0),
        ] {
            assert_eq!(parse_time(s).unwrap(), v, "{s}");
        }
        for s in ["banana", "", "1:2:3:4", "1.5:00", "5m", "1e3"] {
            assert!(parse_time(s).is_err(), "{s}");
        }
    }
}
