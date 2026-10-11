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
}

impl Track {
    fn new() -> Self {
        Self {
            samples: Vec::new(),
            armed: false,
            muted: false,
            source: TrackSource::Master,
        }
    }

    pub fn len_secs(&self) -> f32 {
        self.samples.len() as f32 / SP_RATE as f32
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
        }
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
                    if pos < t.samples.len() {
                        let avail = (t.samples.len() - pos).min(seg);
                        for i in 0..avail {
                            out[off + i] += t.samples[pos + i];
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
                        out[off + i] += input.get(off + i).copied().unwrap_or(0.0);
                    }
                }
            }
            off += seg;
            pos += seg;
        }
        self.playhead = pos;

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
}
