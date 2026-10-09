//! SDL2 audio: the engine renders at 26.04 kHz; the callback resamples
//! linearly to the device rate (44100 Hz requested for compatibility).

use sdl2::audio::{AudioCallback, AudioSpecDesired};
use std::sync::{Arc, Mutex};

use crate::dsp::SP_RATE;
use crate::engine::Engine;

pub struct SpCallback {
    engine: Arc<Mutex<Engine>>,
    dev_rate: f64,
    /// Persistent engine-rate stream; fresh samples are appended as needed.
    buf: Vec<f32>,
    /// Fractional read position into `buf`.
    read_pos: f64,
}

impl SpCallback {
    pub fn new(engine: Arc<Mutex<Engine>>, dev_rate: f64) -> Self {
        Self {
            engine,
            dev_rate,
            buf: Vec::with_capacity(8192),
            read_pos: 0.0,
        }
    }
}

impl AudioCallback for SpCallback {
    type Channel = f32;

    fn callback(&mut self, out: &mut [f32]) {
        let ratio = SP_RATE / self.dev_rate;
        // Make sure the samples we are about to read exist.
        let need_end = (self.read_pos + out.len() as f64 * ratio).ceil() as usize + 2;
        if need_end > self.buf.len() {
            let n = need_end - self.buf.len();
            let old = self.buf.len();
            self.buf.resize(need_end, 0.0);
            if let Ok(mut eng) = self.engine.lock() {
                eng.render(&mut self.buf[old..need_end]);
            }
            let _ = n;
        }
        let mut pos = self.read_pos;
        for s in out.iter_mut() {
            let i0 = pos.floor() as usize;
            let f = (pos - i0 as f64) as f32;
            *s = self.buf[i0] + (self.buf[i0 + 1] - self.buf[i0]) * f;
            pos += ratio;
        }
        self.read_pos = pos;
        // Compact fully-consumed samples, keeping 2 for interpolation.
        let drop = (self.read_pos as usize).saturating_sub(2);
        if drop > 0 {
            self.buf.drain(..drop);
            self.read_pos -= drop as f64;
        }
    }
}

/// Open the default playback device at 44.1 kHz mono and start it.
pub fn open_device(
    audio: &sdl2::AudioSubsystem,
    engine: Arc<Mutex<Engine>>,
) -> Result<sdl2::audio::AudioDevice<SpCallback>, String> {
    let spec = AudioSpecDesired {
        freq: Some(44100),
        channels: Some(1),
        samples: Some(1024),
    };
    let device = audio.open_playback(None, &spec, |spec| {
        SpCallback::new(engine, spec.freq as f64)
    })?;
    device.resume();
    Ok(device)
}
