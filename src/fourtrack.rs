//! 4-track tape: record live input (the audio capture device) or bounce
//! the pattern mix (MASTER) onto four mono tracks, then play them back in
//! sync.
//!
//! Tracks are f32 mono at [`crate::dsp::SP_RATE`]. The tape transport is
//! independent of the pattern sequencer — start both to bounce a pattern.
//! Recording is punch-in style: an armed track's old audio is replaced from
//! the playhead, and while recording, armed tracks are *not* played back
//! (you hear the input / master instead, so there's no feedback).

use crate::dsp::SP_RATE;

pub const N_TRACKS: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrackSource {
    /// The audio capture device (mic/line-in), resampled to SP_RATE.
    /// Records silence when no input device is present.
    Input,
    /// The pattern/voice mix, captured before the 4-track playback is
    /// mixed in — bouncing can never feed back on itself.
    Master,
}

impl TrackSource {
    pub fn toggle(self) -> Self {
        match self {
            TrackSource::Input => TrackSource::Master,
            TrackSource::Master => TrackSource::Input,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TrackSource::Input => "INPUT",
            TrackSource::Master => "MASTER",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "input" => TrackSource::Input,
            _ => TrackSource::Master,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TrackSource::Input => "input",
            TrackSource::Master => "master",
        }
    }
}

pub struct Track {
    /// Mono audio @ SP_RATE.
    pub samples: Vec<f32>,
    pub armed: bool,
    pub muted: bool,
    pub source: TrackSource,
    /// Playback window in samples: only [trim_start, trim_end) sounds.
    /// trim_end None = to the end of the track. Recording ignores the
    /// window (it is a take, not playback); trimming never deletes audio.
    pub trim_start: usize,
    pub trim_end: Option<usize>,
}

impl Track {
    fn new() -> Self {
        Self {
            samples: Vec::new(),
            armed: false,
            muted: false,
            source: TrackSource::Master,
            trim_start: 0,
            trim_end: None,
        }
    }

    pub fn len_secs(&self) -> f32 {
        self.samples.len() as f32 / SP_RATE as f32
    }

    /// End of the playback window, clamped to the track.
    pub fn trim_end_samples(&self) -> usize {
        self.trim_end.unwrap_or(self.samples.len()).min(self.samples.len())
    }

    /// The audible region: [trim_start, trim_end), empty when the window
    /// is degenerate or the track is empty.
    pub fn trimmed_region(&self) -> &[f32] {
        let a = self.trim_start.min(self.samples.len());
        let b = self.trim_end_samples().max(a);
        &self.samples[a..b]
    }

    /// True when a trim window is actually narrowing playback.
    pub fn is_trimmed(&self) -> bool {
        self.trim_start > 0
            || self
                .trim_end
                .map(|e| e < self.samples.len())
                .unwrap_or(false)
    }

    /// One-line trim readout for the UI: "FULL" or "1.2-4.5S".
    pub fn trim_label(&self) -> String {
        if !self.is_trimmed() {
            return "FULL".to_string();
        }
        let a = self.trim_start as f32 / SP_RATE as f32;
        let b = self.trim_end_samples() as f32 / SP_RATE as f32;
        format!("{a:.1}-{b:.1}S")
    }

    /// Set the playback window in samples. Clamped to the track;
    /// start >= end collapses to silence (never panics).
    pub fn set_trim(&mut self, start: usize, end: Option<usize>) {
        let n = self.samples.len();
        self.trim_start = start.min(n);
        self.trim_end = end.map(|e| e.min(n));
    }

    pub fn reset_trim(&mut self) {
        self.trim_start = 0;
        self.trim_end = None;
    }

    /// Nudge the window edges by `delta` samples (negative allowed),
    /// keeping start <= end and both inside the track.
    pub fn nudge_trim_start(&mut self, delta: isize) {
        let end = self.trim_end_samples();
        let v = self.trim_start as isize + delta;
        self.trim_start = v.clamp(0, end as isize) as usize;
    }

    pub fn nudge_trim_end(&mut self, delta: isize) {
        let start = self.trim_start;
        let base = self.trim_end_samples() as isize + delta;
        let v = base.clamp(start as isize, self.samples.len() as isize) as usize;
        self.trim_end = Some(v);
    }
}

pub struct FourTrack {
    pub tracks: [Track; N_TRACKS],
    pub selected: usize,
    /// Playhead in samples; shared by all tracks like a real tape.
    pub playhead: usize,
    pub playing: bool,
    pub recording: bool,
    /// Loop the tape instead of stopping at the end.
    pub loop_enabled: bool,
    /// Loop length in samples. Tracks the longest track while not
    /// recording; frozen when recording starts so a first take can
    /// define its own length and overdubs wrap around existing material.
    pub loop_len: usize,
    /// Tape-bus character, 0.0..1.0 (shown as 0-100%). Applied to the
    /// tape playback/monitor mix only — never baked into takes.
    /// Warmth is asymmetric tube-style saturation (even harmonics) plus
    /// gentle high-end softening; distortion is symmetric tape-style
    /// saturation. Both bypass bit-exactly at 0.
    pub warmth: f32,
    pub distortion: f32,
    /// Warmth-stage filter state (DC blocker + one-pole lowpass). These
    /// advance on every bus sample even while warmth is bypassed, so
    /// re-enabling it never clicks.
    warm_dc_in: f32,
    warm_dc_out: f32,
    warm_lp: f32,
}

impl FourTrack {
    pub fn new() -> Self {
        Self {
            tracks: [(); N_TRACKS].map(|_| Track::new()),
            selected: 0,
            playhead: 0,
            playing: false,
            recording: false,
            loop_enabled: false,
            loop_len: 0,
            warmth: 0.0,
            distortion: 0.0,
            warm_dc_in: 0.0,
            warm_dc_out: 0.0,
            warm_lp: 0.0,
        }
    }

    /// Set tube warmth in percent (0-100), clamped.
    pub fn set_warmth(&mut self, pct: f32) {
        self.warmth = (pct / 100.0).clamp(0.0, 1.0);
    }

    /// Set tape distortion in percent (0-100), clamped.
    pub fn set_distortion(&mut self, pct: f32) {
        self.distortion = (pct / 100.0).clamp(0.0, 1.0);
    }

    /// Nudge tube warmth by delta percent, returning the new percent.
    pub fn nudge_warmth(&mut self, delta_pct: f32) -> f32 {
        self.set_warmth(self.warmth * 100.0 + delta_pct);
        self.warmth * 100.0
    }

    /// Nudge tape distortion by delta percent, returning the new percent.
    pub fn nudge_distortion(&mut self, delta_pct: f32) -> f32 {
        self.set_distortion(self.distortion * 100.0 + delta_pct);
        self.distortion * 100.0
    }

    /// Tape-bus processing for one mix sample: tube warmth (asymmetric
    /// saturation + DC block + softening lowpass) into tape distortion
    /// (symmetric saturation). At 0/0 this returns the input untouched.
    fn tape_bus(&mut self, x: f32) -> f32 {
        // Warmth stage always advances its filter state so toggling it
        // on mid-stream never clicks; its output is used only when on.
        let warmed = self.warm_stage(x);
        let mut y = if self.warmth > 0.0 { warmed } else { x };
        if self.distortion > 0.0 {
            let drive = 1.0 + self.distortion * 7.0;
            y = (y * drive).tanh() / drive;
        }
        y
    }

    /// Asymmetric saturator: the x^2 term adds even harmonics (tube
    /// character), the tanh bounds it, and dividing by drive keeps
    /// small-signal gain at 1. Followed by a DC blocker (the asymmetry
    /// rectifies a DC offset, as a real coupling cap would remove) and
    /// a lowpass that darkens from 18 kHz to 8 kHz across the range.
    fn warm_stage(&mut self, x: f32) -> f32 {
        let w = self.warmth;
        let drive = 1.0 + w * 2.0;
        let v = (x * drive + w * 0.35 * x * x).tanh() / drive;
        let dc = v - self.warm_dc_in + 0.995 * self.warm_dc_out;
        self.warm_dc_in = v;
        self.warm_dc_out = dc;
        let fc = 18000.0 - 10000.0 * w;
        let a = 1.0 - (-2.0 * std::f32::consts::PI * fc / SP_RATE as f32).exp();
        self.warm_lp += a * (dc - self.warm_lp);
        self.warm_lp
    }

    pub fn select(&mut self, i: usize) {
        if i < N_TRACKS {
            self.selected = i;
        }
    }

    pub fn toggle_arm(&mut self) -> String {
        let t = &mut self.tracks[self.selected];
        t.armed = !t.armed;
        format!(
            "TRACK {} {}",
            self.selected + 1,
            if t.armed { "ARMED" } else { "SAFE" }
        )
    }

    pub fn toggle_mute(&mut self) -> String {
        let t = &mut self.tracks[self.selected];
        t.muted = !t.muted;
        format!(
            "TRACK {} {}",
            self.selected + 1,
            if t.muted { "MUTED" } else { "UNMUTED" }
        )
    }

    pub fn toggle_source(&mut self) -> String {
        let t = &mut self.tracks[self.selected];
        t.source = t.source.toggle();
        format!("TRACK {} SRC {}", self.selected + 1, t.source.label())
    }

    pub fn clear_selected(&mut self) -> String {
        let t = &mut self.tracks[self.selected];
        t.samples.clear();
        t.reset_trim();
        format!("TRACK {} CLEARED", self.selected + 1)
    }

    pub fn toggle_play(&mut self) -> String {
        self.playing = !self.playing;
        if !self.playing {
            self.recording = false;
        }
        if self.playing && !self.recording {
            self.loop_len = self.longest();
        }
        if self.playing {
            "TAPE PLAY".to_string()
        } else {
            "TAPE STOP".to_string()
        }
    }

    pub fn toggle_loop(&mut self) -> String {
        self.loop_enabled = !self.loop_enabled;
        if self.loop_enabled {
            if !self.recording {
                self.loop_len = self.longest();
            }
            if self.loop_len == 0 {
                "LOOP ON".to_string()
            } else {
                format!("LOOP ON {:.1}S", self.loop_len_secs())
            }
        } else {
            "LOOP OFF".to_string()
        }
    }

    pub fn longest(&self) -> usize {
        self.tracks.iter().map(|t| t.samples.len()).max().unwrap_or(0)
    }

    pub fn loop_len_secs(&self) -> f32 {
        self.loop_len as f32 / SP_RATE as f32
    }

    pub fn toggle_record(&mut self) -> Result<String, String> {
        if self.recording {
            self.recording = false;
            self.loop_len = self.longest();
            return Ok("REC STOP - TAKE KEPT".to_string());
        }
        if !self.tracks.iter().any(|t| t.armed) {
            return Err("ARM A TRACK FIRST (A)".to_string());
        }
        self.recording = true;
        self.playing = true;
        Ok("REC".to_string())
    }

    /// Universal record/stop: one button for all four tracks. Starting
    /// arms every track (no per-track arming needed) and records; stopping
    /// keeps every take and safes all tracks, so a stray `R` can't
    /// punch over them afterwards. Stops any recording, universal or not.
    pub fn toggle_record_all(&mut self) -> String {
        if self.recording {
            self.recording = false;
            self.loop_len = self.longest();
            for t in self.tracks.iter_mut() {
                t.armed = false;
            }
            return "REC STOP - ALL SAFE".to_string();
        }
        for t in self.tracks.iter_mut() {
            t.armed = true;
        }
        self.recording = true;
        self.playing = true;
        "REC ALL".to_string()
    }

    pub fn stop_rewind(&mut self) -> String {
        self.playing = false;
        self.recording = false;
        self.playhead = 0;
        "TAPE REWOUND".to_string()
    }

    pub fn status(&self) -> &'static str {
        if self.recording {
            "REC"
        } else if self.playing {
            "PLAY"
        } else {
            "STOP"
        }
    }

    /// Mix track playback into `out` and record armed tracks.
    ///
    /// `input`: the resampled capture block (empty = silence when no
    /// device). `master`: the voice/pattern mix for this block, or `None`
    /// when no MASTER-armed track is recording.
    ///
    /// When looping, the block is split at the loop boundary so the
    /// playhead wraps mid-block with no dropout; a recording take wraps
    /// too, punching in around the loop.
    pub fn process(&mut self, out: &mut [f32], input: &[f32], master: Option<&[f32]>) {
        if !self.playing && !self.recording {
            return;
        }
        let n = out.len();
        let recording = self.recording;
        let playing = self.playing;
        // Tape-bus FX run on the tape mix alone (never the pattern
        // voices already in `out`). When both are off, accumulate
        // straight into `out` as before, so the bypass is bit-identical.
        let fx_on = self.warmth > 0.0 || self.distortion > 0.0;
        let mut mix = if fx_on { vec![0.0f32; n] } else { Vec::new() };
        let acc: &mut [f32] = if fx_on { &mut mix } else { out };
        if !recording {
            self.loop_len = self.longest();
        }
        let loop_on = self.loop_enabled && self.loop_len > 0;
        let loop_len = self.loop_len;

        let mut off = 0;
        let mut pos = self.playhead;
        while off < n {
            if loop_on && pos >= loop_len {
                pos = 0;
            }
            // One segment: up to the loop end, or the rest of the block.
            let seg = if loop_on { (loop_len - pos).min(n - off) } else { n - off };

            if playing {
                for t in self.tracks.iter() {
                    if t.muted || t.samples.is_empty() {
                        continue;
                    }
                    if recording && t.armed {
                        continue; // punch-in: monitor the input instead
                    }
                    // Trim window: only the overlap of this segment
                    // with [trim_start, trim_end) sounds.
                    let (ts, te) = (t.trim_start, t.trim_end_samples());
                    let play_from = pos.max(ts);
                    let play_to = (pos + seg).min(te);
                    if play_from < play_to {
                        let skip = play_from - pos;
                        let avail = play_to - play_from;
                        for i in 0..avail {
                            acc[off + skip + i] += t.samples[play_from + i];
                        }
                    }
                }
            }
            if recording {
                let mut monitoring_input = false;
                for t in self.tracks.iter_mut() {
                    if !t.armed {
                        continue;
                    }
                    let src: &[f32] = match t.source {
                        TrackSource::Input => {
                            monitoring_input = true;
                            input
                        }
                        TrackSource::Master => master.unwrap_or(&[]),
                    };
                    if t.samples.len() < pos + seg {
                        t.samples.resize(pos + seg, 0.0);
                    }
                    for i in 0..seg {
                        t.samples[pos + i] = src.get(off + i).copied().unwrap_or(0.0);
                    }
                }
                // Monitor live input while recording it.
                if monitoring_input {
                    for i in 0..seg {
                        acc[off + i] += input.get(off + i).copied().unwrap_or(0.0);
                    }
                }
            }
            off += seg;
            pos += seg;
        }
        self.playhead = pos;

        // Fold the effected tape mix into the output.
        if fx_on {
            for i in 0..n {
                out[i] += self.tape_bus(mix[i]);
            }
        }

        if !loop_on && playing && !recording && self.playhead >= self.longest() {
            self.playing = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn master_block(n: usize, v: f32) -> Vec<f32> {
        vec![v; n]
    }

    #[test]
    fn record_master_then_play_back() {
        let mut ft = FourTrack::new();
        ft.toggle_arm(); // track 1, source MASTER
        ft.toggle_record().unwrap();
        // Feed three blocks of "pattern".
        for _ in 0..3 {
            let m = master_block(100, 0.5);
            let mut out = vec![0.0f32; 100];
            ft.process(&mut out, &[], Some(&m));
        }
        assert_eq!(ft.tracks[0].samples.len(), 300);
        assert!(ft.tracks[0].samples.iter().all(|&s| (s - 0.5).abs() < 1e-6));
        ft.toggle_record().unwrap(); // stop recording, keep playing
        ft.stop_rewind();
        ft.toggle_play();
        // Playback: out should now carry the recorded audio.
        let mut out = vec![0.0f32; 100];
        ft.process(&mut out, &[], None);
        assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn mute_silences_track() {
        let mut ft = FourTrack::new();
        ft.tracks[0].samples = vec![0.5; 200];
        ft.select(0);
        ft.toggle_mute();
        ft.toggle_play();
        let mut out = vec![0.0f32; 100];
        ft.process(&mut out, &[], None);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn punch_in_replaces_and_skips_playback() {
        let mut ft = FourTrack::new();
        ft.tracks[0].samples = vec![0.5; 200];
        ft.toggle_arm();
        ft.playhead = 0;
        ft.toggle_record().unwrap(); // playing + recording
                                     // Old audio must NOT play back while punching in...
        let mut out = vec![0.0f32; 100];
        let m = master_block(100, 0.1);
        ft.process(&mut out, &[], Some(&m));
        assert!(out.iter().all(|&s| s == 0.0));
        // ...and the new take replaces the old from the playhead.
        assert!((ft.tracks[0].samples[0] - 0.1).abs() < 1e-6);
        assert!((ft.tracks[0].samples[150] - 0.5).abs() < 1e-6); // untouched tail
    }

    #[test]
    fn input_records_and_monitors() {
        let mut ft = FourTrack::new();
        ft.toggle_arm();
        ft.toggle_source(); // -> INPUT
        assert_eq!(ft.tracks[0].source, TrackSource::Input);
        ft.toggle_record().unwrap();
        let input = vec![0.3f32; 100];
        let mut out = vec![0.0f32; 100];
        ft.process(&mut out, &input, None);
        assert!(ft.tracks[0].samples.iter().all(|&s| (s - 0.3).abs() < 1e-6));
        assert!(out.iter().all(|&s| (s - 0.3).abs() < 1e-6)); // monitored
    }

    #[test]
    fn playback_auto_stops_at_end() {
        let mut ft = FourTrack::new();
        ft.tracks[0].samples = vec![0.5; 150];
        ft.toggle_play();
        let mut out = vec![0.0f32; 100];
        ft.process(&mut out, &[], None);
        assert!(ft.playing);
        ft.process(&mut out, &[], None);
        assert!(!ft.playing);
        assert_eq!(ft.playhead, 200);
    }

    #[test]
    fn record_needs_armed_track() {
        let mut ft = FourTrack::new();
        assert!(ft.toggle_record().is_err());
        assert!(!ft.recording);
    }

    #[test]
    fn loop_wraps_playback_without_dropout() {
        let mut ft = FourTrack::new();
        // Distinctive ramp so wrap-around is detectable in the output.
        ft.tracks[0].samples = (0..300).map(|i| i as f32 / 300.0).collect();
        ft.toggle_loop();
        assert!(ft.loop_enabled);
        assert_eq!(ft.loop_len, 300);
        ft.toggle_play();
        // Two passes over the loop with a block size that straddles the
        // boundary: output must equal the ramp repeated, sample-exact.
        let mut out = vec![0.0f32; 700];
        ft.process(&mut out, &[], None);
        for i in 0..700 {
            let want = (i % 300) as f32 / 300.0;
            assert!((out[i] - want).abs() < 1e-6, "i={i}");
        }
        assert!(ft.playing, "looping never auto-stops");
        assert_eq!(ft.playhead, 100);
    }

    #[test]
    fn loop_record_overdubs_around() {
        let mut ft = FourTrack::new();
        ft.tracks[0].samples = vec![0.5; 300];
        ft.toggle_arm();
        ft.toggle_loop();
        assert_eq!(ft.loop_len, 300);
        ft.toggle_record().unwrap();
        // Record 400 samples of 0.1: wraps at 300, keeps punching in.
        let m = master_block(400, 0.1);
        let mut out = vec![0.0f32; 400];
        ft.process(&mut out, &[], Some(&m));
        assert_eq!(ft.playhead, 100);
        assert_eq!(ft.tracks[0].samples.len(), 300, "take does not grow past the loop");
        assert!(ft.tracks[0].samples.iter().all(|&s| (s - 0.1).abs() < 1e-6));
    }

    #[test]
    fn loop_len_frozen_while_recording_first_take() {
        let mut ft = FourTrack::new();
        ft.toggle_arm();
        ft.toggle_loop(); // no audio yet: loop armed but length 0
        assert_eq!(ft.loop_len, 0);
        ft.toggle_record().unwrap();
        // First take records linearly, no wrap.
        for _ in 0..3 {
            let m = master_block(100, 0.5);
            let mut out = vec![0.0f32; 100];
            ft.process(&mut out, &[], Some(&m));
        }
        assert_eq!(ft.tracks[0].samples.len(), 300);
        assert_eq!(ft.playhead, 300);
        ft.toggle_record().unwrap(); // stop; still playing -> loop picks up the take
        assert_eq!(ft.loop_len, 300);
        assert!(ft.playing);
        let mut out = vec![0.0f32; 100];
        ft.process(&mut out, &[], None);
        assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-6), "loops the fresh take");
        assert!(ft.playing);
    }

    #[test]
    fn loop_toggle_captures_current_longest() {
        let mut ft = FourTrack::new();
        ft.tracks[0].samples = vec![0.5; 150];
        ft.tracks[1].samples = vec![0.5; 400];
        ft.toggle_loop();
        assert_eq!(ft.loop_len, 400);
        ft.toggle_loop();
        assert!(!ft.loop_enabled);
    }

    #[test]
    fn idle_process_is_noop() {
        let mut ft = FourTrack::new();
        let mut out = vec![0.25f32; 64];
        ft.process(&mut out, &[], None);
        assert!(out.iter().all(|&s| s == 0.25));
        assert_eq!(ft.playhead, 0);
    }

    #[test]
    fn record_all_arms_every_track_and_stop_safes_all() {
        let mut ft = FourTrack::new();
        let m = ft.toggle_record_all();
        assert_eq!(m, "REC ALL");
        assert!(ft.recording && ft.playing);
        assert!(ft.tracks.iter().all(|t| t.armed));
        let block = master_block(100, 0.25);
        let mut out = vec![0.0f32; 100];
        ft.process(&mut out, &[], Some(&block));
        for t in ft.tracks.iter() {
            assert_eq!(t.samples.len(), 100);
            assert!(t.samples.iter().all(|&s| (s - 0.25).abs() < 1e-6));
        }
        let m = ft.toggle_record_all();
        assert_eq!(m, "REC STOP - ALL SAFE");
        assert!(!ft.recording);
        assert!(ft.tracks.iter().all(|t| !t.armed));
        assert!(ft.tracks.iter().all(|t| t.samples.len() == 100)); // takes kept
    }

    #[test]
    fn record_all_stops_a_normal_record_too() {
        let mut ft = FourTrack::new();
        ft.toggle_arm(); // track 1 only
        ft.toggle_record().unwrap();
        assert!(ft.recording);
        ft.toggle_record_all();
        assert!(!ft.recording);
        assert!(ft.tracks.iter().all(|t| !t.armed));
    }

    #[test]
    fn trim_window_gates_playback() {
        let mut ft = FourTrack::new();
        ft.tracks[0].samples = vec![0.5; 300];
        ft.tracks[0].set_trim(100, Some(200));
        ft.toggle_play();
        let mut out = vec![0.0f32; 300];
        ft.process(&mut out, &[], None);
        assert!(out[..100].iter().all(|&s| s == 0.0), "before window: silent");
        assert!(out[100..200].iter().all(|&s| (s - 0.5).abs() < 1e-6));
        assert!(out[200..].iter().all(|&s| s == 0.0), "after window: silent");
    }

    #[test]
    fn trim_nudges_clamp_and_clear_resets() {
        let mut ft = FourTrack::new();
        ft.tracks[0].samples = vec![0.5; 300];
        let t = &mut ft.tracks[0];
        t.nudge_trim_start(400); // past the end: clamps to end
        assert_eq!(t.trim_start, 300);
        t.nudge_trim_start(-500); // below zero: clamps to 0
        assert_eq!(t.trim_start, 0);
        t.nudge_trim_end(-500); // below start: clamps to start
        assert_eq!(t.trim_end, Some(0));
        t.nudge_trim_end(9999); // past the track: clamps to len
        assert_eq!(t.trim_end, Some(300));
        assert_eq!(t.trim_label(), "FULL"); // 0..len reads as full
        t.set_trim(50, Some(150));
        assert!(t.is_trimmed());
        assert_eq!(t.trimmed_region().len(), 100);
        ft.clear_selected();
        assert_eq!(ft.tracks[0].trim_start, 0);
        assert_eq!(ft.tracks[0].trim_end, None);
        assert!(ft.tracks[0].samples.is_empty());
    }

    #[test]
    fn tape_bus_bypass_is_identity() {
        let mut ft = FourTrack::new();
        for x in [-2.0f32, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0] {
            assert_eq!(ft.tape_bus(x), x, "0/0 must pass {x} through untouched");
        }
    }

    #[test]
    fn warmth_is_asymmetric_and_saturates() {
        // Tube signature: positive and negative halves differ (even
        // harmonics), unlike symmetric saturation.
        let mut pos = FourTrack::new();
        pos.set_warmth(100.0);
        let a = pos.tape_bus(0.5);
        let mut neg = FourTrack::new();
        neg.set_warmth(100.0);
        let b = neg.tape_bus(-0.5);
        assert!(a != -b, "warmth must be asymmetric: {a} vs {b}");
        assert!(a < 0.5 && a > 0.1, "warmth saturates but keeps level: {a}");
        let mut hot = FourTrack::new();
        hot.set_warmth(100.0);
        assert!(hot.tape_bus(2.0) < 1.0, "hot signal squashed");
    }

    #[test]
    fn distortion_squashes_symmetrically() {
        let mut ft = FourTrack::new();
        ft.set_distortion(100.0);
        assert!(ft.tape_bus(2.0) < 0.2, "hot signal heavily squashed");
        let mut ft = FourTrack::new();
        ft.set_distortion(100.0);
        let s = ft.tape_bus(0.1);
        assert!(s > 0.05 && s < 0.15, "quiet signal stays near level: {s}");
        let mut pos = FourTrack::new();
        pos.set_distortion(100.0);
        let a = pos.tape_bus(0.5);
        let mut neg = FourTrack::new();
        neg.set_distortion(100.0);
        let b = neg.tape_bus(-0.5);
        assert!((a + b).abs() < 1e-6, "distortion stays symmetric: {a} vs {b}");
    }

    #[test]
    fn process_fx_changes_mix_but_never_bakes_into_takes() {
        // Playback mix through the bus.
        let mut ft = FourTrack::new();
        ft.tracks[0].samples = vec![0.5; 200];
        ft.toggle_play();
        let mut clean = vec![0.0f32; 100];
        ft.process(&mut clean, &[], None);
        assert!(clean.iter().all(|&s| (s - 0.5).abs() < 1e-6));
        ft.stop_rewind();
        ft.set_warmth(100.0);
        ft.toggle_play();
        let mut warm = vec![0.0f32; 100];
        ft.process(&mut warm, &[], None);
        assert!(
            warm.iter().any(|&s| (s - 0.5).abs() > 1e-3),
            "warmth must audibly change the mix"
        );
        // Takes stay clean: recording with warmth on still stores the
        // raw input, so the character is monitoring-only.
        let mut ft = FourTrack::new();
        ft.set_warmth(100.0);
        ft.set_distortion(100.0);
        ft.toggle_arm(); // track 1, MASTER source
        ft.toggle_record().unwrap();
        let m = master_block(100, 0.5);
        let mut out = vec![0.0f32; 100];
        ft.process(&mut out, &[], Some(&m));
        assert!(ft.tracks[0].samples.iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }
}
