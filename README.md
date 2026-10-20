# asciivid

Render any video as ASCII art into a new video file, with optional DedSec-style glitches. Think the *Watch Dogs 2* main menu.

ffmpeg decodes each frame and scales it down to the character grid. Each cell's luminance picks a glyph from a ramp, which is sorted by ink density automatically. The glyphs are rasterized from a real monospace font, glitch passes are applied, and the result is piped back into ffmpeg with the original audio.

## Install

Requires `ffmpeg` (with `ffprobe`; `ffplay` for `--preview`) and `fontconfig`.

```sh
cargo install --git https://github.com/KemonoNeco/asciivid
```

Make sure `~/.cargo/bin` is on your `PATH` (fish: `fish_add_path ~/.cargo/bin`).

GPU rendering (`--gpu`, via wgpu: Vulkan, Metal, DX12 or GL) is built in by default. Build with `--no-default-features` to leave it out.

## Usage

```sh
# Watch Dogs-ish: white on black, shimmering glyphs, glitches
asciivid in.mp4 out.mp4 --preset dedsec

# Closer to the WD2 menu: tiny binary glyphs, crushed shadows
asciivid in.mp4 out.mp4 --preset wd2

# Presets are just defaults; flags still override them
asciivid in.mp4 out.mp4 --preset wd2 --glitch 0.3

# Live preview instead of rendering (GIFs and stills loop until the window is closed)
asciivid in.mp4 --preview --glitch 0.5

# Only render a slice
asciivid in.mp4 clip.mp4 --start 0:48 --duration 10

# The output extension picks the format
asciivid in.gif out.gif
asciivid photo.jpg out.png

# YouTube: download first
yt-dlp -f 'bv*[height<=1080]+ba' -o src.mp4 URL && asciivid src.mp4 out.mp4
```

Ctrl-C stops a render early and still leaves a playable file. Press it twice to quit immediately.

## Presets

A preset is a TOML file holding any of the look options below, with `_` in place of `-` (`font_size = 8.0`, `charset = "binary"`). Built in: `dedsec`, `wd2`, `matrix`, `vivid` (see [`presets/`](presets)).

Options apply in order: defaults, then `--preset`, then flags. To save a look you've dialled in, print it and pass the file back later:

```sh
asciivid --preset wd2 --glitch 0.3 --jitter 0.1 --print-preset > mine.toml
asciivid in.mp4 out.mp4 --preset mine.toml
```

## Output formats

| Extension | Output |
|---|---|
| `mp4` `mov` `mkv` `m4v` | H.264 + AAC |
| `webm` | VP9 + Opus |
| `gif` | Palette-optimised looping GIF, no audio |
| `png` `jpg` `bmp` `tif` `webp` | First frame only (use `--start` to choose it) |
| anything else | ffmpeg's defaults for that container |

## Options

### Look (also settable in presets)

| Option | Default | Description |
|---|---|---|
| `--width` | `1920` | Target output width; sets the column count from the font's cell width |
| `--cols` | | Number of character columns (overrides `--width`) |
| `--font` | `fc-match monospace` | Monospace font file |
| `--font-size` | `10` | Font size in px. Smaller is denser |
| `--bold` | | Thicken glyph strokes by a pixel for brighter, punchier output |
| `--charset` | `wd2` | `wd2`, `classic`, `binary`, `blocks`, `dense` |
| `--ramp` | | Custom character ramp, e.g. `" .:01#"` |
| `--no-sort` | | Keep the ramp order and spread it evenly, instead of sorting and matching glyphs by ink density |
| `--fg` / `--bg` | `#ffffff` / `#000000` | Glyph and background colours |
| `--color` | | Colour glyphs with the source pixel colour |
| `--shade` | `0.3` | How much glyph brightness follows luminance (0 = flat, 1 = full) |
| `--saturation` | `1.5` | Colour saturation multiplier for `--color` |
| `--bg-fill` | `0.3` with `--color`, else `0` | Tint each cell's background with its glyph colour (0 = none, 1 = solid) |
| `--contrast` / `--brightness` / `--gamma` | `1` / `0` / `1` | Luminance adjustments (`gamma < 1` lifts shadows) |
| `--invert` | `auto` | `auto`, `on` or `off`. `auto` inverts light-background images and GIFs in mono mode so the background stays dark |
| `--no-auto-levels` | | Don't stretch each frame's luminance to use the full ramp |
| `--equalize` | `0.5` | How much histogram equalization auto-levels mixes in (0 = plain stretch, 1 = full) |
| `--sharpen` | `0` | 0–5: sharpen the downscaled image so edges survive as glyphs |
| `--jitter` | `0` | 0–1: per-cell luminance noise each frame (character shimmer) |
| `--glitch` | `0` | 0–1: band tearing, block dropouts, scrambles, vertical rolls, bursts |
| `--seed` | `1337` | Seed for jitter and glitches |

Jitter and glitches depend only on the seed and the frame's position in the source, so a `--start` slice looks exactly like the same stretch of a full render.

### Run

| Option | Default | Description |
|---|---|---|
| `--preset` | | Built-in preset name or path to a TOML file |
| `--print-preset` | | Print the effective look settings as TOML and exit |
| `--start` / `--duration` | | Time range, e.g. `48`, `0:48`, `1:02:03.5` |
| `--crf` | `18` | Encoder quality, lower is better (x264 0–51, VP9 0–63) |
| `--preview` | | Play in an ffplay window instead of writing a file |
| `--no-loop` | | Play GIFs and stills once in `--preview` instead of looping until the window is closed |
| `--gpu` | | Composite frames on the GPU instead of the CPU |

## Development

`cargo test` runs the unit tests. Font-dependent tests skip without fontconfig. The GPU test compares the wgpu renderer against the CPU one, and skips without a GPU adapter.

The code is a library ([`src/lib.rs`](src/lib.rs) describes the pipeline) with a thin CLI in `src/main.rs`.

## License

MIT
