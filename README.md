# sp1200-rs

A native **Rust + SDL2** port of the [sp1200](https://github.com/sefuwunder/sp1200)
12-bit drum machine — a playable sibling of the web version, not a replacement.
The web app stays untouched; this is the same instrument compiled to a desktop
binary.

What it is: 8 synthesized drums through a 26.04 kHz / 12-bit signal path with
varispeed tuning, Roger Linn swing, an 8–64-step sequencer, tape loading with
onset/equal splicing (chop-to-pads), JSON project save/load (plus web
`.sp1200.json` import), and an SP-1200-style 2D faceplate. Dependencies are
`sdl2` (windowing/audio), `serde`/`serde_json` (project files — a pragmatic
call; base64 is hand-rolled in `project.rs`, ~30 lines, to avoid another
crate); the DSP core uses no crates at all (deterministic XorShift32 RNG, no
`rand`).

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
| `-` / `=` | Sequence length −8 / +8 steps (8–64; steps past the length are kept) |
| `[` / `]` | Step page back / forward (16 steps per page) |
| `Space` | Play / stop |
| `T` | Tap tempo (average of taps in the last 2 s) |
| `,` / `.` | BPM −1 / +1 |
| `←` / `→` | Swing −1% / +1% (Roger Linn 50–75%) |
| `↑` / `↓` | Master level |
| `F1` `F2` `F3` | Factory presets: BOOM BAP / HOUSE / LOPE |
| `S` | Re-splice the tape (onset detection) |
| `E` | Slice the tape into 8 equal parts |
| `D` | Clear the tape (pads revert to drums) |
| `W` | Save the project as JSON (to the loaded path, else `./sp1200-project.json`) |
| Drop a `.wav` | Load it as the tape (through the 12-bit path, auto-spliced) |
| Drop a `.json` | Load a project (`.sp1200.json` = web import, otherwise native) |
| `Tab` | Toggle the sample editor for the selected pad |
| `P` | Panic — kill all sounding voices immediately |
| `K` | Lock/unlock the selected pad: locked pads keep their sample while tapes are loaded, spliced, or cleared (a small square marks them) |
| `Esc` / `Q` | Quit |
| `:` | Vim-style command prompt (`:help` lists commands; `Esc` cancels) |

## Sample editor (`Tab`)

The editor works on the selected pad's full source sample (drum, tape chop,
or loaded WAV). Edits are non-destructive except normalize.

| Input | Action |
|---|---|
| `Z` / `X` | Trim start −/+ |
| `C` / `V` | Trim end −/+ |
| `R` | Reverse the trimmed region |
| `N` | Normalize source to 0.92 peak (destructive) |
| `B` | Browse `./samples/` (or `--samples-dir`) — `Enter` loads the WAV onto the pad through the 12-bit path |
| `G` | Cycle choke group: `--` → `G1` → `G2` → `G3` → `G4` (pads in a group cut each other off, like open/closed hats) |
| `F` | Cycle per-pad filter: `OFF` → `LP` → `HP` (resonant biquad, runs per voice) |
| `↑` / `↓` | Filter cutoff, 80 Hz – 12 kHz |
| `-` / `=` | Filter resonance (Q), 0.1 – 12 |
| `M` | Mono/poly per pad: mono retriggering cuts the pad's own ringing voices |
| `[` / `]` | Pad pitch −/+ 1 semitone (±24 st) |
| `L` | Toggle pad looping: the voice plays the sample once, then repeats the loop region forever |
| `Shift+Z` / `Shift+X` | Loop start −/+ (same gesture as trim, shifted) |
| `Shift+C` / `Shift+V` | Loop end −/+ |

Loop points are fractions of the pad's playable sample (so they survive trim/reverse edits), set exactly with `:loop START_S END_S`, and saved into native projects.

### 4-track tape

`A` opens the 4-track page: four mono tracks for recording live input
(the default audio capture device — mic/line-in) or bouncing the pattern
mix. Each track has `A` arm, `I` input/master source, `M` mute, `X` clear;
`1-4` selects the track. `ENTER` plays/stops the tape, `R` toggles
recording on armed tracks (starting playback). `U` is the same
record/stop button on every page: it records only to the track(s)
you've armed with `A`, and stopping keeps every take (tracks stay
armed). `:rec all` still arms all four at once. `S` stops and rewinds.
`SPACE` still runs the pattern sequencer, so bouncing is: arm a track with
`MASTER` source, hit `R`, hit `SPACE`, hit `R` again. Recording is
punch-in style — the armed region is replaced, and armed tracks aren't
played back while recording (no feedback). The header shows `IN OK` /
`IN NONE` for the capture device. Track audio and settings are saved into
native projects.

Tape-bus character: `WARMTH` (tube-style saturation, 0–100%) and
`DISTORT` (tape-style saturation, 0–100%) shape the tape playback and
monitor mix only — takes always stay clean, so the character is
non-destructive. Set them exactly with `:warmth N` / `:distort N`, or
nudge in 5% steps on the tape page with `[` / `]` (warmth) and
Shift+`[` / Shift+`]` (distortion); both values show under the tape
header. Saved into native projects.

Trim is a non-destructive playback window per track: `,` / `.` nudge the
selected track's trim start, `;` / `'` its trim end, in 0.1 s steps — a
trimmed track stays silent outside its window, and trimming never deletes
audio. `B` resamples the selected track's trimmed region onto the
same-number pad through the 12-bit path (track 1 → pad 1), printing
the current tape-bus warmth/distortion into the bounce; the pad keeps
the resample as its custom sound, so `W` saves it and tape-clear restores
it.

### Command prompt (`:`)

`:` opens a vim-style command line on any page: type the command,
`Enter` runs it, `Esc` cancels (never quits), `Backspace` edits.

| Command | Action |
|---|---|
| `:help` | List commands (one line; this file has the details) |
| `:w` / `:q` | Save the project / quit |
| `:bpm N` / `:swing N` | Set BPM (0–256; 0 parks the sequencer) / swing (50–75%) |
| `:steps N` | Set sequence length, 8–64 steps (clamped; exact values, unlike `-`/`=`) |
| `:pad N` / `:track N` | Select pad (1–8) / 4-track track (1–4) |
| `:chop [PAD] CHOP` / `:chop [PAD] CLEAR` | Assign any tape chop to a pad (both 1-based; one-arg form targets the selected pad) / restore the pad's default chop or drum sample |
| `:loop` | Toggle looping on the selected pad |
| `:loop on` / `:loop off` | Enable / disable looping |
| `:loop START_S END_S` | Set loop points in seconds and enable looping |
| `:ttrim START_S END_S` / `:ttrim full` | Set / clear the selected track's trim window |
| `:resample [TRACK] [PAD]` | Resample a track's trimmed region onto a pad (defaults: selected track → same-number pad) |
| `:rec` / `:rec all` | Record armed tracks / record all four tracks |
| `:stop` / `:rewind` / `:play` | Stop (keep playhead) / stop + rewind / play-stop the tape |

`sp1200-rs --pad 3 kick.wav` loads a WAV onto pad 3 at startup. Choke
groups, filters (cutoff + resonance), mono/poly, pitch, trim, reverse, and user samples are saved into native
projects (`W`); user samples are embedded as base64, so the project is
self-contained.

## Tape & splicing

`sp1200-rs --tape break.wav` (or drop the file onto the window) loads a tape:
16-bit PCM or 32-bit float WAV, mono or stereo (mixed down), any sample rate —
decoded by a small zero-dependency reader in `src/wav.rs` — or MP3 (decoded
by minimp3, ID3 tags skipped) — normalized like the web
(`normalize(clean, 0.92)`), then run through the SP-1200 converters
(resample to 26.04 kHz + 12-bit quantize). The sample browser, `--pad`, and
drag-and-drop all accept `.wav` and `.mp3`.

On load the tape is auto-spliced by onset detection; `S` re-splices, `E`
slices into 8 equal parts, `D` clears it. Pads 1–8 trigger chops 1–8 (more
than 8 chops: the first 8; the readout shows `8/N CHOPS`). Any chop can
be assigned to any pad with `:chop [PAD] CHOP` — `:chop 12` puts chop 12
on the selected pad, `:chop 3 12` puts it on pad 3, `:chop 3 clear`
restores pad 3's default. Assignments are by chop number, so they
survive a re-splice while that chop still exists; a fresh tape clears
them for unlocked pads, and an explicit assignment overrides a pad lock
the way an explicit file load does. Assignments save into native
projects. Per-pad tune still
applies — varispeed on chops, the classic. The step sequencer keeps working
and fires chops like drums.

## Projects

`W` saves the native JSON format (versioned, `format: "sp1200-rs"`): name,
bpm, swing, master, per-pad tune/level (and optional per-pad chop override),
per-pad filter (type + cutoff), choke group, trim, reverse, label, and any
user-loaded sample (embedded base64, so the project is self-contained),
the full pattern, and tape name + chop markers. Tape *audio* is not
embedded — load the project, then drop the `.wav`, and the saved chops apply
to it. `sp1200-rs --project song.json` loads at startup; a dropped `.json`
also sets the save path for `W`.

**Web import:** files named `*.sp1200.json` are read as the browser app's
project format (`serializeProject`): bpm/swing/master, per-pad tune (ratio →
semitones) / level / label, custom pad samples (base64 pcm16 → the 12-bit
path), and `tapes[0]` audio (base64 pcm16 stereo → mono → the 12-bit path).
Filter/delay/loop/voice-mode/choke settings have no equivalent here and are
ignored. Bad files (malformed JSON, wrong version, missing fields) show a
clean on-screen error and leave the current state untouched.

## CLI

```sh
sp1200-rs --render out.wav [seconds]   # render the fixed demo pattern to WAV
sp1200-rs --shot out.bmp               # render one UI frame offscreen to BMP
sp1200-rs --tape f.wav --shot out.bmp  # offscreen shot with a tape loaded
sp1200-rs --project f.json             # load a project at startup
sp1200-rs --pad 2 kick.wav             # load a WAV onto pad 2 at startup
sp1200-rs --samples-dir ./my-wavs      # sample browser directory
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
  8–64-step per-pad sequencer with BPM + swing, live triggering alongside the
  sequencer, 3 factory presets (16-step; loading one resets the length to 16), tape state (name, 12-bit audio, chop regions;
  pads 1–8 play chops, `D` restores the drum factory), and the exact-integer
  offline renderer used by `--render`.
- `src/wav.rs` — minimal zero-dependency WAV decoder (16-bit PCM / 32-bit
  float, mono-downmixed, any sane rate); every malformed input is a clean
  `WavError`, never a panic.
- `src/project.rs` — native JSON projects (versioned; kit + pattern + tape
  chop markers) and web `.sp1200.json` import (bpm/swing/master, per-pad
  tune/level/label, custom pad samples, `tapes[0]` audio — all through the
  12-bit path). Base64 is hand-rolled; JSON via `serde_json`.
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

Playable core plus tape/splice/project loading (this release). Deliberately
out of scope (future work): per-pad filters and delay, KO II / EP-133 skins
from the web app. The ported DSP helpers the UI doesn't use yet
(`trim_sample`, `reverse_sample`, `fade_sample`, `sp1200ize`) are kept as
tested API surface for the sample editor to come.
