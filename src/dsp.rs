//! SP-1200 DSP core: drum synthesis + the SP-1200 signal path + swing timing.
//! Faithful Rust port of `public/dsp.js` from the web version.
//!
//! Type discipline (this is what makes byte-parity possible):
//! - Sample buffers are `f32`, exactly like JS `Float32Array`.
//! - All scalar math is `f64`, exactly like JS numbers.
//! - Every read-modify-write on a buffer (`out[i] += …`, `x[i] *= …`) is
//!   replicated as f32→f64→f32, because JS stores the intermediate.
//! - `Math.round` is replicated as `(x + 0.5).floor()` ([`js_round`]).

use std::f64::consts::PI;

/// The SP-1200's converters ran at 26.04 kHz, 12 bits.
pub const SP_RATE: f64 = 26040.0;
#[allow(dead_code)] // ported API surface (mirrors dsp.js); wired up by future sample-editor work
pub const SP_BITS: u32 = 12;
/// Drums are synthesized clean at 44.1 kHz, then "sampled" by the SP path.
pub const SYNTH_RATE: f64 = 44100.0;
/// 12-bit scale: 2^(12-1) - 1.
pub const QUANT_SCALE: f64 = 2047.0;

/// Deterministic RNG interface. The JS parity harness monkey-patches
/// `Math.random` to the same XorShift32 algorithm + seed, so synth output
/// is reproducible across languages.
pub trait Rng {
    /// Next value in [0, 1).
    fn next_f64(&mut self) -> f64;
}

/// XorShift32 — tiny, deterministic, zero-dependency.
/// Bit-identical to the JS reference:
/// `s ^= s << 13; s ^= s >>> 17; s ^= s << 5; return (s >>> 0) / 4294967296`.
#[derive(Clone, Debug)]
pub struct XorShift32 {
    state: u32,
}

impl XorShift32 {
    pub fn new(seed: u32) -> Self {
        Self { state: seed }
    }
}

impl Rng for XorShift32 {
    fn next_f64(&mut self) -> f64 {
        let mut x = self.state;
        x ^= x.wrapping_shl(13);
        x ^= x.wrapping_shr(17);
        x ^= x.wrapping_shl(5);
        self.state = x;
        (x as f64) / 4294967296.0
    }
}

/// JS `Math.round` semantics: halves round toward +inf.
/// `(x + 0.5).floor()` matches it for every finite f64 in our domain.
#[inline]
pub fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// JS `Math.sign` for non-NaN inputs.
#[inline]
fn js_sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

// ---------- tiny DSP utilities ----------

pub fn make_noise(n: usize, rng: &mut dyn Rng) -> Vec<f32> {
    (0..n)
        .map(|_| (rng.next_f64() * 2.0 - 1.0) as f32)
        .collect()
}

pub fn lowpass(x: &[f32], sr: f64, cutoff: f64) -> Vec<f32> {
    let rc = 1.0 / (2.0 * PI * cutoff);
    let dt = 1.0 / sr;
    let a = dt / (rc + dt);
    let mut y = Vec::with_capacity(x.len());
    let mut prev = 0.0f64;
    for &xi in x {
        prev += a * (xi as f64 - prev);
        y.push(prev as f32);
    }
    y
}

pub fn highpass(x: &[f32], sr: f64, cutoff: f64) -> Vec<f32> {
    let rc = 1.0 / (2.0 * PI * cutoff);
    let dt = 1.0 / sr;
    let a = rc / (rc + dt);
    let mut y = Vec::with_capacity(x.len());
    let mut prev_y = 0.0f64;
    let mut prev_x = 0.0f64;
    for &xi in x {
        let v = a * (prev_y + xi as f64 - prev_x);
        y.push(v as f32);
        prev_y = v;
        prev_x = xi as f64;
    }
    y
}

pub fn normalize(x: &mut [f32], peak: f64) {
    let mut m = 0.0f64;
    for &xi in x.iter() {
        let a = (xi as f64).abs();
        if a > m {
            m = a;
        }
    }
    if m > 0.0 {
        let g = peak / m;
        for xi in x.iter_mut() {
            // JS `x[i] *= g`: f32 -> f64 -> f32.
            *xi = (*xi as f64 * g) as f32;
        }
    }
}

// ---------- the SP-1200 signal path ----------

pub fn resample_linear(input: &[f32], from_rate: f64, to_rate: f64) -> Vec<f32> {
    if from_rate == to_rate {
        return input.to_vec();
    }
    let ratio = from_rate / to_rate;
    let n = ((input.len() as f64 / ratio).floor() as usize).max(1);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let pos = i as f64 * ratio;
        let i0 = pos.floor() as usize;
        let i1 = (i0 + 1).min(input.len() - 1);
        let f = pos - i0 as f64;
        let v = input[i0] as f64 * (1.0 - f) + input[i1] as f64 * f;
        out.push(v as f32);
    }
    out
}

/// 12-bit quantization, the SP's crunch.
#[allow(dead_code)] // ported API surface (mirrors dsp.js); wired up by future sample-editor work
pub fn quantize12(input: &[f32]) -> Vec<f32> {
    input
        .iter()
        .map(|&xi| {
            let v = (xi as f64).clamp(-1.0, 1.0);
            (js_round(v * QUANT_SCALE) / QUANT_SCALE) as f32
        })
        .collect()
}

/// Memory-light twin: the same 12-bit values as i16. Divide by 2047 to float.
pub fn quantize12_i16(input: &[f32]) -> Vec<i16> {
    input
        .iter()
        .map(|&xi| {
            let v = (xi as f64).clamp(-1.0, 1.0);
            js_round(v * QUANT_SCALE) as i16
        })
        .collect()
}

/// Run clean audio through the SP-1200's converters: 26.04 kHz, 12-bit.
#[allow(dead_code)] // ported API surface (mirrors dsp.js); wired up by future sample-editor work
pub fn sp1200ize(input: &[f32], from_rate: f64) -> Vec<f32> {
    quantize12(&resample_linear(input, from_rate, SP_RATE))
}

/// Int16 twin of [`sp1200ize`].
pub fn sp1200ize_i16(input: &[f32], from_rate: f64) -> Vec<i16> {
    quantize12_i16(&resample_linear(input, from_rate, SP_RATE))
}

// ---------- drum synthesis (clean, then sampled by the SP path) ----------

pub fn synth_kick_raw(rng: &mut dyn Rng) -> Vec<f32> {
    let sr = SYNTH_RATE;
    let n = (sr * 0.5).floor() as usize;
    let mut out = vec![0.0f32; n];
    let mut phase = 0.0f64;
    for i in 0..n {
        let t = i as f64 / sr;
        let f = 46.0 + 150.0 * (-t * 42.0).exp();
        phase += (2.0 * PI * f) / sr;
        out[i] = (phase.sin() * (-t * 9.5).exp()) as f32;
        if t < 0.008 {
            // JS `out[i] += …`: replicate the f32 round-trip.
            out[i] =
                (out[i] as f64 + (rng.next_f64() * 2.0 - 1.0) * (-t * 500.0).exp() * 0.5) as f32;
        }
    }
    normalize(&mut out, 0.92);
    out
}

pub fn synth_snare_raw(rng: &mut dyn Rng) -> Vec<f32> {
    let sr = SYNTH_RATE;
    let n = (sr * 0.28).floor() as usize;
    let noise = highpass(&lowpass(&make_noise(n, rng), sr, 7000.0), sr, 900.0);
    let mut out = vec![0.0f32; n];
    for i in 0..n {
        let t = i as f64 / sr;
        let body = (2.0 * PI * 190.0 * t).sin() * (-t * 28.0).exp() * 0.55;
        out[i] = (body + noise[i] as f64 * (-t * 32.0).exp() * 0.5) as f32;
    }
    normalize(&mut out, 0.9);
    out
}

pub fn synth_clap_raw(rng: &mut dyn Rng) -> Vec<f32> {
    let sr = SYNTH_RATE;
    let n = (sr * 0.32).floor() as usize;
    let noise = highpass(&lowpass(&make_noise(n, rng), sr, 4500.0), sr, 700.0);
    let mut out = vec![0.0f32; n];
    let bursts = [0.0f64, 0.018, 0.034, 0.052];
    for i in 0..n {
        let t = i as f64 / sr;
        let mut v = 0.0f64;
        for &b in &bursts {
            if t >= b {
                v += (-(t - b) * 160.0).exp() * 0.5;
            }
        }
        v += (-t * 22.0).exp() * 0.35; // tail
        out[i] = (noise[i] as f64 * v.min(1.0)) as f32;
    }
    normalize(&mut out, 0.9);
    out
}

pub fn synth_rim_raw(rng: &mut dyn Rng) -> Vec<f32> {
    let sr = SYNTH_RATE;
    let n = (sr * 0.07).floor() as usize;
    let mut out = vec![0.0f32; n];
    for i in 0..n {
        let t = i as f64 / sr;
        let sq =
            js_sign((2.0 * PI * 1720.0 * t).sin()) * 0.35 + (2.0 * PI * 5160.0 * t).sin() * 0.12;
        out[i] = ((sq + (rng.next_f64() * 2.0 - 1.0) * 0.25) * (-t * 130.0).exp()) as f32;
    }
    normalize(&mut out, 0.85);
    out
}

fn hat_noise(sr: f64, dur: f64, decay: f64, rng: &mut dyn Rng) -> Vec<f32> {
    let n = (sr * dur).floor() as usize;
    let mut noise = make_noise(n, rng);
    noise = highpass(&noise, sr, 7800.0);
    noise = highpass(&noise, sr, 7800.0); // steeper, like the analog path
    for (i, s) in noise.iter_mut().enumerate() {
        // JS `noise[i] *= …`: replicate the f32 round-trip.
        *s = (*s as f64 * (-(i as f64 / sr) * decay).exp()) as f32;
    }
    noise
}

pub fn synth_chat_raw(rng: &mut dyn Rng) -> Vec<f32> {
    let mut out = hat_noise(SYNTH_RATE, 0.07, 95.0, rng);
    normalize(&mut out, 0.8);
    out
}

pub fn synth_ohat_raw(rng: &mut dyn Rng) -> Vec<f32> {
    let mut out = hat_noise(SYNTH_RATE, 0.4, 11.0, rng);
    normalize(&mut out, 0.8);
    out
}

pub fn synth_tom_raw(rng: &mut dyn Rng) -> Vec<f32> {
    let sr = SYNTH_RATE;
    let n = (sr * 0.42).floor() as usize;
    let mut out = vec![0.0f32; n];
    let mut phase = 0.0f64;
    for i in 0..n {
        let t = i as f64 / sr;
        let f = 88.0 + 110.0 * (-t * 18.0).exp();
        phase += (2.0 * PI * f) / sr;
        out[i] = (phase.sin() * (-t * 8.5).exp()) as f32;
        if t < 0.006 {
            out[i] =
                (out[i] as f64 + (rng.next_f64() * 2.0 - 1.0) * (-t * 600.0).exp() * 0.3) as f32;
        }
    }
    normalize(&mut out, 0.9);
    out
}

pub fn synth_shaker_raw(rng: &mut dyn Rng) -> Vec<f32> {
    let sr = SYNTH_RATE;
    let dur = 0.2f64;
    let n = (sr * dur).floor() as usize;
    let mut noise = highpass(&make_noise(n, rng), sr, 5200.0);
    for (i, s) in noise.iter_mut().enumerate() {
        let t = i as f64 / sr;
        let env = (-t * 26.0).exp() * (0.55 + 0.45 * (PI * (t / dur).min(1.0)).sin());
        *s = (*s as f64 * env) as f32;
    }
    normalize(&mut noise, 0.75);
    noise
}

/// The 8 drum voices, in pad order.
pub const DRUM_NAMES: [&str; 8] = [
    "kick", "snare", "clap", "rim", "chat", "ohat", "tom", "shaker",
];

/// Synthesize one drum through the full SP-1200 path, as i16 @ 26.04 kHz.
/// Consumes RNG in exactly the same order as the JS `SYNTHS_I16` entries.
pub fn synth_drum_i16(name: &str, rng: &mut dyn Rng) -> Vec<i16> {
    let raw = match name {
        "kick" => synth_kick_raw(rng),
        "snare" => synth_snare_raw(rng),
        "clap" => synth_clap_raw(rng),
        "rim" => synth_rim_raw(rng),
        "chat" => synth_chat_raw(rng),
        "ohat" => synth_ohat_raw(rng),
        "tom" => synth_tom_raw(rng),
        "shaker" => synth_shaker_raw(rng),
        _ => panic!("unknown drum: {name}"),
    };
    sp1200ize_i16(&raw, SYNTH_RATE)
}

// ---------- swing ----------

/// Duration of one 16th note in seconds.
pub fn sixteenth_dur(bpm: f64) -> f64 {
    60.0 / bpm / 4.0
}

/// Roger Linn-style swing: even 16ths stay on the grid, odd 16ths slide
/// late. swing_pct 50 = straight, 75 = full triplet-style lope.
pub fn step_time16(step: u32, bpm: f64, swing_pct: f64) -> f64 {
    let s = swing_pct.clamp(50.0, 75.0) / 100.0;
    let d = sixteenth_dur(bpm);
    let pair_start = (step / 2) as f64 * 2.0 * d;
    if step % 2 == 0 {
        pair_start
    } else {
        pair_start + s * 2.0 * d
    }
}

// ---------- sample editing (pure ops) ----------

/// Crop to a fractional selection [start_frac, end_frac).
#[allow(dead_code)] // ported API surface (mirrors dsp.js); wired up by future sample-editor work
pub fn trim_sample(x: &[f32], start_frac: f64, end_frac: f64) -> Vec<f32> {
    let n = x.len();
    let a = ((start_frac * n as f64).floor().clamp(0.0, n as f64)) as usize;
    let b = ((end_frac * n as f64).ceil() as usize).max(a + 1).min(n);
    x[a..b].to_vec()
}

#[allow(dead_code)] // ported API surface (mirrors dsp.js); wired up by future sample-editor work
pub fn reverse_sample(x: &[f32]) -> Vec<f32> {
    let mut out = x.to_vec();
    out.reverse();
    out
}

/// Linear fades; fractions of total length (0 = no fade on that side).
#[allow(dead_code)] // ported API surface (mirrors dsp.js); wired up by future sample-editor work
pub fn fade_sample(x: &[f32], fade_in_frac: f64, fade_out_frac: f64) -> Vec<f32> {
    let mut out = x.to_vec();
    let n = out.len();
    let fi = (fade_in_frac * n as f64).floor() as usize;
    let fo = (fade_out_frac * n as f64).floor() as usize;
    for i in 0..fi.min(n) {
        out[i] = (out[i] as f64 * i as f64 / (fi.max(1) as f64)) as f32;
    }
    for i in 0..fo.min(n) {
        let j = n - 1 - i;
        out[j] = (out[j] as f64 * i as f64 / (fo.max(1) as f64)) as f32;
    }
    out
}

// ---------- tape slicer: transient (onset) detection ----------

#[derive(Clone, Debug)]
#[allow(dead_code)] // ported API surface (mirrors dsp.js); wired up by future sample-editor work
pub struct OnsetOpts {
    pub window: usize,
    pub hop: usize,
    pub sensitivity: f64,
    pub min_gap_sec: f64,
}

impl Default for OnsetOpts {
    fn default() -> Self {
        Self {
            window: 1024,
            hop: 512,
            sensitivity: 1.5,
            min_gap_sec: 0.08,
        }
    }
}

/// Energy-flux onset detection: returns sample offsets where the signal's
/// short-time energy jumps.
#[allow(dead_code)] // ported API surface (mirrors dsp.js); wired up by future sample-editor work
pub fn detect_onsets(x: &[f32], sr: f64, opts: &OnsetOpts) -> Vec<usize> {
    let win = opts.window;
    let hop = opts.hop;
    if x.len() < win * 2 {
        return Vec::new();
    }
    // RMS energy per frame.
    let mut frames: Vec<f64> = Vec::new();
    let mut i = 0;
    while i + win <= x.len() {
        let mut e = 0.0f64;
        for j in i..i + win {
            e += x[j] as f64 * x[j] as f64;
        }
        frames.push((e / win as f64).sqrt());
        i += hop;
    }
    // Positive energy differences (flux), thresholded at mean * sensitivity.
    let mut flux = vec![0.0f64; frames.len()];
    for f in 1..frames.len() {
        flux[f] = (frames[f] - frames[f - 1]).max(0.0);
    }
    let mean = flux.iter().sum::<f64>() / flux.len() as f64;
    let thr = mean * opts.sensitivity + 1e-9;
    let min_gap_frames = (js_round((opts.min_gap_sec * sr) / hop as f64) as usize).max(1);

    let mut onsets = Vec::new();
    let mut last: isize = -(min_gap_frames as isize);
    for f in 1..flux.len() {
        if flux[f] > thr && (f as isize - last) >= min_gap_frames as isize {
            onsets.push(f * hop);
            last = f as isize;
        }
    }
    onsets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_round_matches_math_round() {
        assert_eq!(js_round(1023.5), 1024.0);
        assert_eq!(js_round(-1023.5), -1023.0);
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(0.4), 0.0);
        assert_eq!(js_round(-0.4), 0.0);
        assert_eq!(js_round(2047.0), 2047.0);
    }

    #[test]
    fn quantize_known_values() {
        let q = quantize12_i16(&[1.0, -1.0, 0.0, 0.5, -0.5]);
        assert_eq!(q, vec![2047, -2047, 0, 1024, -1023]);
        // clamping
        let q = quantize12_i16(&[2.0, -3.0]);
        assert_eq!(q, vec![2047, -2047]);
    }

    #[test]
    fn swing_math() {
        assert_eq!(sixteenth_dur(120.0), 0.125);
        // 50% = straight
        assert_eq!(step_time16(0, 120.0, 50.0), 0.0);
        assert_eq!(step_time16(1, 120.0, 50.0), 0.125);
        assert_eq!(step_time16(2, 120.0, 50.0), 0.25);
        // 75% = triplet lope: odd 16th lands at 1.5 sixteenths
        assert_eq!(step_time16(1, 120.0, 75.0), 0.1875);
        assert_eq!(step_time16(3, 120.0, 75.0), 0.4375);
        // clamped
        assert_eq!(step_time16(1, 120.0, 10.0), step_time16(1, 120.0, 50.0));
        assert_eq!(step_time16(1, 120.0, 99.0), step_time16(1, 120.0, 75.0));
    }

    #[test]
    fn resample_lengths_match_js() {
        // n = floor(len / (44100/26040)); spot-check a drum-sized buffer.
        let x = vec![0.5f32; 22050]; // kick raw length
        assert_eq!(resample_linear(&x, 44100.0, 26040.0).len(), 13020);
        let x = vec![0.5f32; 12348]; // snare raw length
        assert_eq!(resample_linear(&x, 44100.0, 26040.0).len(), 7291);
    }

    #[test]
    fn synth_output_lengths() {
        let mut rng = XorShift32::new(0x12345678);
        // (name, expected i16 length @26.04kHz)
        let expected = [
            ("kick", 13020),
            ("snare", 7291),
            ("clap", 8332),
            ("rim", 1822),
            ("chat", 1822),
            ("ohat", 10416),
            ("tom", 10936),
            ("shaker", 5208),
        ];
        for (name, len) in expected {
            let s = synth_drum_i16(name, &mut rng);
            assert_eq!(s.len(), len, "{name}");
            let peak = s.iter().map(|v| v.unsigned_abs() as i32).max().unwrap();
            assert!(peak > 100, "{name} silent (peak {peak})");
        }
    }

    #[test]
    fn xorshift_deterministic() {
        let mut a = XorShift32::new(42);
        let mut b = XorShift32::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_f64().to_bits(), b.next_f64().to_bits());
        }
        let mut c = XorShift32::new(43);
        assert_ne!(a.next_f64().to_bits(), c.next_f64().to_bits());
        // range check
        let mut r = XorShift32::new(7);
        for _ in 0..1000 {
            let v = r.next_f64();
            assert!(v >= 0.0 && v < 1.0);
        }
    }

    #[test]
    fn edit_ops() {
        let x: Vec<f32> = (0..100).map(|i| i as f32).collect();
        assert_eq!(
            trim_sample(&x, 0.1, 0.2),
            (10..20).map(|i| i as f32).collect::<Vec<_>>()
        );
        let r = reverse_sample(&[1.0f32, 2.0, 3.0]);
        assert_eq!(r, vec![3.0, 2.0, 1.0]);
        let f = fade_sample(&[1.0f32; 10], 0.5, 0.0);
        assert_eq!(f[0], 0.0);
        assert!((f[4] - 0.8).abs() < 1e-6);
        assert_eq!(f[9], 1.0);
    }

    #[test]
    fn onset_detection_finds_bursts() {
        // Two noise bursts separated by silence.
        let mut x = vec![0.0f32; 44100];
        let mut rng = XorShift32::new(9);
        for i in 5000..9000 {
            x[i] = (rng.next_f64() * 2.0 - 1.0) as f32 * 0.9;
        }
        for i in 25000..29000 {
            x[i] = (rng.next_f64() * 2.0 - 1.0) as f32 * 0.9;
        }
        let onsets = detect_onsets(&x, 44100.0, &OnsetOpts::default());
        assert_eq!(onsets.len(), 2, "got {onsets:?}");
        assert!((onsets[0] as i32 - 5000).abs() < 1500, "got {onsets:?}");
        assert!((onsets[1] as i32 - 25000).abs() < 1500, "got {onsets:?}");
    }
}
