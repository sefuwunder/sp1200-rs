# sp1200-rs

A native **Rust + SDL2** port of the [sp1200](https://github.com/sefuwunder/sp1200)
12-bit drum machine — a playable sibling of the web version, not a replacement.
The web app stays untouched; this is the same instrument compiled to a desktop
binary.

What it is: 8 synthesized drums through a 26.04 kHz / 12-bit signal path with
varispeed tuning, Roger Linn swing, a 16-step sequencer, and an SP-1200-style
2D faceplate. Zero Rust dependencies beyond `sdl2` for windowing/audio; the DSP
core uses no crates at all (deterministic XorShift32 RNG, no `rand`).

## Build

```sh
# system dependency: SDL2 development headers
sudo apt-get install -y libsdl2-dev

# Rust toolchain (rustup)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

cargo build --release
./target/release/sp1200-rs
```

`cargo build` and `cargo test` need no display or audio hardware. The binary
links SDL2 dynamically (`libsdl2-dev` at build time, `libsdl2-2.0-0` at run
time).

## Controls

| Input | Action |
|---|---|
| `1`–`8` / click pad | Trigger pad (also selects it) |
| Click step button | Toggle that step for the selected pad |
| `Space` | Play / stop |
| `T` | Tap tempo (average of taps in the last 2 s) |
| `,` / `.` | BPM −1 / +1 |
| `←` / `→` | Swing −1% / +1% (Roger Linn 50–75%) |
| `↑` / `↓` | Master level |
| `F1` `F2` `F3` | Factory presets: BOOM BAP / HOUSE / LOPE |
| `Esc` / `Q` | Quit |

## CLI

```sh
sp1200-rs --render out.wav [seconds]   # render the fixed demo pattern to WAV
sp1200-rs --shot out.bmp               # render one UI frame offscreen to BMP
sp1200-rs --smoke [secs]               # run the UI headless for N s, exit 0
sp1200-rs --help
```

`--render` is deterministic: same seed, same drums, same pattern, same bytes,
every run. It is the artifact the parity test compares.

## Layout

- `src/dsp.rs` — faithful port of the web app's `public/dsp.js` (318 lines):
  `SP_RATE = 26040`, 12-bit quantization (scale 2047), linear resampling, all
  8 drum synths, swing timing (`sixteenth_dur`, `step_time16`), trim / reverse /
  fade, onset detection. Every synth takes `&mut dyn Rng`; the RNG is a tiny
  `XorShift32` with an explicit seed — no `rand` crate, per the zero-dep
  spirit.
- `src/engine.rs` — 8 pads (i16 samples @ 26.04 kHz), per-pad tune (varispeed
  ratio) and level, 16-voice polyphonic allocator with oldest-voice stealing,
  16-step per-pad sequencer with BPM + swing, live triggering alongside the
  sequencer, 3 factory presets, and the exact-integer offline renderer used by
  `--render`.
- `src/audio.rs` — SDL2 audio callback. The engine renders at 26.04 kHz; the
  callback linearly resamples to the device rate (44100 Hz requested) through
  a small persistent ring buffer.
- `src/ui.rs` — SP-1200-style 2D faceplate (dark plate, warm orange accents):
  8 pads, 16 step LEDs with playhead, BPM / swing / master readouts, tap
  tempo, preset row. Text is a built-in 5×7 bitmap font — no sdl2_ttf, no
  extra system dependencies. Drawing goes through a `Painter` trait so the
  same code paints the window canvas and the offscreen `--shot` surface.
- `src/main.rs` — CLI, 16-bit PCM WAV writer, SDL bootstrap, 60 fps main loop.

## Parity with the web core

The DSP port is verified **bit-for-bit** against the shipped `dsp.js`:

1. `./target/release/sp1200-rs --render /tmp/parity-rs.wav`
2. Render the identical 2-bar demo in JS with `bun`: a small harness
   monkey-patches `Math.random` to the same XorShift32 algorithm + seed
   (`0x12345678`) *before* requiring the unmodified `dsp.js`, synthesizes the
   8 drums in the same order, and mixes with the same step times
   (`Math.floor(t*26040 + 0.5)` offsets, i32 accumulation, saturating cast).
3. `cmp /tmp/parity-rs.wav /tmp/parity-js.wav`

Result: **byte-identical** — 127,596 samples, 0 LSB difference (the allowance
was ≤1 LSB for `exp`/`sin` libm variance; it wasn't needed).

Parity holds because the port replicates JS semantics pedantically: f32
storage with f64 intermediates on every read-modify-write (matters in the
kick/tom click transients and hat/shaker envelopes), JS `Math.round`
(halves toward +∞ — Rust's `.round()` differs on negatives), and the exact
XorShift32 bit ops with the seed used verbatim as state.

## v1 scope

Playable core only. Deliberately out of scope (future work): loading/slicing
sample files, per-pad filters and delay, project save/load, KO II / EP-133
skins from the web app. The ported DSP helpers that the UI doesn't use yet
(`trim_sample`, `reverse_sample`, `fade_sample`, `detect_onsets`,
`sp1200ize`) are kept as tested API surface for the sample editor to come.
