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
}

impl FourTrack {
    pub fn new() -> Self {
        Self {
            tracks: [(); N_TRACKS].map(|_| Track::new()),
            selected: 0,
            playhead: 0,
            playing: false,
            recording: false,
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
        if self.playing {
            "TAPE PLAY".to_string()
        } else {
            "TAPE STOP".to_string()
        }
    }

    pub fn toggle_record(&mut self) -> Result<String, String> {
        if self.recording {
            self.recording = false;
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
    pub fn process(&mut self, out: &mut [f32], input: &[f32], master: Option<&[f32]>) {
        if !self.playing && !self.recording {
            return;
        }
        let n = out.len();
        if self.playing {
            let recording = self.recording;
            let playhead = self.playhead;
            for t in self.tracks.iter() {
                if t.muted || t.samples.is_empty() {
                    continue;
                }
                if recording && t.armed {
                    continue; // punch-in: monitor the input instead
                }
                if playhead < t.samples.len() {
                    let avail = (t.samples.len() - playhead).min(n);
                    for i in 0..avail {
                        out[i] += t.samples[playhead + i];
                    }
                }
            }
        }
        if self.recording {
            let playhead = self.playhead;
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
                if t.samples.len() < playhead + n {
                    t.samples.resize(playhead + n, 0.0);
                }
                for i in 0..n {
                    t.samples[playhead + i] = src.get(i).copied().unwrap_or(0.0);
                }
            }
            // Monitor live input while recording it.
            if monitoring_input {
                for i in 0..n {
                    out[i] += input.get(i).copied().unwrap_or(0.0);
                }
            }
        }
        self.playhead += n;
        if self.playing && !self.recording {
            let longest = self
                .tracks
                .iter()
                .map(|t| t.samples.len())
                .max()
                .unwrap_or(0);
            if self.playhead >= longest {
                self.playing = false;
            }
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
    fn idle_process_is_noop() {
        let mut ft = FourTrack::new();
        let mut out = vec![0.25f32; 64];
        ft.process(&mut out, &[], None);
        assert!(out.iter().all(|&s| s == 0.25));
        assert_eq!(ft.playhead, 0);
    }
}
