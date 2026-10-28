//! Playback engine: 8 pads, polyphonic voices, 16-step sequencer, mixer.
//!
//! All samples are i16 @ 26.04 kHz (the SP path). The real-time mixer works
//! in f32; the offline `--render` path uses exact integer accumulation so it
//! can be byte-compared against the JS reference.

use crate::dsp::{self, js_round, FilterKind, XorShift32, DRUM_NAMES, SP_RATE};
use crate::fourtrack::{FourTrack, TrackSource};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

pub const N_PADS: usize = 8;
/// Step storage: every pattern holds up to 64 steps; `seq_len` (8..=64)
/// selects how many actually play. Steps past the length are kept, so
/// shortening then lengthening restores them.
pub const MAX_STEPS: usize = 64;
pub const MIN_STEPS: usize = 8;
/// Steps shown per row on the Pads page; longer sequences page.
pub const STEPS_PER_PAGE: usize = 16;
pub const N_VOICES: usize = 16;

/// Choke groups: 0 = none, 1..=4 = mute each other on trigger.
pub const N_CHOKE_GROUPS: u8 = 4;

/// Fixed seed so the instrument sounds identical on every launch.
pub const DRUM_SEED: u32 = 0x1200_1987;

/// Fixed seed for the `--render` parity demo.
pub const RENDER_SEED: u32 = 0x12_345678;

/// Default filter cutoff when a filter is first engaged.
pub const FILTER_DEFAULT_CUTOFF: f32 = 3000.0;

/// BPM range: 0 parks the sequencer (manual pad triggers still sound),
/// 256 is the top. Every clamp in the UI and project loaders uses these
/// so the range can't drift apart again.
pub const BPM_MIN: f64 = 0.0;
pub const BPM_MAX: f64 = 256.0;

pub struct Pad {
    /// Playable sample: trim + reverse applied to `source`.
    pub sample: Vec<i16>,
    /// Full sample (drum synth, tape chop, or loaded WAV).
    pub source: Vec<i16>,
    pub tune_st: f32,
    pub level: f32,
    /// Optional display label (from web project import); falls back to the
    /// drum name, or CHOP n in tape mode.
    pub label: Option<String>,
    /// True when the source is a user sample (loaded WAV / web import):
    /// saved into native projects as base64.
    pub custom: bool,
    pub filter_kind: FilterKind,
    pub filter_cutoff: f32,
    /// Filter resonance (Q), 0.1..12.
    pub filter_res: f32,
    /// Precomputed biquad coefficients (b0,b1,b2 / a1,a2), refreshed by
    /// `set_filter` / `set_res`.
    pub fb: [f32; 3],
    pub fa: [f32; 2],
    /// Mono: retriggering the pad cuts its own ringing voices.
    pub mono: bool,
    pub choke: u8,
    /// Non-destructive edit region, fractions of `source`.
    pub trim: (f32, f32),
    pub reverse: bool,
    /// Loop region, fractions of the playable `sample` (not `source`, so
    /// it survives trim/rebuild): a looping voice plays the sample from
    /// the start, then repeats [loop_pts.0, loop_pts.1) forever.
    pub loop_pts: (f32, f32),
    pub loop_enabled: bool,
    /// Pinned: tape load/splice/clear won't replace this pad's sample.
    /// Explicit per-pad actions (browser load, chop assign, edits)
    /// still apply.
    pub locked: bool,
}

impl Pad {
    fn fresh(sample: Vec<i16>) -> Self {
        let (fb, fa) = dsp::biquad_coeffs(FilterKind::Off, FILTER_DEFAULT_CUTOFF, 0.71, SP_RATE);
        Self {
            sample: sample.clone(),
            source: sample,
            tune_st: 0.0,
            level: 0.9,
            label: None,
            custom: false,
            filter_kind: FilterKind::Off,
            filter_cutoff: FILTER_DEFAULT_CUTOFF,
            filter_res: 0.71,
            fb,
            fa,
            mono: false,
            choke: 0,
            trim: (0.0, 1.0),
            reverse: false,
            loop_pts: (0.0, 1.0),
            loop_enabled: false,
            locked: false,
        }
    }

    /// Varispeed replay ratio, like the hardware: tuning replays the sample
    /// faster or slower.
    pub fn ratio(&self) -> f64 {
        2f64.powf(self.tune_st as f64 / 12.0)
    }

    /// Rebuild the playable sample from source + trim + reverse.
    pub fn rebuild(&mut self) {
        let n = self.source.len();
        if n == 0 {
            self.sample.clear();
            return;
        }
        let (ts, te) = self.trim;
        let a = (ts.clamp(0.0, 1.0) * n as f32) as usize;
        let b = (te.clamp(0.0, 1.0) * n as f32) as usize;
        let (a, b) = (a.min(n), b.min(n));
        let mut s: Vec<i16> = if b > a {
            self.source[a..b].to_vec()
        } else {
            Vec::new()
        };
        if self.reverse {
            s.reverse();
        }
        self.sample = s;
    }

    /// Install a new source sample: trim/reverse reset, playable rebuilt.
    pub fn set_source(&mut self, source: Vec<i16>, label: Option<String>) {
        self.source = source;
        self.trim = (0.0, 1.0);
        self.reverse = false;
        self.label = label;
        self.rebuild();
    }

    /// Recompute biquad coefficients from kind + cutoff + resonance.
    pub fn refresh_filter(&mut self) {
        let (fb, fa) = dsp::biquad_coeffs(
            self.filter_kind,
            self.filter_cutoff,
            self.filter_res,
            SP_RATE,
        );
        self.fb = fb;
        self.fa = fa;
    }

    /// Loop region in playable-sample indices, or None when looping is
    /// off or the sample is too short. Always valid for interpolation:
    /// loop_end <= len - 1, so idx + 1 never runs past the sample.
    pub fn loop_range(&self) -> Option<(usize, usize)> {
        if !self.loop_enabled {
            return None;
        }
        let n = self.sample.len();
        if n < 2 {
            return None;
        }
        let a = (self.loop_pts.0.clamp(0.0, 1.0) * n as f32) as usize;
        let a = a.min(n - 2);
        let b = (self.loop_pts.1.clamp(0.0, 1.0) * n as f32) as usize;
        let b = b.clamp(a + 1, n - 1);
        Some((a, b))
    }
}

/// The 8 drum voices, in pad order.
pub const DRUM_LABELS: [&str; N_PADS] = [
    "KICK", "SNARE", "CLAP", "RIM", "CHAT", "OHAT", "TOM", "SHAKER",
];

/// A loaded tape: 12-bit audio @ 26.04 kHz plus chop regions (sample
/// offsets into `audio`). Empty `audio` means "markers without audio"
/// (restored from a project file before the .wav is dropped).
pub struct Tape {
    pub name: String,
    pub audio: Vec<i16>,
    pub chops: Vec<(usize, usize)>,
}

#[derive(Clone, Copy)]
struct Voice {
    active: bool,
    pad: usize,
    pos: f64,
    age: u64,
    /// Biquad state [x1, x2, y1, y2] for the pad's filter.
    fstate: [f32; 4],
}

impl Voice {
    const fn idle() -> Self {
        Self {
            active: false,
            pad: 0,
            pos: 0.0,
            age: 0,
            fstate: [0.0; 4],
        }
    }
}

pub struct Engine {
    pub pads: Vec<Pad>,
    voices: [Voice; N_VOICES],
    pub patterns: [[bool; MAX_STEPS]; N_PADS],
    /// Playing sequence length, 8..=64 steps (default 16).
    pub seq_len: usize,
    pub bpm: f64,
    pub swing: f64,
    pub master: f32,
    pub playing: bool,
    clock: u64,
    sched_step: u64,
    pattern_start: u64,
    next_step_at: u64,
    voice_age: u64,
    /// Engine-clock of the last trigger per pad (for UI hit flashes).
    pub pad_hit_at: [u64; N_PADS],
    /// Last fired step within the bar (for the playhead LEDs).
    pub last_step: u32,
    /// Loaded tape (if any). Pads 1-8 trigger chops 1-8 while audio is present.
    pub tape: Option<Tape>,
    /// Which chop each pad plays (None = drum). 1:1 by default; a native
    /// project file may override individual pads.
    pub(crate) chop_map: [Option<usize>; N_PADS],
    /// Drum-mode samples (synths, or web-imported customs): what pads
    /// revert to when the tape is cleared.
    pub(crate) pad_factory: Vec<Vec<i16>>,
    /// 4-track tape recorder (live input / pattern bounces).
    pub four_track: FourTrack,
    /// Live-input capture queue (None when no input device was found).
    capture: Option<CaptureInput>,
    /// Fractional resample position into the capture queue.
    cap_phase: f64,
}

/// Live audio input from the SDL2 capture device.
struct CaptureInput {
    queue: Arc<Mutex<VecDeque<f32>>>,
    rate: f64,
}

impl Engine {
    pub fn new() -> Self {
        let mut rng = XorShift32::new(DRUM_SEED);
        let pads: Vec<Pad> = DRUM_NAMES
            .iter()
            .map(|name| Pad::fresh(dsp::synth_drum_i16(name, &mut rng)))
            .collect();
        let pad_factory = pads
            .iter()
            .map(|p: &Pad| p.sample.clone())
            .collect::<Vec<_>>();
        let mut e = Self {
            pads,
            voices: [Voice::idle(); N_VOICES],
            patterns: [[false; MAX_STEPS]; N_PADS],
            seq_len: 16,
            bpm: 100.0,
            swing: 60.0,
            master: 0.8,
            playing: false,
            clock: 0,
            sched_step: 0,
            pattern_start: 0,
            next_step_at: 0,
            voice_age: 0,
            pad_hit_at: [0; N_PADS],
            last_step: 0,
            tape: None,
            chop_map: [None; N_PADS],
            pad_factory,
            four_track: FourTrack::new(),
            capture: None,
            cap_phase: 0.0,
        };
        e.apply_preset(0);
        e
    }

    pub fn trigger(&mut self, pad: usize) {
        if pad >= N_PADS || self.pads[pad].sample.is_empty() {
            return;
        }
        // Mono: retriggering the pad cuts its own ringing voices.
        if self.pads[pad].mono {
            for v in self.voices.iter_mut() {
                if v.active && v.pad == pad {
                    v.active = false;
                }
            }
        }
        // Choke: pads sharing a group cut each other's voices.
        let g = self.pads[pad].choke;
        if g != 0 {
            for v in self.voices.iter_mut() {
                if v.active && v.pad != pad && self.pads[v.pad].choke == g {
                    v.active = false;
                }
            }
        }
        // Steal the oldest voice if all are busy.
        let mut slot = None;
        let mut oldest = u64::MAX;
        let mut oldest_idx = 0;
        for (i, v) in self.voices.iter().enumerate() {
            if !v.active {
                slot = Some(i);
                break;
            }
            if v.age < oldest {
                oldest = v.age;
                oldest_idx = i;
            }
        }
        let i = slot.unwrap_or(oldest_idx);
        self.voice_age += 1;
        self.voices[i] = Voice {
            active: true,
            pad,
            pos: 0.0,
            age: self.voice_age,
            fstate: [0.0; 4],
        };
        self.pad_hit_at[pad] = self.clock;
    }

    /// Panic: silence every voice immediately. The transport keeps its
    /// state — the next scheduled step will sound again.
    pub fn panic(&mut self) {
        for v in self.voices.iter_mut() {
            v.active = false;
        }
    }

    /// Number of currently sounding voices (tests).
    #[cfg(test)]
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    /// Pin/unpin a pad against tape load/splice/clear.
    pub fn set_locked(&mut self, i: usize, locked: bool) {
        if i < N_PADS {
            self.pads[i].locked = locked;
        }
    }

    pub fn play(&mut self) {
        self.playing = true;
        self.pattern_start = self.clock;
        self.sched_step = 0;
        self.next_step_at = self.clock;
    }

    pub fn stop(&mut self) {
        self.playing = false;
        self.sched_step = 0;
        for v in self.voices.iter_mut() {
            v.active = false;
        }
    }

    // ---------- tape ----------

    /// Load tape audio (12-bit @ 26.04 kHz): auto-splice via onset
    /// detection, pads 1-8 take chops 1-8. If chop markers were restored
    /// from a project file (tape with no audio yet), the dropped tape
    /// inherits them instead of being re-spliced. Returns the chop count.
    pub fn load_tape(&mut self, name: String, audio: Vec<i16>) -> usize {
        let marked = match &self.tape {
            Some(t) if t.audio.is_empty() && !t.chops.is_empty() => Some(t.chops.clone()),
            _ => None,
        };
        let chops = if audio.is_empty() {
            Vec::new()
        } else {
            let fallback = || {
                let mut c = dsp::onset_chops(&audio, SP_RATE);
                if c.is_empty() {
                    c = vec![(0, audio.len())];
                }
                c
            };
            match marked {
                Some(m) => {
                    let n = audio.len();
                    let v: Vec<(usize, usize)> = m
                        .into_iter()
                        .map(|(a, b)| (a.min(n), b.min(n)))
                        .filter(|&(a, b)| b > a)
                        .collect();
                    if v.is_empty() {
                        fallback()
                    } else {
                        v
                    }
                }
                None => fallback(),
            }
        };
        for i in 0..N_PADS {
            if !self.pads[i].locked {
                self.chop_map[i] = None;
            }
        }
        self.tape = Some(Tape { name, audio, chops });
        self.refresh_pad_samples();
        self.tape.as_ref().map(|t| t.chops.len()).unwrap_or(0)
    }

    /// Re-splice the loaded tape with onset detection. None if no tape.
    pub fn splice_onset(&mut self) -> Option<usize> {
        let chops = {
            let t = self.tape.as_ref()?;
            if t.audio.is_empty() {
                return None;
            }
            let mut c = dsp::onset_chops(&t.audio, SP_RATE);
            if c.is_empty() {
                c = vec![(0, t.audio.len())];
            }
            c
        };
        self.tape.as_mut().unwrap().chops = chops;
        self.refresh_pad_samples();
        Some(self.tape.as_ref().unwrap().chops.len())
    }

    /// Slice the loaded tape into 8 equal parts. None if no tape.
    pub fn splice_equal(&mut self) -> Option<usize> {
        let chops = {
            let t = self.tape.as_ref()?;
            if t.audio.is_empty() {
                return None;
            }
            dsp::equal_chops(t.audio.len())
        };
        self.tape.as_mut().unwrap().chops = chops;
        self.refresh_pad_samples();
        Some(self.tape.as_ref().unwrap().chops.len())
    }

    /// Clear the tape: pads revert to their drum-mode samples.
    pub fn clear_tape(&mut self) {
        self.tape = None;
        self.refresh_pad_samples();
    }

    /// Assign any tape chop to any pad (or None to restore the pad's
    /// default: its own chop, else its drum-mode sample). Explicit, so
    /// it replaces even a locked pad's sample — like an explicit file
    /// load — and trim/reverse edits stay applied on top. Assignments
    /// are by chop number: they survive a re-splice while that chop
    /// still exists, and a fresh tape clears them for unlocked pads.
    /// Returns a status line, or an error message.
    pub fn assign_chop(&mut self, pad: usize, chop: Option<usize>) -> Result<String, String> {
        if pad >= N_PADS {
            return Err("BAD PAD".to_string());
        }
        let (audio, chops) = match &self.tape {
            Some(t) if !t.audio.is_empty() => (t.audio.clone(), t.chops.clone()),
            _ => return Err("NO TAPE".to_string()),
        };
        if let Some(c) = chop {
            if c >= chops.len() {
                return Err(format!(
                    "CHOP {} OUT OF RANGE ({} CHOPS)",
                    c + 1,
                    chops.len()
                ));
            }
        }
        let (sample, m) = match chop {
            Some(c) => {
                let (a, b) = chops[c];
                (audio[a..b].to_vec(), Some(c))
            }
            None => {
                if pad < chops.len() {
                    let (a, b) = chops[pad];
                    (audio[a..b].to_vec(), Some(pad))
                } else {
                    (self.pad_factory[pad].clone(), None)
                }
            }
        };
        self.chop_map[pad] = m;
        self.pads[pad].source = sample;
        self.pads[pad].rebuild();
        Ok(format!("PAD {} <- {}", pad + 1, self.pad_label(pad)))
    }

    /// Re-derive pad samples from the tape + chop map. Pads without a chop
    /// (or with no tape audio) play their drum-mode sample. Trim/reverse
    /// edits apply on top of whatever source lands here.
    pub(crate) fn refresh_pad_samples(&mut self) {
        let resolved: Vec<(Vec<i16>, Option<usize>)> = (0..N_PADS)
            .map(|i| {
                if let Some(t) = &self.tape {
                    if !t.audio.is_empty() {
                        let ci = match self.chop_map[i] {
                            Some(c) if c < t.chops.len() => c,
                            _ => i,
                        };
                        if ci < t.chops.len() {
                            let (a, b) = t.chops[ci];
                            return (t.audio[a..b].to_vec(), Some(ci));
                        }
                    }
                }
                (self.pad_factory[i].clone(), None)
            })
            .collect();
        for (i, (s, m)) in resolved.into_iter().enumerate() {
            if self.pads[i].locked {
                continue;
            }
            self.pads[i].source = s;
            self.pads[i].rebuild();
            self.chop_map[i] = m;
        }
    }

    /// Install a custom drum-mode sample (web project import): it becomes
    /// the pad's drum-mode sound and what `clear_tape` restores. A pad
    /// currently playing a chop keeps the chop until the next splice.
    pub(crate) fn set_custom_pad(&mut self, i: usize, sample: Vec<i16>, label: Option<String>) {
        if i >= N_PADS {
            return;
        }
        self.pad_factory[i] = sample.clone();
        self.pads[i].custom = true;
        if self.chop_map[i].is_none() {
            // Fresh source: trim/reverse reset; project load re-applies
            // saved edits afterwards.
            self.pads[i].set_source(sample, label);
        } else {
            self.pads[i].label = label;
        }
    }

    // ---------- per-pad sound design ----------

    /// Load a WAV/MP3 file onto a pad: decode → normalize → the 12-bit path,
    /// like the tape loader. Becomes the pad's drum-mode sample.
    pub fn load_pad_sample(&mut self, i: usize, path: &str) -> Result<String, String> {
        if i >= N_PADS {
            return Err("bad pad".to_string());
        }
        let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let dec = crate::decode::decode_file(path)?;
        if dec.samples.is_empty() {
            return Err(format!("{file}: no audio"));
        }
        let mut clean = dec.samples;
        dsp::normalize(&mut clean, 0.92);
        let audio = dsp::sp1200ize_i16(&clean, dec.sample_rate as f64);
        let label: String = file_stem(path).to_uppercase().chars().take(12).collect();
        let n = audio.len();
        self.pad_factory[i] = audio.clone();
        self.pads[i].custom = true;
        self.pads[i].set_source(audio, Some(label.clone()));
        self.chop_map[i] = None; // explicit load: this pad plays the file
        Ok(format!("PAD {} <- {label} ({n} SAMPLES)", i + 1))
    }

    /// Set the pad's filter. Cutoff is clamped to 80..12000 Hz.
    pub fn set_filter(&mut self, i: usize, kind: FilterKind, cutoff: f32) {
        if i >= N_PADS {
            return;
        }
        let p = &mut self.pads[i];
        p.filter_kind = kind;
        p.filter_cutoff = cutoff.clamp(80.0, 12000.0);
        p.refresh_filter();
    }

    /// Nudge the filter cutoff multiplicatively (the UI's Up/Down).
    pub fn nudge_cutoff(&mut self, i: usize, up: bool) {
        if i >= N_PADS {
            return;
        }
        let factor = if up { 1.122 } else { 1.0 / 1.122 };
        let cutoff = (self.pads[i].filter_cutoff * factor).clamp(80.0, 12000.0);
        let kind = self.pads[i].filter_kind;
        self.set_filter(i, kind, cutoff);
    }

    /// Set the pad's filter resonance (Q), clamped 0.1..12.
    pub fn set_res(&mut self, i: usize, q: f32) {
        if i >= N_PADS {
            return;
        }
        let p = &mut self.pads[i];
        p.filter_res = q.clamp(0.1, 12.0);
        p.refresh_filter();
    }

    /// Nudge the filter resonance multiplicatively (the UI's -/=).
    pub fn nudge_res(&mut self, i: usize, up: bool) {
        if i >= N_PADS {
            return;
        }
        let factor = if up { 1.2 } else { 1.0 / 1.2 };
        let q = (self.pads[i].filter_res * factor).clamp(0.1, 12.0);
        self.set_res(i, q);
    }

    /// Nudge the pad's pitch in semitones, clamped -24..+24.
    pub fn nudge_pitch(&mut self, i: usize, delta_st: f32) {
        if i >= N_PADS {
            return;
        }
        self.pads[i].tune_st = (self.pads[i].tune_st + delta_st).clamp(-24.0, 24.0);
    }

    /// Toggle mono/poly voice mode for a pad.
    pub fn set_mono(&mut self, i: usize, mono: bool) {
        if i < N_PADS {
            self.pads[i].mono = mono;
        }
    }

    /// Set the pad's choke group (0 = none).
    pub fn set_choke(&mut self, i: usize, group: u8) {
        if i >= N_PADS {
            return;
        }
        self.pads[i].choke = group.min(N_CHOKE_GROUPS);
    }

    /// Nudge a trim edge by `delta` (fraction of the source, signed).
    /// `which`: 0 = start, 1 = end. Start never passes end and vice versa.
    pub fn nudge_trim(&mut self, i: usize, which: usize, delta: f32) {
        if i >= N_PADS {
            return;
        }
        let p = &mut self.pads[i];
        if which == 0 {
            p.trim.0 = (p.trim.0 + delta).clamp(0.0, p.trim.1 - 0.01);
        } else {
            p.trim.1 = (p.trim.1 + delta).clamp(p.trim.0 + 0.01, 1.0);
        }
        p.rebuild();
    }

    /// Toggle reverse playback of the trimmed region.
    pub fn toggle_reverse(&mut self, i: usize) {
        if i >= N_PADS {
            return;
        }
        let p = &mut self.pads[i];
        p.reverse = !p.reverse;
        p.rebuild();
    }

    /// Toggle pad looping. Returns the new state.
    pub fn toggle_loop(&mut self, i: usize) -> bool {
        if i >= N_PADS {
            return false;
        }
        self.pads[i].loop_enabled = !self.pads[i].loop_enabled;
        self.pads[i].loop_enabled
    }

    /// Nudge a loop point by a fraction of the playable sample (which 0 =
    /// start, 1 = end), keeping start < end with a 1% gap, like trim.
    pub fn nudge_loop(&mut self, i: usize, which: usize, delta: f32) {
        if i >= N_PADS {
            return;
        }
        let p = &mut self.pads[i];
        if which == 0 {
            p.loop_pts.0 = (p.loop_pts.0 + delta).clamp(0.0, p.loop_pts.1 - 0.01);
        } else {
            p.loop_pts.1 = (p.loop_pts.1 + delta).clamp(p.loop_pts.0 + 0.01, 1.0);
        }
    }

    /// Set loop points in seconds (of the playable sample) and enable
    /// looping. Used by the `:loop` command.
    pub fn set_loop_secs(&mut self, i: usize, start: f32, end: f32) -> Result<String, String> {
        if i >= N_PADS {
            return Err("NO SUCH PAD".to_string());
        }
        let dur = self.pads[i].sample.len() as f32 / SP_RATE as f32;
        if dur <= 0.0 {
            return Err("PAD HAS NO SAMPLE".to_string());
        }
        if !(start >= 0.0 && end > start && end <= dur) {
            return Err(format!("LOOP NEEDS 0 <= START < END <= {dur:.2}S"));
        }
        self.pads[i].loop_pts = (start / dur, end / dur);
        self.pads[i].loop_enabled = true;
        Ok(format!("PAD {} LOOP {start:.2}-{end:.2}S", i + 1))
    }

    /// Resample a 4-track track's trimmed region onto a pad, through the
    /// 12-bit path. The pad stops playing any tape chop and keeps the
    /// resample as its custom sound (saved by `W`, restored on tape
    /// clear). Returns a status line, or an error for bad indices /
    /// empty regions.
    pub fn resample_track_to_pad(&mut self, track: usize, pad: usize) -> Result<String, String> {
        if track >= crate::fourtrack::N_TRACKS {
            return Err("NO SUCH TRACK (1-4)".to_string());
        }
        if pad >= N_PADS {
            return Err("NO SUCH PAD (1-8)".to_string());
        }
        let region = self.four_track.tracks[track].trimmed_region().to_vec();
        if region.is_empty() {
            return Err(format!("TRACK {} EMPTY (OR TRIM EMPTY)", track + 1));
        }
        // Print the tape-bus character into the bounce: the pad gets
        // what the tape section sounds like, not the dry take.
        let region = self.four_track.print_bus(&region);
        let audio = dsp::quantize12_i16(&region);
        let secs = audio.len() as f32 / SP_RATE as f32;
        self.chop_map[pad] = None;
        self.pad_factory[pad] = audio.clone();
        self.pads[pad].custom = true;
        self.pads[pad].set_source(audio, Some(format!("TRK{}", track + 1)));
        Ok(format!(
            "TRACK {} -> PAD {} ({:.1}S)",
            track + 1,
            pad + 1,
            secs
        ))
    }

    /// Destructively normalize the pad's source to 0.92 peak, then rebuild.
    pub fn normalize_pad(&mut self, i: usize) {
        if i >= N_PADS || self.pads[i].source.is_empty() {
            return;
        }
        let mut f: Vec<f32> = self.pads[i]
            .source
            .iter()
            .map(|&s| s as f32 / 2047.0)
            .collect();
        dsp::normalize(&mut f, 0.92);
        self.pads[i].source = dsp::quantize12_i16(&f);
        self.pads[i].rebuild();
    }

    /// Display label for a pad: CHOP n in tape mode, else custom label or
    /// the drum name.
    pub fn pad_label(&self, i: usize) -> String {
        if let Some(c) = self.chop_map[i] {
            return format!("CHOP {}", c + 1);
        }
        if let Some(l) = &self.pads[i].label {
            return l.clone();
        }
        DRUM_LABELS[i].to_string()
    }

    /// One-line tape readout for the UI, or None when no tape is loaded.
    pub fn tape_status(&self) -> Option<String> {
        self.tape.as_ref().map(|t| {
            let n = t.chops.len();
            if t.audio.is_empty() {
                format!("TAPE: {} - NO AUDIO", t.name)
            } else if n <= N_PADS {
                format!("TAPE: {} - {} CHOPS", t.name, n)
            } else {
                format!("TAPE: {} - {}/{} CHOPS", t.name, N_PADS, n)
            }
        })
    }

    /// Set the playing sequence length, clamped to 8..=64. Steps past
    /// the new length are kept, not cleared.
    pub fn set_seq_len(&mut self, n: usize) {
        self.seq_len = n.clamp(MIN_STEPS, MAX_STEPS);
    }

    fn fire_step(&mut self, step: u64) {
        let s = (step % self.seq_len as u64) as usize;
        self.last_step = s as u32;
        for pad in 0..N_PADS {
            if self.patterns[pad][s] {
                self.trigger(pad);
            }
        }
    }

    /// Render `out.len()` samples @ 26.04 kHz into `out` (-1.0..1.0).
    /// Attach the live-input capture queue (best-effort, from the SDL2
    /// capture device). Until this is called, INPUT tracks record silence.
    pub fn set_capture_input(&mut self, queue: Arc<Mutex<VecDeque<f32>>>, rate: f64) {
        self.capture = Some(CaptureInput { queue, rate });
    }

    pub fn has_input(&self) -> bool {
        self.capture.is_some()
    }

    /// Drain the capture queue, resampling to SP_RATE. Underruns emit
    /// silence without advancing, so the recording stays in sync.
    fn drain_capture(&mut self, n: usize) -> Vec<f32> {
        let (rate, queue) = match &self.capture {
            Some(c) => (c.rate, Arc::clone(&c.queue)),
            None => return vec![0.0; n],
        };
        let mut q = queue.lock().unwrap();
        // Bound memory: drop oldest beyond 4 s of audio.
        let maxq = (rate * 4.0) as usize;
        if q.len() > maxq {
            let drop = q.len() - maxq;
            q.drain(..drop);
            self.cap_phase = (self.cap_phase - drop as f64).max(0.0);
        }
        let step = rate / SP_RATE;
        let mut out = Vec::with_capacity(n);
        let mut pos = self.cap_phase;
        for _ in 0..n {
            let i0 = pos as usize;
            if i0 + 1 >= q.len() {
                out.push(0.0);
                continue;
            }
            let f = (pos - i0 as f64) as f32;
            out.push(q[i0] + (q[i0 + 1] - q[i0]) * f);
            pos += step;
        }
        let consumed = (pos as usize).min(q.len());
        q.drain(..consumed);
        self.cap_phase = pos - consumed as f64;
        out
    }

    pub fn render(&mut self, out: &mut [f32]) {
        let d = dsp::sixteenth_dur(self.bpm);
        for s in out.iter_mut() {
            // BPM 0 parks the sequencer: steps never fire (sixteenth_dur
            // would be infinite and the step time would overflow), but
            // voices and manual pad triggers still sound.
            while self.bpm > 0.0 && self.playing && self.clock >= self.next_step_at {
                self.fire_step(self.sched_step);
                self.sched_step += 1;
                let bar = self.sched_step / self.seq_len as u64;
                let st = (self.sched_step % self.seq_len as u64) as u32;
                let t =
                    bar as f64 * self.seq_len as f64 * d + dsp::step_time16(st, self.bpm, self.swing);
                self.next_step_at = self.pattern_start + js_round(t * SP_RATE) as u64;
            }
            let mut mix = 0.0f32;
            for v in self.voices.iter_mut() {
                if !v.active {
                    continue;
                }
                let pad = &self.pads[v.pad];
                // Looping: the voice plays from the start once, then
                // repeats the loop region instead of dying at the end.
                if let Some((la, lb)) = pad.loop_range() {
                    let span = (lb - la) as f64;
                    if span > 0.0 {
                        while v.pos >= lb as f64 {
                            v.pos -= span;
                        }
                    }
                }
                let idx = v.pos as usize;
                if idx + 1 >= pad.sample.len() {
                    v.active = false;
                    continue;
                }
                let frac = (v.pos - idx as f64) as f32;
                let a = pad.sample[idx] as f32 / 2047.0;
                let b = pad.sample[idx + 1] as f32 / 2047.0;
                let x = (a + (b - a) * frac) * pad.level;
                // Off is a branch, not a pass-through filter: bit-identical
                // to the pre-filter mixer.
                let y = if pad.filter_kind == FilterKind::Off {
                    x
                } else {
                    dsp::apply_biquad(x, &mut v.fstate, &pad.fb, &pad.fa)
                };
                mix += y;
                v.pos += pad.ratio();
            }
            *s = (mix * self.master).clamp(-1.0, 1.0);
            self.clock += 1;
        }
        // 4-track tape: playback + recording. The bounce source is the
        // voice/pattern mix captured above, before tape playback is added.
        if self.four_track.playing || self.four_track.recording {
            let n = out.len();
            let rec = self.four_track.recording;
            let need_input = rec
                && self
                    .four_track
                    .tracks
                    .iter()
                    .any(|t| t.armed && t.source == TrackSource::Input);
            let need_master = rec
                && self
                    .four_track
                    .tracks
                    .iter()
                    .any(|t| t.armed && t.source == TrackSource::Master);
            let input = if need_input {
                self.drain_capture(n)
            } else {
                Vec::new()
            };
            let master = if need_master {
                Some(out.to_vec())
            } else {
                None
            };
            self.four_track.process(out, &input, master.as_deref());
        }
    }

    pub fn clock(&self) -> u64 {
        self.clock
    }

    /// Three factory presets: (name, bpm, swing, patterns).
    pub fn apply_preset(&mut self, idx: usize) {
        const K: usize = 0;
        const S: usize = 1;
        const C: usize = 2;
        const R: usize = 3;
        const H: usize = 4;
        const O: usize = 5;
        const T: usize = 6;
        const X: usize = 7; // shaker
        let mut p = [[false; MAX_STEPS]; N_PADS];
        let mut on = |pad: usize, steps: &[usize]| {
            for &s in steps {
                p[pad][s] = true;
            }
        };
        match idx {
            // BOOM BAP — 92 BPM, loping swing
            0 => {
                self.bpm = 92.0;
                self.swing = 62.0;
                on(K, &[0, 4, 8, 12]);
                on(S, &[4, 12]);
                on(C, &[12]);
                on(H, &[0, 2, 4, 6, 8, 10, 12, 14]);
                on(O, &[14]);
                on(X, &[2, 6, 10]);
            }
            // HOUSE — 122 BPM, straight four on the floor
            1 => {
                self.bpm = 122.0;
                self.swing = 50.0;
                on(K, &[0, 4, 8, 12]);
                on(C, &[4, 12]);
                on(H, &[0, 2, 4, 6, 8, 10, 12, 14]);
                on(O, &[2, 6, 10, 14]);
                on(R, &[15]);
            }
            // LOPE — 84 BPM, heavy swing
            _ => {
                self.bpm = 84.0;
                self.swing = 70.0;
                on(K, &[0, 4, 8, 12]);
                on(S, &[4, 12]);
                on(R, &[7, 15]);
                on(H, &[0, 4, 8, 12]);
                on(T, &[10]);
                on(X, &[2, 6, 10, 14]);
            }
        }
        self.patterns = p;
        // Factory presets are 16-step patterns; loading one resets the
        // length so stale steps past 16 can't leak into the preset.
        self.seq_len = 16;
    }

    pub fn preset_name(idx: usize) -> &'static str {
        match idx {
            0 => "BOOM BAP",
            1 => "HOUSE",
            _ => "LOPE",
        }
    }
}

// ---------- offline deterministic render (parity path) ----------

/// The fixed 2-bar demo pattern used by `--render` and the JS parity
/// harness. 32 steps; every drum appears at least once.
pub const DEMO_BPM: f64 = 100.0;
pub const DEMO_SWING: f64 = 60.0;

pub fn demo_pattern() -> [[bool; 32]; N_PADS] {
    let mut p = [[false; 32]; N_PADS];
    let mut on = |pad: usize, steps: &[usize]| {
        for &s in steps {
            p[pad][s] = true;
        }
    };
    on(0, &[0, 4, 8, 12, 16, 20, 24, 28]); // kick
    on(1, &[4, 12, 20, 28]); // snare
    on(2, &[12, 28]); // clap
    on(3, &[7, 23]); // rim
    on(
        4,
        &[0, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 22, 24, 26, 28, 30],
    ); // chat
    on(5, &[14, 30]); // ohat
    on(6, &[24]); // tom
    on(7, &[2, 6, 10, 14, 18, 22, 26, 30]); // shaker
    p
}

/// Render the demo pattern with exact integer accumulation:
/// each voice is a plain i16 copy-add at its scheduled offset, levels 1.0,
/// tune 1.0. The JS harness mirrors this bit-for-bit.
pub fn render_demo_i16(seed: u32) -> Vec<i16> {
    let mut rng = XorShift32::new(seed);
    let drums: Vec<Vec<i16>> = DRUM_NAMES
        .iter()
        .map(|name| dsp::synth_drum_i16(name, &mut rng))
        .collect();
    let pat = demo_pattern();
    let d = dsp::sixteenth_dur(DEMO_BPM);
    let mut hits: Vec<(usize, usize)> = Vec::new();
    let mut total = 0usize;
    for s in 0..32u32 {
        let bar = s / 16;
        let st = s % 16;
        let t = bar as f64 * 16.0 * d + dsp::step_time16(st, DEMO_BPM, DEMO_SWING);
        let start = js_round(t * SP_RATE) as usize;
        for (pad, col) in pat.iter().enumerate() {
            if col[s as usize] {
                hits.push((start, pad));
                total = total.max(start + drums[pad].len());
            }
        }
    }
    let mut mix = vec![0i32; total];
    for (start, pad) in hits {
        for (i, &sm) in drums[pad].iter().enumerate() {
            mix[start + i] += sm as i32;
        }
    }
    mix.into_iter()
        .map(|v| v.clamp(-32768, 32767) as i16)
        .collect()
}

/// Fit a rendered demo to an exact duration: truncate or pad with silence.
pub fn fit_to_seconds(samples: &[i16], seconds: f64) -> Vec<i16> {
    let want = js_round(seconds * SP_RATE) as usize;
    let mut out = vec![0i16; want];
    let n = samples.len().min(want);
    out[..n].copy_from_slice(&samples[..n]);
    out
}

// ---------- tape file loading ----------

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn file_stem(path: &str) -> String {
    let f = file_name(path);
    match f.rfind('.') {
        Some(i) => f[..i].to_string(),
        None => f.to_string(),
    }
}

/// Load a .wav/.mp3 as the tape: decode → normalize (like the web) → the
/// 12-bit path → auto-splice. Returns a status line for the UI.
pub fn load_tape_file(eng: &mut Engine, path: &str) -> Result<String, String> {
    let dec = crate::decode::decode_file(path)?;
    if dec.samples.is_empty() {
        return Err(format!("{}: no audio", file_name(path)));
    }
    // Mirror the web pipeline: normalize clean, then the SP-1200 converters.
    let mut clean = dec.samples;
    dsp::normalize(&mut clean, 0.92);
    let audio = dsp::sp1200ize_i16(&clean, dec.sample_rate as f64);
    let name: String = file_stem(path).to_uppercase().chars().take(18).collect();
    let n = eng.load_tape(name.clone(), audio);
    Ok(format!("TAPE {name} - {n} CHOPS (S/E RESPLICE, D CLEAR)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_retrigger_cuts_own_voices_only() {
        let mut e = Engine::new();
        // Poly (default): voices stack.
        e.trigger(0);
        e.trigger(0);
        assert_eq!(e.active_voices(), 2);
        e.panic();
        // Mono: the second hit kills the first.
        e.set_mono(0, true);
        e.trigger(0);
        e.trigger(0);
        assert_eq!(e.active_voices(), 1);
        // Other pads are unaffected.
        e.trigger(1);
        assert_eq!(e.active_voices(), 2);
        e.trigger(0);
        assert_eq!(e.active_voices(), 2); // pad 0's voice replaced, pad 1 rings on
    }

    #[test]
    fn pitch_nudge_clamps_and_changes_ratio() {
        let mut e = Engine::new();
        e.nudge_pitch(2, 12.0);
        assert!((e.pads[2].tune_st - 12.0).abs() < 1e-6);
        assert!((e.pads[2].ratio() - 2.0).abs() < 1e-9);
        e.nudge_pitch(2, 100.0);
        assert_eq!(e.pads[2].tune_st, 24.0);
        e.nudge_pitch(2, -100.0);
        assert_eq!(e.pads[2].tune_st, -24.0);
        // Out-of-range pad index is a no-op, not a panic.
        e.nudge_pitch(99, 1.0);
        e.set_mono(99, true);
        e.nudge_res(99, true);
    }

    #[test]
    fn res_nudge_clamps_and_recomputes_coeffs() {
        let mut e = Engine::new();
        e.set_filter(3, FilterKind::Lowpass, 1000.0);
        let before = e.pads[3].fb;
        e.nudge_res(3, true);
        assert!(e.pads[3].filter_res > 0.71);
        assert_ne!(e.pads[3].fb, before);
        e.set_res(3, 100.0);
        assert_eq!(e.pads[3].filter_res, 12.0);
        e.set_res(3, -5.0);
        assert_eq!(e.pads[3].filter_res, 0.1);
        // Render with high resonance: no panic, finite output.
        e.trigger(3);
        let mut out = vec![0.0f32; 2000];
        e.render(&mut out);
        assert!(out.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn panic_kills_all_voices_keeps_transport() {
        let mut e = Engine::new();
        e.play();
        for i in 0..N_PADS {
            e.trigger(i);
        }
        assert!(e.voices.iter().any(|v| v.active));
        e.panic();
        assert!(e.voices.iter().all(|v| !v.active));
        assert!(e.playing); // transport untouched
                            // New triggers still work after a panic.
        e.trigger(0);
        assert!(e.voices.iter().any(|v| v.active));
    }

    #[test]
    fn locked_pad_survives_tape_load_splice_clear() {
        let mut e = Engine::new();
        let drum0 = e.pads[0].source.clone();
        e.set_locked(0, true);
        // Tape load: unlocked pads get chops, locked pad keeps its drum.
        let mut tape = Vec::new();
        for _ in 0..4 {
            tape.push(2047);
            tape.extend(std::iter::repeat(300i16).take(3000));
        }
        e.load_tape("T".to_string(), tape);
        assert_eq!(e.pads[0].source, drum0);
        assert_eq!(e.pad_label(0), "KICK");
        assert_eq!(e.pad_label(1), "CHOP 2"); // pad 2 plays chop 2
                                              // Splice: locked pad still untouched.
        e.splice_equal();
        assert_eq!(e.pads[0].source, drum0);
        // Clear: locked pad keeps its sample instead of reverting.
        e.clear_tape();
        assert_eq!(e.pads[0].source, drum0);
        assert!(e.tape.is_none());
        // Unlock and reload: now it takes the chop like everyone else.
        e.set_locked(0, false);
        let mut tape = Vec::new();
        for _ in 0..4 {
            tape.push(2047);
            tape.extend(std::iter::repeat(300i16).take(3000));
        }
        e.load_tape("T2".to_string(), tape);
        assert_eq!(e.pad_label(0), "CHOP 1");
    }

    #[test]
    fn locked_chop_pad_keeps_chop_across_tape_swap() {
        let mut e = Engine::new();
        let mut tape = Vec::new();
        for _ in 0..4 {
            tape.push(2047);
            tape.extend(std::iter::repeat(300i16).take(3000));
        }
        e.load_tape("A".to_string(), tape);
        let chop_src = e.pads[2].source.clone();
        e.set_locked(2, true);
        // New tape: locked pad keeps the OLD chop audio.
        let mut tape2 = Vec::new();
        for _ in 0..8 {
            tape2.push(2047);
            tape2.extend(std::iter::repeat(900i16).take(2000));
        }
        e.load_tape("B".to_string(), tape2);
        assert_eq!(e.pads[2].source, chop_src);
        assert_eq!(e.pad_label(2), "CHOP 3"); // old chop index preserved
        assert_eq!(e.pad_label(0), "CHOP 1"); // unlocked pads follow the new tape
    }

    #[test]
    fn explicit_pad_load_overrides_but_keeps_lock() {
        let mut e = Engine::new();
        e.set_locked(1, true);
        let custom = vec![777i16; 300];
        // Simulate an explicit browser load via set_source path.
        e.pad_factory[1] = custom.clone();
        e.pads[1].custom = true;
        e.pads[1].set_source(custom.clone(), Some("MINE".to_string()));
        e.chop_map[1] = None;
        assert_eq!(e.pads[1].source, custom);
        assert!(e.pads[1].locked); // still pinned against future tapes
                                   // ...which a later tape load then respects.
        let mut tape = Vec::new();
        for _ in 0..4 {
            tape.push(2047);
            tape.extend(std::iter::repeat(300i16).take(3000));
        }
        e.load_tape("T".to_string(), tape);
        assert_eq!(e.pads[1].source, custom);
    }

    #[test]
    fn choke_group_kills_same_group_voices() {
        let mut e = Engine::new();
        e.set_choke(0, 1);
        e.set_choke(1, 1);
        e.set_choke(2, 2);
        e.trigger(0);
        e.trigger(2);
        assert_eq!(e.voices.iter().filter(|v| v.active).count(), 2);
        // Pad 1 shares group 1 with pad 0: pad 0's voice dies.
        e.trigger(1);
        let active: Vec<usize> = e
            .voices
            .iter()
            .filter(|v| v.active)
            .map(|v| v.pad)
            .collect();
        assert!(!active.contains(&0), "choked voice survived: {active:?}");
        assert!(active.contains(&1));
        assert!(active.contains(&2), "other group killed: {active:?}");
        // Group 0 never chokes.
        e.set_choke(0, 0);
        e.set_choke(1, 0);
        e.trigger(0);
        e.trigger(1);
        assert!(e.voices.iter().filter(|v| v.active && v.pad == 0).count() >= 1);
    }

    #[test]
    fn trim_and_reverse_rebuild() {
        let mut e = Engine::new();
        let src = e.pads[0].source.clone();
        let n = src.len();
        e.nudge_trim(0, 0, 0.5); // start -> 50%
        e.nudge_trim(0, 1, -0.25); // end -> 75%
        assert!((e.pads[0].trim.0 - 0.5).abs() < 1e-6);
        assert!((e.pads[0].trim.1 - 0.75).abs() < 1e-6);
        let expect_len = (0.75 * n as f32) as usize - (0.5 * n as f32) as usize;
        assert_eq!(e.pads[0].sample.len(), expect_len);
        // Start can never pass end.
        e.nudge_trim(0, 0, 0.9);
        assert!(e.pads[0].trim.0 <= e.pads[0].trim.1);
        // Reverse flips the playable buffer.
        let before = e.pads[0].sample.clone();
        e.toggle_reverse(0);
        assert!(e.pads[0].reverse);
        let after = &e.pads[0].sample;
        assert_eq!(after.len(), before.len());
        assert_eq!(after[0], before[before.len() - 1]);
        assert_eq!(after[after.len() - 1], before[0]);
    }

    #[test]
    fn loop_range_clamps_to_valid_indices() {
        let mut e = Engine::new();
        // Loop off by default.
        assert_eq!(e.pads[0].loop_range(), None);
        e.pads[0].set_source((0..100).map(|i| i as i16).collect(), None);
        e.pads[0].loop_enabled = true;
        // Full loop: end clamps to len - 1 so idx + 1 stays in bounds.
        assert_eq!(e.pads[0].loop_range(), Some((0, 99)));
        // Fractions past the edges clamp, and a reversed pair still yields
        // a valid (start < end) region.
        e.pads[0].loop_pts = (0.9, 0.2);
        assert_eq!(e.pads[0].loop_range(), Some((90, 91)));
        // Too-short samples never loop.
        e.pads[1].set_source(vec![7], None);
        e.pads[1].loop_enabled = true;
        assert_eq!(e.pads[1].loop_range(), None);
    }

    #[test]
    fn looping_voice_survives_past_sample_end_and_repeats() {
        let mut e = Engine::new();
        // A tiny deterministic sample: ramp 0..99, so the loop period is
        // visible sample-for-sample in the output.
        let ramp: Vec<i16> = (0..100).map(|i| (i * 20) as i16).collect();
        e.pads[0].set_source(ramp.clone(), None);
        e.pads[0].loop_pts = (0.25, 0.75);
        e.pads[0].loop_enabled = true;
        // Pad 0 defaults: level 0.9, tune 0 st (ratio 1.0), master 0.8.
        e.trigger(0);
        let mut out = vec![0.0f32; 200];
        e.render(&mut out);
        assert_eq!(e.active_voices(), 1, "looping voice never dies");
        let scale = 0.9 * 0.8 / 2047.0;
        for i in 0..200 {
            // Playhead: 0..74 once, then 25..74 repeating.
            let pos = if i < 75 { i } else { 25 + (i - 75) % 50 };
            let want = ramp[pos] as f32 * scale;
            assert!((out[i] - want).abs() < 1e-4, "i={i} pos={pos}");
        }
        // Same pad, loop off: the voice dies at the end.
        let mut e2 = Engine::new();
        e2.pads[0].set_source(ramp, None);
        e2.trigger(0);
        let mut out2 = vec![0.0f32; 200];
        e2.render(&mut out2);
        assert_eq!(e2.active_voices(), 0, "non-looping voice dies");
        assert!(out2[150..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn loop_nudge_and_secs_command_validation() {
        let mut e = Engine::new();
        e.pads[0].set_source(vec![0i16; 26040], None); // exactly 1.0 s
        e.nudge_loop(0, 0, 0.2);
        assert!((e.pads[0].loop_pts.0 - 0.2).abs() < 1e-6);
        e.nudge_loop(0, 1, -0.5);
        assert!((e.pads[0].loop_pts.1 - 0.5).abs() < 1e-6);
        e.nudge_loop(0, 0, 0.9); // start can never pass end
        assert!(e.pads[0].loop_pts.0 <= e.pads[0].loop_pts.1);
        assert!(e.set_loop_secs(0, 0.25, 0.75).is_ok());
        assert!(e.pads[0].loop_enabled);
        assert!(e.set_loop_secs(0, 0.75, 0.25).is_err()); // reversed
        assert!(e.set_loop_secs(0, 0.0, 1.5).is_err()); // past the end
        assert!(e.set_loop_secs(0, -0.1, 0.5).is_err()); // negative start
    }

    #[test]
    fn resample_track_to_pad_quantizes_trimmed_region() {
        let mut e = Engine::new();
        e.four_track.tracks[2].samples = vec![0.0, 0.5, -0.5, 1.0, -1.0, 0.25];
        e.four_track.tracks[2].set_trim(1, Some(5)); // 0.5, -0.5, 1.0, -1.0
        let msg = e.resample_track_to_pad(2, 4).unwrap();
        assert!(msg.contains("TRACK 3 -> PAD 5"), "{msg}");
        let pad = &e.pads[4];
        assert_eq!(pad.sample, vec![1024, -1023, 2047, -2047]);
        assert!(pad.custom, "resample is saved by W");
        assert_eq!(pad.label.as_deref(), Some("TRK3"));
        // Empty region / bad indices are clean errors, no panic.
        assert!(e.resample_track_to_pad(0, 0).is_err());
        assert!(e.resample_track_to_pad(4, 0).is_err());
        assert!(e.resample_track_to_pad(0, 8).is_err());
    }

    #[test]
    fn resample_prints_tape_bus_into_pad() {
        // Warmth: an alternating signal settles to a saturated level
        // well below the dry 1024/-1023 (a constant would be pure DC,
        // which the warmth stage's DC blocker correctly removes).
        let mut e = Engine::new();
        e.four_track.tracks[0].samples =
            (0..1000).map(|i| if i % 2 == 0 { 0.5 } else { -0.5 }).collect();
        e.four_track.set_warmth(100.0);
        e.resample_track_to_pad(0, 0).unwrap();
        let warm = e.pads[0].sample.clone();
        assert_eq!(warm[900], warm[902], "settled by the tail");
        assert!(warm[900] < 1024 && warm[900] > 0, "saturated: {}", warm[900]);
        assert!(warm[901] < 0 && warm[901] > -1023, "saturated: {}", warm[901]);
        // Distortion: constant in, constant (squashed) out.
        let mut e = Engine::new();
        e.four_track.tracks[0].samples = vec![0.5; 100];
        e.four_track.set_distortion(100.0);
        e.resample_track_to_pad(0, 0).unwrap();
        let dist = e.pads[0].sample.clone();
        assert!(dist.iter().all(|&s| s == dist[0]), "stateless");
        assert!(dist[0] < 512 && dist[0] > 0, "squashed: {}", dist[0]);
        // Same bounce twice is identical (fresh bus state each time).
        let mut e = Engine::new();
        e.four_track.tracks[0].samples = vec![0.5, -0.5, 0.25, -0.25];
        e.four_track.set_warmth(75.0);
        e.four_track.set_distortion(40.0);
        e.resample_track_to_pad(0, 0).unwrap();
        e.resample_track_to_pad(0, 1).unwrap();
        assert_eq!(e.pads[0].sample, e.pads[1].sample);
    }

    #[test]
    fn bpm_zero_parks_sequencer_without_panic() {
        let mut e = Engine::new();
        e.patterns[0][0] = true;
        e.play();
        e.bpm = 0.0;
        // 100k samples at 0 BPM: no step ever fires (the step time
        // would be infinite), and nothing overflows or panics.
        let mut out = vec![0.0f32; 100_000];
        e.render(&mut out);
        assert_eq!(e.sched_step, 0);
        assert_eq!(e.last_step, 0);
        // Manual pad triggers still sound at 0 BPM.
        e.trigger(0);
        let mut out2 = vec![0.0f32; 1000];
        e.render(&mut out2);
        assert!(out2.iter().any(|&s| s != 0.0));
    }

    #[test]
    fn bpm_256_steps_fire() {
        let mut e = Engine::new();
        e.patterns[0][0] = true;
        e.play();
        e.bpm = 256.0;
        // A 16th at 256 BPM is ~1525 samples; 100k samples must fire
        // many steps.
        let mut out = vec![0.0f32; 100_000];
        e.render(&mut out);
        assert!(e.sched_step > 4);
    }

    #[test]
    fn seq_len_default_clamps_and_keeps_steps() {
        let mut e = Engine::new();
        assert_eq!(e.seq_len, 16);
        e.set_seq_len(4);
        assert_eq!(e.seq_len, 8);
        e.set_seq_len(100);
        assert_eq!(e.seq_len, 64);
        e.set_seq_len(32);
        assert_eq!(e.seq_len, 32);
        // Shortening keeps steps past the length; lengthening restores them.
        e.patterns[0][40] = true;
        e.set_seq_len(16);
        e.set_seq_len(64);
        assert!(e.patterns[0][40]);
    }

    #[test]
    fn sequencer_wraps_at_seq_len() {
        // Step 20 fires with a 32-step sequence, never with an 8-step one.
        let mut e = Engine::new();
        e.set_seq_len(32);
        e.patterns[3][20] = true;
        e.play();
        let mut out = vec![0.0f32; 300_000];
        e.render(&mut out);
        assert!(e.pad_hit_at[3] > 0, "step 20 never fired at len 32");
        assert!(e.last_step < 32);

        let mut e = Engine::new();
        e.set_seq_len(8);
        e.patterns[3][20] = true;
        e.play();
        let mut out = vec![0.0f32; 300_000];
        e.render(&mut out);
        assert_eq!(e.pad_hit_at[3], 0, "step 20 fired at len 8");
        assert!(e.last_step < 8);
    }

    #[test]
    fn preset_resets_to_16_steps() {
        let mut e = Engine::new();
        e.set_seq_len(32);
        e.patterns[0][40] = true;
        e.apply_preset(0);
        assert_eq!(e.seq_len, 16);
        assert!(!e.patterns[0][40]);
        assert!(e.patterns[0][0]);
    }

    #[test]
    fn filter_off_is_bit_identical_to_legacy_mix() {
        // Two engines, same trigger: Off must match sample-for-sample.
        let mut a = Engine::new();
        let mut b = Engine::new();
        b.set_filter(0, FilterKind::Off, 8000.0);
        a.trigger(0);
        b.trigger(0);
        let mut oa = vec![0.0f32; 5000];
        let mut ob = vec![0.0f32; 5000];
        a.render(&mut oa);
        b.render(&mut ob);
        assert_eq!(oa, ob);
    }

    #[test]
    fn filter_lp_changes_output_and_hp_too() {
        let mut e = Engine::new();
        // Click-like source: full-spectrum, so filters bite.
        let click: Vec<i16> = (0..2000).map(|i| if i == 0 { 2047 } else { 0 }).collect();
        e.pads[0].set_source(click, None);
        e.pads[0].level = 1.0;
        e.master = 1.0;
        e.trigger(0);
        let mut off = vec![0.0f32; 2000];
        e.render(&mut off);
        let peak_off = off.iter().map(|v| v.abs()).fold(0.0f32, f32::max);

        let mut e2 = Engine::new();
        e2.pads[0].set_source(
            (0..2000).map(|i| if i == 0 { 2047 } else { 0 }).collect(),
            None,
        );
        e2.pads[0].level = 1.0;
        e2.master = 1.0;
        e2.set_filter(0, FilterKind::Lowpass, 400.0);
        e2.trigger(0);
        let mut lp = vec![0.0f32; 2000];
        e2.render(&mut lp);
        let peak_lp = lp.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        assert!(
            peak_lp < peak_off * 0.5,
            "LP did not tame the click: {peak_lp} vs {peak_off}"
        );
        for v in lp.iter() {
            assert!(v.is_finite());
        }
        // Cutoff knob moves the coefficients.
        let before = e2.pads[0].fb;
        e2.nudge_cutoff(0, true);
        assert!(e2.pads[0].filter_cutoff > 400.0);
        assert_ne!(e2.pads[0].fb, before);
    }

    #[test]
    fn normalize_pad_peaks_at_target() {
        let mut e = Engine::new();
        e.pads[0].set_source(vec![100i16; 1000], None);
        e.normalize_pad(0);
        let peak = e.pads[0]
            .source
            .iter()
            .map(|v| v.unsigned_abs())
            .max()
            .unwrap();
        // 0.92 * 2047 = 1883.24 -> quantized
        assert!((peak as i32 - 1883).abs() <= 1, "peak {peak}");
        assert_eq!(e.pads[0].source.len(), 1000);
    }

    #[test]
    fn load_pad_sample_reads_mp3() {
        let mut e = Engine::new();
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/sine.mp3");
        let msg = e.load_pad_sample(1, path).unwrap();
        assert!(msg.contains("PAD 2"), "{msg}");
        assert!(!e.pads[1].source.is_empty());
        assert!(e.pads[1].custom);
        assert_eq!(e.pads[1].label.as_deref(), Some("SINE"));
        // 12-bit path: all samples within +-2047.
        assert!(e.pads[1].source.iter().all(|&s| s.unsigned_abs() <= 2047));
        // Trigger + render: no panic, finite output.
        e.trigger(1);
        let mut out = vec![0.0f32; 5000];
        e.render(&mut out);
        assert!(out.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn load_tape_file_reads_mp3() {
        let mut e = Engine::new();
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/sine.mp3");
        let msg = load_tape_file(&mut e, path).unwrap();
        assert!(msg.contains("TAPE SINE"), "{msg}");
        assert!(e.tape.is_some());
        assert!(!e.tape.as_ref().unwrap().audio.is_empty());
        assert!(!e.tape.as_ref().unwrap().chops.is_empty());
    }

    #[test]
    fn load_pad_sample_reads_wav() {
        let dir = std::env::temp_dir().join(format!("sp1200rs-pad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("blip.wav");
        // Minimal mono 16-bit WAV, 44.1 kHz, 100 samples of a ramp.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&((36 + 200) as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&44100u32.to_le_bytes());
        bytes.extend_from_slice(&88200u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&200u32.to_le_bytes());
        for i in 0..100 {
            let s = (i as i16 * 300).min(30000);
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(&path, &bytes).unwrap();
        let mut e = Engine::new();
        let msg = e.load_pad_sample(3, path.to_str().unwrap()).unwrap();
        assert!(msg.contains("PAD 4"), "{msg}");
        assert!(!e.pads[3].source.is_empty());
        assert!(e.pads[3].custom);
        assert_eq!(e.pads[3].label.as_deref(), Some("BLIP"));
        // 12-bit path: all samples within +-2047.
        assert!(e.pads[3].source.iter().all(|&s| s.unsigned_abs() <= 2047));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn engine_boots_with_drums() {
        let e = Engine::new();
        assert_eq!(e.pads.len(), N_PADS);
        for (i, pad) in e.pads.iter().enumerate() {
            assert!(!pad.sample.is_empty(), "pad {i} empty");
        }
        assert_eq!(e.bpm, 92.0); // preset 0 applied
    }

    #[test]
    fn ninth_member_rejected_analogy_voice_stealing() {
        // Fill all voices, trigger one more: oldest is stolen, count stays 16.
        let mut e = Engine::new();
        for i in 0..20 {
            e.trigger(i % N_PADS);
        }
        let active = e.voices.iter().filter(|v| v.active).count();
        assert_eq!(active, N_VOICES);
    }

    #[test]
    fn render_advances_clock_and_fires_steps() {
        let mut e = Engine::new();
        e.play();
        let mut buf = vec![0.0f32; 26040]; // 1 second
        e.render(&mut buf);
        assert_eq!(e.clock(), 26040);
        // preset 0 has kick on step 0 -> pad 0 must have been hit
        assert!(e.pad_hit_at[0] < 26040);
        e.stop();
        assert!(!e.playing);
    }

    #[test]
    fn demo_render_is_deterministic() {
        let a = render_demo_i16(RENDER_SEED);
        let b = render_demo_i16(RENDER_SEED);
        assert_eq!(a, b);
        assert!(!a.is_empty());
        let peak = a.iter().map(|v| v.unsigned_abs() as i32).max().unwrap();
        assert!(peak > 1000, "demo too quiet: {peak}");
    }

    #[test]
    fn fit_to_seconds_pads_and_truncates() {
        let s = vec![7i16; 100];
        assert_eq!(fit_to_seconds(&s, 100.0 / SP_RATE).len(), 100);
        let padded = fit_to_seconds(&s, 200.0 / SP_RATE);
        assert_eq!(padded.len(), 200);
        assert_eq!(padded[150], 0);
        let cut = fit_to_seconds(&s, 50.0 / SP_RATE);
        assert_eq!(cut.len(), 50);
    }

    /// Synthetic tape: 4 regions with distinct constant values.
    fn region_tape() -> Vec<i16> {
        let mut t = Vec::new();
        for r in 0..4 {
            // Each region: loud click then a distinct DC-ish level.
            t.push(2047);
            t.extend(std::iter::repeat((r as i16 + 1) * 200).take(4999));
        }
        t
    }

    #[test]
    fn tape_load_splices_and_assigns_chop_pads() {
        let mut e = Engine::new();
        let n = e.load_tape("TEST".to_string(), region_tape());
        assert_eq!(n, 4);
        assert_eq!(e.pad_label(0), "CHOP 1");
        assert_eq!(e.pad_label(3), "CHOP 4");
        assert_eq!(e.pad_label(4), "CHAT"); // beyond the chops: drum
        assert_eq!(e.tape_status().unwrap(), "TAPE: TEST - 4 CHOPS");
        // Pad 3 plays the tape's 3rd chop region (level/master 1.0).
        e.pads[2].level = 1.0;
        e.master = 1.0;
        let (a, b) = e.tape.as_ref().unwrap().chops[2];
        e.trigger(2);
        let mut out = vec![0.0f32; b - a];
        e.render(&mut out);
        // Note: the voice mixer cuts the final sample (its `idx + 1`
        // bounds guard), so compare all but the last.
        for (i, &s) in out.iter().enumerate().take(out.len() - 1) {
            let expect = e.tape.as_ref().unwrap().audio[a + i] as f32 / 2047.0;
            assert!((s - expect).abs() < 1e-6, "sample {i}: {s} vs {expect}");
        }
    }

    #[test]
    fn splice_equal_assigns_first_eight() {
        let mut e = Engine::new();
        e.load_tape("EQ".to_string(), vec![100i16; 8000]);
        let n = e.splice_equal().unwrap();
        assert_eq!(n, 8);
        let chops = &e.tape.as_ref().unwrap().chops;
        assert_eq!(chops[0], (0, 1000));
        assert_eq!(chops[7], (7000, 8000));
        assert_eq!(e.pad_label(7), "CHOP 8");
    }

    #[test]
    fn more_than_eight_chops_shows_first_eight() {
        let mut e = Engine::new();
        // 12 onset-separated regions.
        let mut t = Vec::new();
        for _ in 0..12 {
            t.push(2047);
            t.extend(std::iter::repeat(300i16).take(3000));
        }
        let n = e.load_tape("MANY".to_string(), t);
        assert!(n >= 12, "got {n}");
        assert_eq!(
            e.tape_status().unwrap(),
            format!("TAPE: MANY - 8/{n} CHOPS")
        );
        assert_eq!(e.pad_label(7), "CHOP 8");
    }

    #[test]
    fn clear_tape_restores_drums() {
        let mut e = Engine::new();
        let drum0 = e.pads[0].sample.clone();
        e.load_tape("X".to_string(), region_tape());
        assert_ne!(e.pads[0].sample, drum0);
        e.clear_tape();
        assert_eq!(e.pads[0].sample, drum0);
        assert!(e.tape.is_none());
        assert_eq!(e.pad_label(0), "KICK");
    }

    #[test]
    fn custom_pad_survives_tape_clear() {
        let mut e = Engine::new();
        let custom = vec![1234i16; 500];
        e.set_custom_pad(0, custom.clone(), Some("MINE".to_string()));
        assert_eq!(e.pads[0].sample, custom);
        assert_eq!(e.pad_label(0), "MINE");
        e.load_tape("X".to_string(), region_tape());
        e.clear_tape();
        assert_eq!(e.pads[0].sample, custom);
    }

    #[test]
    fn splice_without_tape_is_none() {
        let mut e = Engine::new();
        assert_eq!(e.splice_onset(), None);
        assert_eq!(e.splice_equal(), None);
    }

    fn many_region_tape() -> Vec<i16> {
        // 12 onset-separated regions, like the many-chops test.
        let mut t = Vec::new();
        for _ in 0..12 {
            t.push(2047);
            t.extend(std::iter::repeat(300i16).take(3000));
        }
        t
    }

    #[test]
    fn assign_chop_arbitrary_and_survives_resplice() {
        let mut e = Engine::new();
        let n = e.load_tape("MANY".to_string(), many_region_tape());
        assert!(n >= 12, "got {n}");
        // Pad 1 takes chop 12: beyond the first eight pads' defaults.
        let m = e.assign_chop(0, Some(11)).unwrap();
        assert_eq!(m, "PAD 1 <- CHOP 12");
        assert_eq!(e.pad_label(0), "CHOP 12");
        assert_eq!(e.chop_map[0], Some(11));
        let (a, b) = e.tape.as_ref().unwrap().chops[11];
        assert_eq!(
            e.pads[0].source,
            e.tape.as_ref().unwrap().audio[a..b].to_vec()
        );
        // Re-splice the same audio: onset chops are identical, so the
        // assignment survives. (Before the refresh fix, any assignment
        // past chop 8 fell back to the drum sample here.)
        let n2 = e.splice_onset().unwrap();
        assert_eq!(n2, n);
        assert_eq!(e.pad_label(0), "CHOP 12");
        assert_eq!(e.chop_map[0], Some(11));
        let (a, b) = e.tape.as_ref().unwrap().chops[11];
        assert_eq!(
            e.pads[0].source,
            e.tape.as_ref().unwrap().audio[a..b].to_vec()
        );
    }

    #[test]
    fn assign_chop_errors_and_clear() {
        let mut e = Engine::new();
        assert_eq!(e.assign_chop(0, Some(0)).unwrap_err(), "NO TAPE");
        assert!(e.assign_chop(8, Some(0)).is_err());
        let n = e.load_tape("MANY".to_string(), many_region_tape());
        assert!(n >= 12, "got {n}");
        // Index n is out of range.
        assert!(e.assign_chop(0, Some(n)).is_err());
        // Clear: pad 1 back to its own chop.
        e.assign_chop(0, Some(11)).unwrap();
        let m = e.assign_chop(0, None).unwrap();
        assert_eq!(m, "PAD 1 <- CHOP 1");
        assert_eq!(e.chop_map[0], Some(0));
        // Clear a pad past the chop count: back to its drum sample.
        let mut e = Engine::new();
        e.load_tape("FOUR".to_string(), region_tape());
        e.assign_chop(4, Some(1)).unwrap();
        assert_eq!(e.pad_label(4), "CHOP 2");
        let m = e.assign_chop(4, None).unwrap();
        assert_eq!(m, "PAD 5 <- CHAT");
        assert_eq!(e.chop_map[4], None);
    }

    #[test]
    fn assign_chop_overrides_lock() {
        let mut e = Engine::new();
        e.set_locked(0, true);
        e.load_tape("T".to_string(), many_region_tape());
        assert_eq!(e.pad_label(0), "KICK"); // locked: kept its drum
        let m = e.assign_chop(0, Some(1)).unwrap();
        assert_eq!(m, "PAD 1 <- CHOP 2");
        assert_eq!(e.chop_map[0], Some(1));
        assert_ne!(e.pads[0].source, e.pad_factory[0]);
    }

    #[test]
    fn dropped_tape_inherits_project_markers() {
        let mut e = Engine::new();
        // Simulate a project load: markers, no audio.
        e.tape = Some(Tape {
            name: "SAVED".to_string(),
            audio: Vec::new(),
            chops: vec![(0, 1000), (1000, 3000)],
        });
        // Drop a tape: saved chops apply instead of onset detection.
        let n = e.load_tape("NEWFILE".to_string(), vec![100i16; 5000]);
        assert_eq!(n, 2);
        assert_eq!(
            e.tape.as_ref().unwrap().chops,
            vec![(0, 1000), (1000, 3000)]
        );
        // Markers beyond the audio are clamped; degenerate ones dropped.
        e.tape = Some(Tape {
            name: "SAVED".to_string(),
            audio: Vec::new(),
            chops: vec![(0, 99999), (5000, 5000)],
        });
        let n = e.load_tape("NEWFILE".to_string(), vec![100i16; 5000]);
        assert_eq!(n, 1);
        assert_eq!(e.tape.as_ref().unwrap().chops, vec![(0, 5000)]);
    }
}
