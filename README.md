# asciivid

Render any video as ASCII art into a new video file, with optional DedSec-style glitches. Think the *Watch Dogs 2* main menu.

ffmpeg decodes each frame and scales it down to the character grid. Each cell's luminance picks a glyph from a ramp, which is sorted by ink density automatically. The glyphs are rasterized from a real monospace font, glitch passes are applied, and the result is piped back into ffmpeg (H.264, original audio kept).

## Install

Requires `ffmpeg` (with `ffprobe`; `ffplay` for `--preview`) and `fontconfig`.

```sh
cargo install --git https://github.com/KemonoNeco/asciivid
```

Make sure `~/.cargo/bin` is on your `PATH` (fish: `fish_add_path ~/.cargo/bin`).

## Usage

```sh
# Watch Dogs-ish: white on black, shimmering glyphs, glitches
asciivid in.mp4 out.mp4 --glitch 0.5 --jitter 0.15 --contrast 1.4

# Closer to the WD2 menu: tiny binary glyphs, crushed shadows
asciivid in.mp4 out.mp4 --font-size 8 --charset binary --contrast 1.6 --gamma 1.3

# Live preview instead of rendering
asciivid in.mp4 --preview --glitch 0.5

# Only render a slice
asciivid in.mp4 clip.mp4 --start 0:48 --duration 10

# YouTube: download first
yt-dlp -f 'bv*[height<=1080]+ba' -o src.mp4 URL && asciivid src.mp4 out.mp4
```

## Options

| Option | Default | Description |
|---|---|---|
| `--width` | `1920` | Target output width; sets the column count from the font's cell width |
| `--cols` | | Number of character columns (overrides `--width`) |
| `--font` | `fc-match monospace` | Monospace font file |
| `--font-size` | `10` | Font size in px. Smaller is denser |
| `--charset` | `wd2` | `wd2`, `classic`, `binary`, `blocks`, `dense` |
| `--ramp` | | Custom character ramp, e.g. `" .:01#"` |
| `--no-sort` | | Keep the ramp order instead of sorting by ink density |
| `--fg` / `--bg` | `#ffffff` / `#000000` | Glyph and background colours |
| `--color` | | Colour glyphs with the source pixel colour |
| `--shade` | `0.6` | How much glyph brightness follows luminance (0 = flat, 1 = full) |
| `--contrast` / `--brightness` / `--gamma` | `1` / `0` / `1` | Luminance adjustments (`gamma < 1` lifts shadows) |
| `--invert` | | Invert luminance |
| `--jitter` | `0` | Per-cell luminance noise each frame (character shimmer) |
| `--glitch` | `0` | 0–1: band tearing, block dropouts, scrambles, vertical rolls, bursts |
| `--seed` | `1337` | RNG seed for jitter and glitches |
| `--start` / `--duration` | | Passed to ffmpeg as `-ss` / `-t` |
| `--crf` | `18` | x264 quality (lower is better) |
| `--preview` | | Play in an ffplay window instead of writing a file |

## License

MIT
