//! Everything that defines a look, loadable from TOML presets.

use anyhow::{Context, Result, bail, ensure};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Charset {
    /// Sparse terminal look: " .:;+=01#"
    Wd2,
    Classic,
    Binary,
    Blocks,
    Dense,
}

impl Charset {
    pub fn ramp(self) -> &'static str {
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

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Invert {
    /// Invert light-background stills and GIFs in mono mode so the background stays dark
    Auto,
    On,
    Off,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Params {
    /// Target output width in pixels; sets the column count from the font's cell width.
    pub width: u32,
    /// Number of character columns (overrides `width`).
    pub cols: Option<u32>,
    /// Monospace font file (default: fc-match monospace).
    pub font: Option<PathBuf>,
    pub font_size: f32,
    pub bold: bool,

    pub charset: Charset,
    /// Custom ramp (overrides `charset`).
    pub ramp: Option<String>,
    /// Sort the ramp by ink density.
    pub sort: bool,

    pub fg: String,
    pub bg: String,
    /// Colour glyphs with the source pixel colour instead of `fg`.
    pub color: bool,
    pub shade: f32,
    pub saturation: f32,
    /// Default: 0.3 with `color`, else 0.
    pub bg_fill: Option<f32>,

    pub invert: Invert,
    pub auto_levels: bool,
    pub equalize: f32,
    pub sharpen: f32,
    pub contrast: f32,
    pub brightness: f32,
    pub gamma: f32,
    pub jitter: f32,

    pub glitch: f32,
    pub seed: u64,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            width: 1920,
            cols: None,
            font: None,
            font_size: 10.0,
            bold: false,
            charset: Charset::Wd2,
            ramp: None,
            sort: true,
            fg: "#ffffff".into(),
            bg: "#000000".into(),
            color: false,
            shade: 0.3,
            saturation: 1.5,
            bg_fill: None,
            invert: Invert::Auto,
            auto_levels: true,
            equalize: 0.5,
            sharpen: 0.0,
            contrast: 1.0,
            brightness: 0.0,
            gamma: 1.0,
            jitter: 0.0,
            glitch: 0.0,
            seed: 1337,
        }
    }
}

/// Built-in presets, as (name, TOML). Each is a partial set of overrides on the defaults.
pub const PRESETS: &[(&str, &str)] = &[
    ("dedsec", include_str!("../presets/dedsec.toml")),
    ("wd2", include_str!("../presets/wd2.toml")),
    ("matrix", include_str!("../presets/matrix.toml")),
    ("vivid", include_str!("../presets/vivid.toml")),
];

impl Params {
    /// Loads a built-in preset by name, or a TOML file by path.
    pub fn preset(name_or_path: &str) -> Result<Params> {
        let (src, text) = match PRESETS.iter().find(|(n, _)| *n == name_or_path) {
            Some((_, text)) => (format!("preset {name_or_path:?}"), text.to_string()),
            None => {
                let text = std::fs::read_to_string(name_or_path).with_context(|| {
                    let names: Vec<_> = PRESETS.iter().map(|(n, _)| *n).collect();
                    format!(
                        "{name_or_path:?} is not a built-in preset ({}) or a readable file",
                        names.join(", ")
                    )
                })?;
                (name_or_path.to_string(), text)
            }
        };
        toml::from_str(&text).with_context(|| format!("parsing {src}"))
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("Params always serializes")
    }

    pub fn ramp(&self) -> &str {
        self.ramp.as_deref().unwrap_or(self.charset.ramp())
    }

    pub fn bg_fill(&self) -> f32 {
        self.bg_fill.unwrap_or(if self.color { 0.3 } else { 0.0 })
    }

    pub fn validate(&self) -> Result<()> {
        let unit = [
            ("shade", self.shade),
            ("bg-fill", self.bg_fill()),
            ("equalize", self.equalize),
            ("jitter", self.jitter),
            ("glitch", self.glitch),
        ];
        for (name, v) in unit {
            ensure!((0.0..=1.0).contains(&v), "--{name} must be between 0 and 1, got {v}");
        }
        let positive = [
            ("font-size", self.font_size),
            ("gamma", self.gamma),
        ];
        for (name, v) in positive {
            ensure!(v.is_finite() && v > 0.0, "--{name} must be greater than 0, got {v}");
        }
        ensure!(self.font_size <= 512.0, "--font-size must be at most 512, got {}", self.font_size);
        ensure!(
            self.contrast.is_finite() && self.contrast >= 0.0,
            "--contrast must be 0 or more, got {}",
            self.contrast
        );
        ensure!(
            self.saturation.is_finite() && self.saturation >= 0.0,
            "--saturation must be 0 or more, got {}",
            self.saturation
        );
        ensure!(
            (-1.0..=1.0).contains(&self.brightness),
            "--brightness must be between -1 and 1, got {}",
            self.brightness
        );
        // ffmpeg's unsharp filter caps luma amount at 5.
        ensure!((0.0..=5.0).contains(&self.sharpen), "--sharpen must be between 0 and 5, got {}", self.sharpen);
        ensure!(self.width > 0, "--width must be greater than 0");
        ensure!(self.cols != Some(0), "--cols must be greater than 0");
        parse_hex(&self.fg)?;
        parse_hex(&self.bg)?;
        Ok(())
    }
}

pub fn parse_hex(s: &str) -> Result<[f32; 3]> {
    let h = s.trim_start_matches('#');
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("colour must be #rrggbb, got {s:?}");
    }
    let v = u32::from_str_radix(h, 16)?;
    Ok([(v >> 16) as f32, ((v >> 8) & 0xff) as f32, (v & 0xff) as f32])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_presets_parse_and_validate() {
        for (name, _) in PRESETS {
            let p = Params::preset(name).unwrap();
            p.validate().unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_ne!(p, Params::default(), "{name} changes nothing");
        }
    }

    #[test]
    fn toml_roundtrip() {
        let p = Params { cols: Some(80), ramp: Some(" .#".into()), ..Params::preset("vivid").unwrap() };
        assert_eq!(toml::from_str::<Params>(&p.to_toml()).unwrap(), p);
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<Params>("glich = 0.5").is_err());
    }

    #[test]
    fn validation() {
        assert!(Params::default().validate().is_ok());
        let bad = [
            Params { gamma: -1.0, ..Default::default() },
            Params { gamma: 0.0, ..Default::default() },
            Params { gamma: f32::NAN, ..Default::default() },
            Params { glitch: 2.0, ..Default::default() },
            Params { font_size: 0.0, ..Default::default() },
            Params { fg: "#12345g".into(), ..Default::default() },
            Params { sharpen: 9.0, ..Default::default() },
        ];
        for p in bad {
            assert!(p.validate().is_err(), "{p:?}");
        }
    }

    #[test]
    fn hex() {
        assert_eq!(parse_hex("#ff8000").unwrap(), [255.0, 128.0, 0.0]);
        assert!(parse_hex("+fffff").is_err());
        assert!(parse_hex("fff").is_err());
    }
}
