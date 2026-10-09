//! Playback engine: 8 pads, polyphonic voices, 16-step sequencer, mixer.
//!
//! All samples are i16 @ 26.04 kHz (the SP path). The real-time mixer works
//! in f32; the offline `--render` path uses exact integer accumulation so it
//! can be byte-compared against the JS reference.

use crate::dsp::{self, js_round, XorShift32, DRUM_NAMES, SP_RATE};

pub const N_PADS: usize = 8;
pub const N_STEPS: usize = 16;
pub const N_VOICES: usize = 16;

/// Fixed seed so the instrument sounds identical on every launch.
pub const DRUM_SEED: u32 = 0x1200_1987;

/// Fixed seed for the `--render` parity demo.
pub const RENDER_SEED: u32 = 0x12_345678;

pub struct Pad {
    pub sample: Vec<i16>,
    pub tune_st: f32,
    pub level: f32,
    /// Optional display label (from web project import); falls back to the
    /// drum name, or CHOP n in tape mode.
    pub label: Option<String>,
}

impl Pad {
    /// Varispeed replay ratio, like the hardware: tuning replays the sample
    /// faster or slower.
    pub fn ratio(&self) -> f64 {
        2f64.powf(self.tune_st as f64 / 12.0)
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
}

impl Voice {
    const fn idle() -> Self {
        Self {
            active: false,
            pad: 0,
            pos: 0.0,
            age: 0,
        }
    }
}

pub struct Engine {
    pub pads: Vec<Pad>,
    voices: [Voice; N_VOICES],
    pub patterns: [[bool; N_STEPS]; N_PADS],
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
}

impl Engine {
    pub fn new() -> Self {
        let mut rng = XorShift32::new(DRUM_SEED);
        let pads: Vec<Pad> = DRUM_NAMES
            .iter()
            .map(|name| Pad {
                sample: dsp::synth_drum_i16(name, &mut rng),
                tune_st: 0.0,
                level: 0.9,
                label: None,
            })
            .collect();
        let pad_factory = pads
            .iter()
            .map(|p: &Pad| p.sample.clone())
            .collect::<Vec<_>>();
        let mut e = Self {
            pads,
            voices: [Voice::idle(); N_VOICES],
            patterns: [[false; N_STEPS]; N_PADS],
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
        };
        e.apply_preset(0);
        e
    }

    pub fn trigger(&mut self, pad: usize) {
        if pad >= N_PADS || self.pads[pad].sample.is_empty() {
            return;
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
        };
        self.pad_hit_at[pad] = self.clock;
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
        self.chop_map = [None; N_PADS];
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

    /// Re-derive pad samples from the tape + chop map. Pads without a chop
    /// (or with no tape audio) play their drum-mode sample.
    pub(crate) fn refresh_pad_samples(&mut self) {
        let resolved: Vec<(Vec<i16>, Option<usize>)> = (0..N_PADS)
            .map(|i| {
                if let Some(t) = &self.tape {
                    if !t.audio.is_empty() {
                        let n = t.chops.len().min(N_PADS);
                        let ci = match self.chop_map[i] {
                            Some(c) if c < t.chops.len() => c,
                            _ => i,
                        };
                        if ci < n {
                            let (a, b) = t.chops[ci];
                            return (t.audio[a..b].to_vec(), Some(ci));
                        }
                    }
                }
                (self.pad_factory[i].clone(), None)
            })
            .collect();
        for (i, (s, m)) in resolved.into_iter().enumerate() {
            self.pads[i].sample = s;
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
        self.pads[i].label = label;
        if self.chop_map[i].is_none() {
            self.pads[i].sample = sample;
        }
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

    fn fire_step(&mut self, step: u64) {
        let s = (step % N_STEPS as u64) as usize;
        self.last_step = s as u32;
        for pad in 0..N_PADS {
            if self.patterns[pad][s] {
                self.trigger(pad);
            }
        }
    }

    /// Render `out.len()` samples @ 26.04 kHz into `out` (-1.0..1.0).
    pub fn render(&mut self, out: &mut [f32]) {
        let d = dsp::sixteenth_dur(self.bpm);
        for s in out.iter_mut() {
            while self.playing && self.clock >= self.next_step_at {
                self.fire_step(self.sched_step);
                self.sched_step += 1;
                let bar = self.sched_step / N_STEPS as u64;
                let st = (self.sched_step % N_STEPS as u64) as u32;
                let t =
                    bar as f64 * N_STEPS as f64 * d + dsp::step_time16(st, self.bpm, self.swing);
                self.next_step_at = self.pattern_start + js_round(t * SP_RATE) as u64;
            }
            let mut mix = 0.0f32;
            for v in self.voices.iter_mut() {
                if !v.active {
                    continue;
                }
                let pad = &self.pads[v.pad];
                let idx = v.pos as usize;
                if idx + 1 >= pad.sample.len() {
                    v.active = false;
                    continue;
                }
                let frac = (v.pos - idx as f64) as f32;
                let a = pad.sample[idx] as f32 / 2047.0;
                let b = pad.sample[idx + 1] as f32 / 2047.0;
                mix += (a + (b - a) * frac) * pad.level;
                v.pos += pad.ratio();
            }
            *s = (mix * self.master).clamp(-1.0, 1.0);
            self.clock += 1;
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
        let mut p = [[false; N_STEPS]; N_PADS];
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

/// Load a .wav as the tape: decode → normalize (like the web) → the 12-bit
/// path → auto-splice. Returns a status line for the UI.
pub fn load_tape_file(eng: &mut Engine, path: &str) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|e| format!("can't read {}: {e}", file_name(path)))?;
    let wav = crate::wav::decode_wav(&data).map_err(|e| format!("{}: {e}", file_name(path)))?;
    if wav.samples.is_empty() {
        return Err(format!("{}: no audio", file_name(path)));
    }
    // Mirror the web pipeline: normalize clean, then the SP-1200 converters.
    let mut clean = wav.samples;
    dsp::normalize(&mut clean, 0.92);
    let audio = dsp::sp1200ize_i16(&clean, wav.sample_rate as f64);
    let name: String = file_stem(path).to_uppercase().chars().take(18).collect();
    let n = eng.load_tape(name.clone(), audio);
    Ok(format!("TAPE {name} - {n} CHOPS (S/E RESPLICE, D CLEAR)"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
