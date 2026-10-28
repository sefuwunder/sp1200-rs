//! Project save/load.
//!
//! Two formats:
//! - **Native** `sp1200-rs` JSON (versioned, we define it): kit + step
//!   pattern + pad samples + tape audio and chop markers. Projects are
//!   self-contained: reloading one restores the exact pad sounds without
//!   needing the original .wav files.
//! - **Web import**: the browser app's `.sp1200.json`
//!   (`serializeProject`/`loadProjectData` in the web `app.js`): bpm/swing/
//!   master, per-pad tune/level/label, custom pad samples (base64 pcm16),
//!   and `tapes[0]` audio. Filter/delay/loop/voice/choke settings have no
//!   equivalent in this port and are ignored.
//!
//! Loading never panics and never leaves half-applied state: everything
//! fallible happens before the engine is touched.

use serde::{Deserialize, Serialize};

use crate::dsp;
use crate::engine::{BPM_MAX, BPM_MIN, Engine, MAX_STEPS, Tape, N_PADS};

// ---------- hand-rolled base64 (decode only; ~30 lines, no extra deps) ----------

const B64ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return Err("empty base64".to_string());
    }
    if bytes.len() % 4 != 0 {
        return Err("bad base64 length".to_string());
    }
    let mut rev = [255u8; 256];
    for (i, &c) in B64ABC.iter().enumerate() {
        rev[c as usize] = i as u8;
    }
    let chunks: Vec<&[u8]> = bytes.chunks_exact(4).collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for (ci, ch) in chunks.iter().enumerate() {
        let last = ci + 1 == chunks.len();
        let mut vals = [0u32; 4];
        let mut pad = 0;
        for (j, &c) in ch.iter().enumerate() {
            if c == b'=' {
                if j < 2 || !last {
                    return Err("bad base64 padding".to_string());
                }
                pad += 1;
            } else {
                if pad > 0 {
                    return Err("data after base64 padding".to_string());
                }
                let v = rev[c as usize];
                if v == 255 {
                    return Err(format!("bad base64 character {:?}", c as char));
                }
                vals[j] = v as u32;
            }
        }
        if pad > 2 {
            return Err("too much base64 padding".to_string());
        }
        let n = (vals[0] << 18) | (vals[1] << 12) | (vals[2] << 6) | vals[3];
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

/// Web `pcm16B64ToF32`: base64 of little-endian i16 → f32
/// (`s < 0 ? s/32768 : s/32767`, mirroring the web exactly).
pub fn pcm16_b64_to_f32(b64: &str) -> Result<Vec<f32>, String> {
    let bytes = b64_decode(b64)?;
    if bytes.len() % 2 != 0 {
        return Err("odd base64 pcm16 length".to_string());
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|c| {
            let s = i16::from_le_bytes([c[0], c[1]]);
            if s < 0 {
                s as f32 / 32768.0
            } else {
                s as f32 / 32767.0
            }
        })
        .collect())
}

/// base64 of little-endian f32 → String (for embedding 4-track audio).
pub fn f32_b64_encode(samples: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for v in samples {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    b64_encode_bytes(&bytes)
}

pub fn f32_b64_decode(s: &str) -> Result<Vec<f32>, String> {
    let bytes = b64_decode(s)?;
    if bytes.len() % 4 != 0 {
        return Err("bad f32 audio length".to_string());
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

/// base64 of little-endian i16 → String (for embedding pad samples).
pub fn pcm16_b64_encode(samples: &[i16]) -> String {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    b64_encode_bytes(&bytes)
}

fn b64_encode_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for ch in bytes.chunks(3) {
        let n = (ch[0] as u32) << 16
            | (*ch.get(1).unwrap_or(&0) as u32) << 8
            | (*ch.get(2).unwrap_or(&0) as u32);
        out.push(B64ABC[(n >> 18) as usize & 63] as char);
        out.push(B64ABC[(n >> 12) as usize & 63] as char);
        out.push(if ch.len() > 1 {
            B64ABC[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if ch.len() > 2 {
            B64ABC[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

// ---------- native format ----------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NativePad {
    pub tune: f32, // semitones, like the engine
    pub level: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chop: Option<usize>, // chop this pad plays; default = pad index
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub filter: String, // "off" | "lp" | "hp"
    #[serde(default = "def_cutoff")]
    pub cutoff: f32,
    #[serde(default = "def_res")]
    pub res: f32, // filter resonance (Q)
    #[serde(default)]
    pub choke: u8, // 0 = none
    #[serde(default = "def_trim")]
    pub trim: [f32; 2],
    #[serde(default)]
    pub reverse: bool,
    /// Loop region, fractions of the playable sample; off in older files.
    #[serde(default = "def_trim")]
    pub loop_pts: [f32; 2],
    #[serde(default)]
    pub loop_on: bool,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub mono: bool,
    /// User-loaded sample (base64 LE i16, full drum-mode source); absent
    /// for drums.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<String>,
}

fn def_cutoff() -> f32 {
    3000.0
}

fn def_res() -> f32 {
    0.71
}

fn def_trim() -> [f32; 2] {
    [0.0, 1.0]
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NativeTape {
    pub name: String,
    pub chops: Vec<[usize; 2]>,
    /// Tape audio (base64 LE i16 @ SP_RATE). Absent in older files, which
    /// load markers-only (drop the .wav after loading, as before).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NativeTrack {
    /// base64 LE f32 mono @ SP_RATE; absent for empty tracks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
    #[serde(default)]
    pub armed: bool,
    #[serde(default)]
    pub muted: bool,
    #[serde(default = "def_track_source")]
    pub source: String, // "input" | "master"
    /// Playback window start in samples; 0 in older files.
    #[serde(default)]
    pub trim_start: usize,
    /// Playback window end in samples; None = to the end of the track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trim_end: Option<usize>,
}

fn def_track_source() -> String {
    "master".to_string()
}

#[derive(Serialize, Deserialize, Debug)]
pub struct NativeProject {
    pub format: String, // "sp1200-rs"
    pub version: u32,   // 1
    #[serde(default)]
    pub name: String,
    #[serde(default = "def_bpm")]
    pub bpm: f64,
    #[serde(default = "def_swing")]
    pub swing: f64,
    #[serde(default = "def_master")]
    pub master: f32,
    pub pads: [NativePad; 8],
    /// 8 pads x up to 64 steps. Older files carry 16-wide rows; short
    /// rows pad with false, long rows truncate, so old projects load.
    /// (Custom ser/de because serde's array impls stop at 32.)
    #[serde(
        serialize_with = "ser_pattern",
        deserialize_with = "de_pattern",
        default = "def_pattern"
    )]
    pub pattern: [[bool; MAX_STEPS]; 8],
    /// Playing sequence length; 16 in older files (field absent).
    #[serde(default = "def_seq_len")]
    pub seq_len: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tape: Option<NativeTape>,
    /// 4-track tape; empty in older files.
    #[serde(default)]
    pub four_track: Vec<NativeTrack>,
    /// 4-track loop toggle; off in older files.
    #[serde(default)]
    pub four_track_loop: bool,
    /// Tape-bus warmth/distortion in percent; 0 in older files.
    #[serde(default)]
    pub four_track_warmth: f32,
    #[serde(default)]
    pub four_track_distortion: f32,
}

fn def_bpm() -> f64 {
    100.0
}

fn def_pattern() -> [[bool; MAX_STEPS]; 8] {
    [[false; MAX_STEPS]; 8]
}

fn def_seq_len() -> usize {
    16
}

fn de_pattern<'de, D>(d: D) -> Result<[[bool; MAX_STEPS]; 8], D::Error>
where
    D: serde::Deserializer<'de>,
{
    let rows = Vec::<Vec<bool>>::deserialize(d)?;
    let mut p = [[false; MAX_STEPS]; 8];
    for (i, row) in rows.iter().take(8).enumerate() {
        for (j, &v) in row.iter().take(MAX_STEPS).enumerate() {
            p[i][j] = v;
        }
    }
    Ok(p)
}

fn ser_pattern<S>(p: &[[bool; MAX_STEPS]; 8], s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(8))?;
    for row in p.iter() {
        seq.serialize_element(&row[..])?;
    }
    seq.end()
}
fn def_swing() -> f64 {
    60.0
}
fn def_master() -> f32 {
    0.8
}

pub fn parse_native(json: &str) -> Result<NativeProject, String> {
    let p: NativeProject =
        serde_json::from_str(json).map_err(|e| format!("bad project JSON: {e}"))?;
    if p.format != "sp1200-rs" {
        return Err(format!(
            "not an sp1200-rs project (format {:?}); web projects need a .sp1200.json name",
            p.format
        ));
    }
    if p.version != 1 {
        return Err(format!("unsupported project version: {}", p.version));
    }
    Ok(p)
}

/// Apply a parsed native project. Infallible — call only after `parse_native`.
pub fn apply_native(eng: &mut Engine, p: &NativeProject) {
    use crate::dsp::FilterKind;
    eng.bpm = p.bpm.clamp(BPM_MIN, BPM_MAX);
    eng.swing = p.swing.clamp(50.0, 75.0);
    eng.master = p.master.clamp(0.0, 1.0);
    for (i, pad) in p.pads.iter().enumerate() {
        eng.pads[i].tune_st = pad.tune.clamp(-24.0, 24.0);
        eng.pads[i].level = pad.level.clamp(0.0, 1.0);
        eng.pads[i].locked = false; // restored from the file below
                                    // User sample first (becomes the drum-mode source), then edits.
        if let Some(b64) = &pad.sample {
            if let Ok(bytes) = b64_decode(b64) {
                if bytes.len() % 2 == 0 && !bytes.is_empty() {
                    let src: Vec<i16> = bytes
                        .chunks_exact(2)
                        .map(|c| i16::from_le_bytes([c[0], c[1]]))
                        .collect();
                    eng.set_custom_pad(i, src, pad.label.clone());
                }
            }
        } else if pad.label.is_some() {
            eng.pads[i].label = pad.label.clone();
        }
        eng.pads[i].trim = (pad.trim[0].clamp(0.0, 1.0), pad.trim[1].clamp(0.0, 1.0));
        if eng.pads[i].trim.1 < eng.pads[i].trim.0 {
            eng.pads[i].trim.1 = eng.pads[i].trim.0;
        }
        eng.pads[i].reverse = pad.reverse;
        eng.pads[i].loop_pts = (
            pad.loop_pts[0].clamp(0.0, 1.0),
            pad.loop_pts[1].clamp(0.0, 1.0),
        );
        eng.pads[i].loop_enabled = pad.loop_on;
        eng.pads[i].rebuild();
        eng.set_filter(i, FilterKind::from_str(&pad.filter), pad.cutoff);
        eng.set_res(i, pad.res);
        eng.set_choke(i, pad.choke);
        eng.set_mono(i, pad.mono);
        eng.pads[i].locked = pad.locked;
    }
    eng.patterns = p.pattern;
    eng.set_seq_len(p.seq_len);
    // 4-track: restore audio + settings (transport always starts stopped).
    {
        use crate::fourtrack::TrackSource;
        let ft = &mut eng.four_track;
        for t in ft.tracks.iter_mut() {
            t.samples.clear();
            t.armed = false;
            t.muted = false;
            t.source = TrackSource::Master;
            t.trim_start = 0;
            t.trim_end = None;
        }
        for (i, nt) in p.four_track.iter().enumerate().take(4) {
            let t = &mut ft.tracks[i];
            t.armed = nt.armed;
            t.muted = nt.muted;
            t.source = TrackSource::from_str(&nt.source);
            t.trim_start = nt.trim_start;
            t.trim_end = nt.trim_end;
            if let Some(a) = &nt.audio {
                if let Ok(v) = f32_b64_decode(a) {
                    t.samples = v;
                }
            }
        }
        ft.playhead = 0;
        ft.playing = false;
        ft.recording = false;
        ft.selected = 0;
        ft.loop_enabled = p.four_track_loop;
        ft.set_warmth(p.four_track_warmth);
        ft.set_distortion(p.four_track_distortion);
        ft.loop_len = 0;
    }
    match &p.tape {
        Some(t) => {
            let audio: Vec<i16> = t
                .audio
                .as_ref()
                .and_then(|b64| b64_decode(b64).ok())
                .and_then(|bytes| {
                    if bytes.len() % 2 == 0 && !bytes.is_empty() {
                        Some(
                            bytes
                                .chunks_exact(2)
                                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                                .collect(),
                        )
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            let n = audio.len();
            // Chops are sample offsets: clamp/drop against the audio so a
            // corrupt file can never panic the player. Older markers-only
            // files (no audio) keep their chops verbatim.
            let mut chops: Vec<(usize, usize)> = Vec::with_capacity(t.chops.len());
            for &[a, b] in &t.chops {
                let (a, mut b) = (a.min(b), b.max(a));
                if b <= a {
                    continue;
                }
                if n > 0 {
                    if a >= n {
                        continue;
                    }
                    b = b.min(n);
                    if b <= a {
                        continue;
                    }
                }
                chops.push((a, b));
            }
            eng.tape = Some(Tape {
                name: t.name.clone(),
                audio,
                chops,
            });
        }
        None => eng.tape = None,
    }
    // Per-pad chop overrides (validated against the tape, if any).
    for (i, pad) in p.pads.iter().enumerate() {
        eng.chop_map[i] = match pad.chop {
            Some(c)
                if eng
                    .tape
                    .as_ref()
                    .map(|t| c < t.chops.len())
                    .unwrap_or(false) =>
            {
                Some(c)
            }
            _ => None,
        };
    }
    eng.refresh_pad_samples();
}

/// Serialize the engine to the native format.
pub fn save_native(eng: &Engine, name: &str) -> String {
    let pads: [NativePad; 8] = std::array::from_fn(|i| {
        let p = &eng.pads[i];
        NativePad {
            tune: p.tune_st,
            level: p.level,
            chop: eng.chop_map[i],
            label: p.label.clone(),
            filter: p.filter_kind.as_str().to_string(),
            cutoff: p.filter_cutoff,
            res: p.filter_res,
            mono: p.mono,
            choke: p.choke,
            trim: [p.trim.0, p.trim.1],
            reverse: p.reverse,
            loop_pts: [p.loop_pts.0, p.loop_pts.1],
            loop_on: p.loop_enabled,
            locked: p.locked,
            // The custom drum-mode sample, not the live `source`: when a
            // tape is loaded the source is chop audio, and saving that
            // would permanently replace the user's sample on reload.
            sample: if p.custom {
                Some(pcm16_b64_encode(&eng.pad_factory[i]))
            } else {
                None
            },
        }
    });
    let tape = eng.tape.as_ref().map(|t| NativeTape {
        name: t.name.clone(),
        chops: t.chops.iter().map(|&(a, b)| [a, b]).collect(),
        audio: if t.audio.is_empty() {
            None
        } else {
            Some(pcm16_b64_encode(&t.audio))
        },
    });
    let four_track = eng
        .four_track
        .tracks
        .iter()
        .map(|t| NativeTrack {
            audio: if t.samples.is_empty() {
                None
            } else {
                Some(f32_b64_encode(&t.samples))
            },
            armed: t.armed,
            muted: t.muted,
            source: t.source.as_str().to_string(),
            trim_start: t.trim_start,
            trim_end: t.trim_end,
        })
        .collect();
    let p = NativeProject {
        format: "sp1200-rs".to_string(),
        version: 1,
        name: name.to_string(),
        bpm: eng.bpm,
        swing: eng.swing,
        master: eng.master,
        pads,
        pattern: eng.patterns,
        seq_len: eng.seq_len,
        tape,
        four_track,
        four_track_loop: eng.four_track.loop_enabled,
        four_track_warmth: eng.four_track.warmth * 100.0,
        four_track_distortion: eng.four_track.distortion * 100.0,
    };
    serde_json::to_string_pretty(&p).unwrap_or_else(|_| "{}".to_string())
}

pub fn save_native_file(eng: &Engine, path: &str, name: &str) -> Result<(), String> {
    let text = save_native(eng, name);
    std::fs::write(path, text).map_err(|e| format!("can't write {path}: {e}"))?;
    Ok(())
}

// ---------- web import (.sp1200.json) ----------

#[derive(Deserialize, Debug, Default)]
pub(crate) struct WebProject {
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    bpm: Option<f64>,
    #[serde(default)]
    swing: Option<f64>,
    #[serde(default)]
    master: Option<f64>, // 0..100 in the web
    #[serde(default)]
    pads: Vec<WebPad>,
    #[serde(default)]
    tapes: Vec<WebTape>,
}

#[derive(Deserialize, Debug, Default)]
struct WebPad {
    #[serde(default)]
    tune: Option<f64>, // ratio 0.5..2 in the web
    #[serde(default)]
    level: Option<f64>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    custom: Option<WebCustom>,
}

#[derive(Deserialize, Debug)]
struct WebCustom {
    #[serde(default)]
    name: Option<String>,
    pcm: String, // base64 pcm16 @ 44.1 kHz clean
}

#[derive(Deserialize, Debug, Default)]
struct WebTape {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    audio: Option<WebAudio>,
}

#[derive(Deserialize, Debug)]
struct WebAudio {
    sr: f64,
    len: usize,
    pcm: String, // base64 pcm16, stereo planar
}

struct PreparedPad {
    tune_st: f32,
    level: f32,
    label: Option<String>,
    custom: Option<Vec<i16>>,
}

struct PreparedWeb {
    bpm: f64,
    swing: f64,
    master: f32,
    pads: Vec<PreparedPad>,
    tape: Option<(String, Vec<i16>)>,
    warnings: Vec<String>,
}

pub(crate) fn parse_web(json: &str) -> Result<WebProject, String> {
    let p: WebProject = serde_json::from_str(json).map_err(|e| format!("bad project JSON: {e}"))?;
    match p.version {
        Some(1) => Ok(p),
        other => Err(format!("unsupported web project version: {other:?}")),
    }
}

/// Decode everything fallible *before* touching the engine.
fn prepare_web(p: &WebProject) -> Result<PreparedWeb, String> {
    let mut warnings = Vec::new();
    let mut pads = Vec::new();
    for (i, wp) in p.pads.iter().take(N_PADS).enumerate() {
        let tune_st = wp
            .tune
            .map(|t| 12.0 * t.max(0.05).log2() as f32)
            .unwrap_or(0.0)
            .clamp(-24.0, 24.0);
        let level = wp.level.unwrap_or(0.9) as f32;
        // Label: explicit label first, then the custom sample's name.
        let label = wp
            .label
            .clone()
            .or_else(|| wp.custom.as_ref().and_then(|c| c.name.clone()))
            .map(|l| l.chars().take(12).collect::<String>().to_uppercase());
        let custom = match &wp.custom {
            Some(c) => match pcm16_b64_to_f32(&c.pcm) {
                Ok(f) if !f.is_empty() => Some(dsp::sp1200ize_i16(&f, dsp::SYNTH_RATE)),
                Ok(_) => {
                    warnings.push(format!("pad {}: empty custom sample", i + 1));
                    None
                }
                Err(e) => {
                    warnings.push(format!("pad {}: bad custom sample ({e})", i + 1));
                    None
                }
            },
            None => None,
        };
        pads.push(PreparedPad {
            tune_st,
            level: level.clamp(0.0, 1.0),
            label,
            custom,
        });
    }
    let tape = match p.tapes.first().and_then(|t| t.audio.as_ref()) {
        Some(a) => match pcm16_b64_to_f32(&a.pcm) {
            Ok(f) => {
                // Stereo planar: first `len` samples are L, next `len` are R.
                let len = a.len.min(f.len() / 2);
                if len == 0 {
                    warnings.push("tape: empty audio".to_string());
                    None
                } else {
                    let mut mono = Vec::with_capacity(len);
                    for i in 0..len {
                        mono.push((f[i] + f[len + i]) * 0.5);
                    }
                    let mut clean = mono;
                    dsp::normalize(&mut clean, 0.92);
                    let name = p.tapes[0]
                        .name
                        .clone()
                        .unwrap_or_else(|| "TAPE".to_string())
                        .to_uppercase()
                        .chars()
                        .take(18)
                        .collect();
                    Some((name, dsp::sp1200ize_i16(&clean, a.sr)))
                }
            }
            Err(e) => {
                warnings.push(format!("tape: bad audio ({e})"));
                None
            }
        },
        None => None,
    };
    Ok(PreparedWeb {
        bpm: p.bpm.unwrap_or(100.0).clamp(BPM_MIN, BPM_MAX),
        swing: p.swing.unwrap_or(60.0).clamp(50.0, 75.0),
        master: (p.master.unwrap_or(80.0) / 100.0).clamp(0.0, 1.0) as f32,
        pads,
        tape,
        warnings,
    })
}

/// Apply a prepared web project. Returns a human summary for the UI.
fn apply_web(eng: &mut Engine, p: PreparedWeb) -> String {
    eng.bpm = p.bpm;
    eng.swing = p.swing;
    eng.master = p.master;
    let mut customs = 0;
    for (i, pp) in p.pads.into_iter().enumerate() {
        eng.pads[i].tune_st = pp.tune_st;
        eng.pads[i].level = pp.level;
        if let Some(c) = pp.custom {
            customs += 1;
            eng.set_custom_pad(i, c, pp.label.or(Some(format!("PAD{}", i + 1))));
        } else {
            eng.pads[i].label = pp.label;
        }
    }
    let mut parts = vec![format!("BPM {:.0}", eng.bpm)];
    if customs > 0 {
        parts.push(format!(
            "{customs} CUSTOM PAD{}",
            if customs > 1 { "S" } else { "" }
        ));
    }
    if let Some((name, audio)) = p.tape {
        let n = eng.load_tape(name.clone(), audio);
        parts.push(format!("TAPE {name} ({n} CHOPS)"));
    }
    let mut msg = format!("WEB PROJECT: {}", parts.join(", "));
    if !p.warnings.is_empty() {
        msg.push_str("; ");
        msg.push_str(&p.warnings.join("; "));
    }
    msg
}

// ---------- file entry points ----------

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Load a project file: `.sp1200.json` → web import, anything else → native.
/// Returns a summary for the UI; the engine is untouched on error.
pub fn load_project_file(eng: &mut Engine, path: &str) -> Result<String, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("can't read {}: {e}", file_name(path)))?;
    if path.to_lowercase().ends_with(".sp1200.json") {
        let wp = parse_web(&text)?;
        let prep = prepare_web(&wp)?;
        Ok(apply_web(eng, prep))
    } else {
        let np = parse_native(&text)?;
        apply_native(eng, &np);
        let n = if np.name.is_empty() {
            file_name(path).to_string()
        } else {
            np.name.clone()
        };
        Ok(format!("PROJECT LOADED: {n}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tape_bus_settings_round_trip_native() {
        let mut eng = Engine::new();
        eng.four_track.set_warmth(45.0);
        eng.four_track.set_distortion(30.0);
        let text = save_native(&eng, "BUS");
        let np = parse_native(&text).unwrap();
        let mut eng2 = Engine::new();
        apply_native(&mut eng2, &np);
        assert!((eng2.four_track.warmth - 0.45).abs() < 1e-6);
        assert!((eng2.four_track.distortion - 0.30).abs() < 1e-6);
    }

    #[test]
    fn b64_vectors() {
        assert_eq!(b64_decode("SGVsbG8=").unwrap(), b"Hello");
        assert_eq!(b64_decode("TWFu").unwrap(), b"Man");
        assert_eq!(b64_decode("TWE=").unwrap(), b"Ma");
        assert_eq!(b64_decode("TQ==").unwrap(), b"M");
        // round-trip through longer input
        let data: Vec<u8> = (0..256).map(|i| i as u8).collect();
        let enc: String = {
            const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut s = String::new();
            for ch in data.chunks(3) {
                let b0 = ch[0] as u32;
                let b1 = *ch.get(1).unwrap_or(&0) as u32;
                let b2 = *ch.get(2).unwrap_or(&0) as u32;
                let n = (b0 << 16) | (b1 << 8) | b2;
                s.push(ABC[((n >> 18) & 63) as usize] as char);
                s.push(ABC[((n >> 12) & 63) as usize] as char);
                s.push(if ch.len() > 1 {
                    ABC[((n >> 6) & 63) as usize] as char
                } else {
                    '='
                });
                s.push(if ch.len() > 2 {
                    ABC[(n & 63) as usize] as char
                } else {
                    '='
                });
            }
            s
        };
        assert_eq!(b64_decode(&enc).unwrap(), data);
    }

    #[test]
    fn b64_invalid_is_err() {
        assert!(b64_decode("").is_err());
        assert!(b64_decode("abc").is_err()); // bad length
        assert!(b64_decode("====").is_err()); // padding too early
        assert!(b64_decode("ab=c").is_err()); // data after padding
        assert!(b64_decode("ab!d").is_err()); // bad char
        assert!(b64_decode("abcd====").is_err()); // padding mid-stream
    }

    fn sample_engine() -> Engine {
        let mut e = Engine::new();
        e.bpm = 128.0;
        e.swing = 70.0;
        e.master = 0.9;
        e.pads[0].tune_st = -5.0;
        e.pads[3].level = 0.5;
        e.patterns[0][0] = true;
        e.patterns[7][15] = true;
        // Tape with two chops (markers only, like a loaded project file).
        e.tape = Some(Tape {
            name: "BREAK".to_string(),
            audio: Vec::new(),
            chops: vec![(0, 1000), (1000, 2500)],
        });
        e
    }

    #[test]
    fn b64_encode_round_trips() {
        let samples: Vec<i16> = vec![0, 1, -1, 2047, -2048, 32767, -32768, 12345];
        let enc = pcm16_b64_encode(&samples);
        let bytes = b64_decode(&enc).unwrap();
        let back: Vec<i16> = bytes
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(back, samples);
        // Padding shapes: 1 sample = 2 bytes -> one '='; 3 samples = 6 bytes -> none.
        assert!(pcm16_b64_encode(&[7i16]).ends_with('='));
        assert!(!pcm16_b64_encode(&[1i16, 2, 3]).ends_with('='));
    }

    #[test]
    fn native_round_trip_sound_design() {
        use crate::dsp::FilterKind;
        let mut e = Engine::new();
        e.set_filter(2, FilterKind::Lowpass, 800.0);
        e.set_filter(5, FilterKind::Highpass, 5000.0);
        e.set_choke(4, 2);
        e.set_choke(5, 2);
        e.nudge_trim(1, 0, 0.25);
        e.toggle_reverse(1);
        e.set_custom_pad(6, vec![11i16, -22, 33, -44], Some("WOBBLE".to_string()));
        e.set_locked(3, true);
        e.set_res(2, 3.5);
        e.set_mono(5, true);
        e.nudge_loop(0, 0, 0.25);
        e.nudge_loop(0, 1, -0.25);
        e.toggle_loop(0);
        // 4-track: bounce a block of "pattern" onto track 1.
        e.four_track.toggle_arm();
        e.four_track.toggle_record().unwrap();
        let mut blk = vec![0.0f32; 64];
        e.render(&mut blk); // no pattern playing: renders near-silence
        e.trigger(0);
        e.render(&mut blk); // now the master block carries pad 1
        e.four_track.toggle_record().unwrap();
        let take = e.four_track.tracks[0].samples.clone();
        assert!(!take.is_empty());
        e.four_track.tracks[0].set_trim(10, Some(50));
        e.four_track.select(1);
        e.four_track.toggle_mute();
        e.four_track.toggle_source(); // -> INPUT
        e.four_track.stop_rewind();
        e.four_track.toggle_loop();
        let json = save_native(&e, "design");
        assert!(json.contains("\"filter\": \"lp\""), "{json}");
        let p = parse_native(&json).unwrap();
        let mut e2 = Engine::new();
        apply_native(&mut e2, &p);
        assert_eq!(e2.pads[2].filter_kind, FilterKind::Lowpass);
        assert!((e2.pads[2].filter_cutoff - 800.0).abs() < 1e-3);
        assert!((e2.pads[2].filter_res - 3.5).abs() < 1e-6);
        assert!(e2.pads[5].mono);
        assert!(!e2.pads[0].mono);
        // 4-track round-trips (audio + settings).
        assert_eq!(e2.four_track.tracks[0].samples, take);
        assert_eq!(e2.four_track.tracks[0].trim_start, 10);
        assert_eq!(e2.four_track.tracks[0].trim_end, Some(50));
        // Pad loop points round-trip.
        assert!(e2.pads[0].loop_enabled);
        assert!((e2.pads[0].loop_pts.0 - 0.25).abs() < 1e-6);
        assert!((e2.pads[0].loop_pts.1 - 0.75).abs() < 1e-6);
        assert!(e2.four_track.loop_enabled, "loop toggle round-trips");
        assert!(e2.four_track.tracks[0].armed);
        assert!(e2.four_track.tracks[1].muted);
        assert_eq!(
            e2.four_track.tracks[1].source,
            crate::fourtrack::TrackSource::Input
        );
        assert!(!e2.four_track.playing);
        assert!(!e2.four_track.recording);
        assert_eq!(e2.pads[5].filter_kind, FilterKind::Highpass);
        assert_eq!(e2.pads[4].choke, 2);
        assert_eq!(e2.pads[5].choke, 2);
        assert!(e2.pads[3].locked);
        assert!(!e2.pads[0].locked);
        assert!((e2.pads[1].trim.0 - 0.25).abs() < 1e-6);
        assert!(e2.pads[1].reverse);
        // Reversed trim: playable is the flipped region.
        assert_eq!(e2.pads[1].sample[0], e.pads[1].sample[0]);
        assert_eq!(e2.pads[6].source, vec![11i16, -22, 33, -44]);
        assert_eq!(e2.pads[6].label.as_deref(), Some("WOBBLE"));
        assert!(e2.pads[6].custom);
        // Untouched pads stay drums.
        assert_eq!(e2.pads[0].filter_kind, FilterKind::Off);
        assert_eq!(e2.pads[0].choke, 0);
        assert!(!e2.pads[0].custom);
    }

    #[test]
    fn native_old_file_without_new_fields_loads() {
        // v1 files predate filter/choke/trim/reverse/sample: defaults apply.
        let pads: Vec<String> = (0..8)
            .map(|_| "{\"tune\":0.0,\"level\":0.9}".to_string())
            .collect();
        let steps: Vec<String> = (0..8)
            .map(|_| format!("[{}]", ["false"; 16].join(",")))
            .collect();
        let json = format!(
            "{{\"format\":\"sp1200-rs\",\"version\":1,\"name\":\"old\",\"bpm\":100.0,\"swing\":60.0,\"master\":0.8,\"pads\":[{}],\"pattern\":[{}]}}",
            pads.join(","),
            steps.join(",")
        );
        let p = parse_native(&json).unwrap();
        let mut e = Engine::new();
        apply_native(&mut e, &p);
        assert_eq!(e.pads[3].filter_kind, crate::dsp::FilterKind::Off);
        assert_eq!(e.pads[3].choke, 0);
        assert_eq!(e.pads[3].trim, (0.0, 1.0));
        assert!(!e.pads[3].reverse);
        assert!(!e.pads[3].custom);
        // v1 patterns are 16-wide: rows pad with false, length is 16.
        assert_eq!(e.seq_len, 16);
        assert!(e.patterns.iter().all(|row| row[16..].iter().all(|&s| !s)));
    }

    #[test]
    fn seq_len_and_long_pattern_round_trip() {
        let mut e = sample_engine();
        e.set_seq_len(32);
        e.patterns[0][40] = true;
        e.patterns[7][63] = true;
        let json = save_native(&e, "longpattern");
        let p = parse_native(&json).unwrap();
        let mut e2 = Engine::new();
        apply_native(&mut e2, &p);
        assert_eq!(e2.seq_len, 32);
        assert_eq!(e2.patterns, e.patterns);
    }

    #[test]
    fn native_round_trip() {
        let e = sample_engine();
        let json = save_native(&e, "roundtrip");
        let p = parse_native(&json).unwrap();
        let mut e2 = Engine::new();
        apply_native(&mut e2, &p);
        assert_eq!(e2.bpm, 128.0);
        assert_eq!(e2.swing, 70.0);
        assert_eq!(e2.master, 0.9);
        assert_eq!(e2.pads[0].tune_st, -5.0);
        assert_eq!(e2.pads[3].level, 0.5);
        assert_eq!(e2.patterns, e.patterns);
        let t = e2.tape.as_ref().unwrap();
        assert_eq!(t.name, "BREAK");
        assert_eq!(t.chops, vec![(0, 1000), (1000, 2500)]);
        assert!(t.audio.is_empty());
        // And it serializes back identically.
        assert_eq!(save_native(&e2, "roundtrip"), json);
    }

    #[test]
    fn native_round_trip_chop_assignment() {
        // An arbitrary assignment (pad 1 <- chop 12) survives save/load:
        // the project stores the chop override, and load rebuilds the
        // pad from it.
        let mut e = Engine::new();
        let mut audio = Vec::new();
        for _ in 0..12 {
            audio.push(2047);
            audio.extend(std::iter::repeat(300i16).take(3000));
        }
        let n = e.load_tape("MANY".to_string(), audio.clone());
        assert!(n >= 12, "got {n}");
        e.assign_chop(0, Some(11)).unwrap();
        let json = save_native(&e, "assigned");
        let p = parse_native(&json).unwrap();
        let mut e2 = Engine::new();
        apply_native(&mut e2, &p);
        assert_eq!(e2.chop_map[0], Some(11));
        assert_eq!(e2.pad_label(0), "CHOP 12");
        let (a, b) = e2.tape.as_ref().unwrap().chops[11];
        assert_eq!(e2.pads[0].source, audio[a..b].to_vec());
    }

    #[test]
    fn native_round_tape_audio() {
        // Tape audio is embedded now: pads playing chops survive save/load.
        let mut e = Engine::new();
        let audio: Vec<i16> = (0..3000).map(|i| (i % 251) as i16 - 125).collect();
        e.tape = Some(Tape {
            name: "LOOP".to_string(),
            audio: audio.clone(),
            chops: vec![(0, 1000), (1000, 3000)],
        });
        e.refresh_pad_samples();
        assert_eq!(e.pads[0].source, audio[0..1000].to_vec());
        assert_eq!(e.pads[1].source, audio[1000..3000].to_vec());

        let json = save_native(&e, "tapey");
        assert!(json.contains("\"audio\""), "tape audio embedded");
        let p = parse_native(&json).unwrap();
        let mut e2 = Engine::new();
        apply_native(&mut e2, &p);
        let t = e2.tape.as_ref().unwrap();
        assert_eq!(t.audio, audio);
        assert_eq!(t.chops, vec![(0, 1000), (1000, 3000)]);
        // Pads rebuilt from the embedded chops.
        assert_eq!(e2.pads[0].source, audio[0..1000].to_vec());
        assert_eq!(e2.pads[1].source, audio[1000..3000].to_vec());
        assert_eq!(e2.chop_map[0], Some(0));
        assert_eq!(e2.chop_map[1], Some(1));
        // Full loop: serializes back identically.
        assert_eq!(save_native(&e2, "tapey"), json);
    }

    #[test]
    fn native_custom_pad_with_tape_saves_factory_sample() {
        // A custom pad playing a chop must save its drum-mode sample, not
        // the chop audio it happens to be playing.
        let mut e = Engine::new();
        let custom: Vec<i16> = vec![1111; 500];
        e.set_custom_pad(2, custom.clone(), Some("MINE".to_string()));
        let audio: Vec<i16> = (0..2000).map(|i| (i % 97) as i16).collect();
        e.tape = Some(Tape {
            name: "T".to_string(),
            audio: audio.clone(),
            chops: vec![(0, 500), (500, 1500), (1500, 2000)],
        });
        e.refresh_pad_samples();
        // Pad 3 now plays chop 3's audio (pad i defaults to chop i)...
        assert_eq!(e.pads[2].source, audio[1500..2000].to_vec());
        assert!(e.pads[2].custom);
        assert_ne!(e.pads[2].source, custom);

        let json = save_native(&e, "mix");
        let p = parse_native(&json).unwrap();
        let saved: Vec<i16> = {
            let b64 = p.pads[2].sample.as_ref().expect("custom pad saved");
            let bytes = b64_decode(b64).unwrap();
            bytes
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect()
        };
        assert_eq!(saved, custom, "saved the drum-mode sample, not chop audio");

        let mut e2 = Engine::new();
        apply_native(&mut e2, &p);
        // Drum-mode sample survived...
        assert_eq!(e2.pad_factory[2], custom);
        assert!(e2.pads[2].custom);
        // ...and with the embedded tape the pad still plays its chop.
        assert_eq!(e2.pads[2].source, audio[1500..2000].to_vec());
    }

    #[test]
    fn native_tape_chops_validated_against_audio() {
        // Corrupt chop offsets can never panic the player.
        let mut e = Engine::new();
        e.tape = Some(Tape {
            name: "T".to_string(),
            audio: vec![7; 100],
            chops: vec![(0, 50), (40, 5000), (9000, 9500), (70, 70)],
        });
        let json = save_native(&e, "clamp");
        let p = parse_native(&json).unwrap();
        let mut e2 = Engine::new();
        apply_native(&mut e2, &p); // must not panic
        let t = e2.tape.as_ref().unwrap();
        assert_eq!(t.chops, vec![(0, 50), (40, 100)]);
        // Playing a pad on a clamped chop must not panic either.
        e2.trigger(1);
        let mut blk = vec![0.0f32; 64];
        e2.render(&mut blk);
    }

    #[test]
    fn native_rejects_bad_input() {
        assert!(parse_native("{oops").is_err());
        assert!(
            parse_native(r#"{"format":"sp1200-rs","version":999,"pads":[],"pattern":[]}"#).is_err()
        );
        assert!(parse_native(r#"{"format":"nope","version":1}"#).is_err());
        // Missing structural fields.
        assert!(parse_native(r#"{"format":"sp1200-rs","version":1}"#).is_err());
        // Wrong pad count.
        assert!(parse_native(
            r#"{"format":"sp1200-rs","version":1,"pads":[],"pattern":[[true],[true],[true],[true],[true],[true],[true],[true]]}"#
        )
        .is_err());
    }

    /// Build a minimal web project: one custom pad + bpm, as the web saves it.
    fn web_project_json() -> String {
        // 8 samples of clean 44.1 kHz audio, pcm16 web encoding.
        let clean = [0.0f32, 0.5, -0.5, 1.0, -1.0, 0.25, -0.25, 0.0];
        let mut bytes = Vec::new();
        for &v in &clean {
            let s = if v < 0.0 {
                (v * 32768.0).round() as i16
            } else {
                (v * 32767.0).round() as i16
            };
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut b64 = String::new();
        for ch in bytes.chunks(3) {
            let b0 = ch[0] as u32;
            let b1 = *ch.get(1).unwrap_or(&0) as u32;
            let b2 = *ch.get(2).unwrap_or(&0) as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;
            b64.push(ABC[((n >> 18) & 63) as usize] as char);
            b64.push(ABC[((n >> 12) & 63) as usize] as char);
            b64.push(if ch.len() > 1 {
                ABC[((n >> 6) & 63) as usize] as char
            } else {
                '='
            });
            b64.push(if ch.len() > 2 {
                ABC[(n & 63) as usize] as char
            } else {
                '='
            });
        }
        format!(
            r#"{{"version":1,"name":"webtest","bpm":100,"swing":62,"master":85,
            "pads":[{{"tune":1.5,"level":0.7,"label":"HIT","custom":{{"name":"HIT","pcm":"{b64}"}}}}],
            "tapes":[]}}"#
        )
    }

    #[test]
    fn web_import_minimal() {
        let json = web_project_json();
        let wp = parse_web(&json).unwrap();
        let prep = prepare_web(&wp).unwrap();
        assert!(prep.warnings.is_empty(), "{:?}", prep.warnings);
        let mut e = Engine::new();
        let msg = apply_web(&mut e, prep);
        assert_eq!(e.bpm, 100.0);
        assert_eq!(e.swing, 62.0);
        assert!((e.master - 0.85).abs() < 1e-6);
        // tune ratio 1.5 → +7.02 semitones
        assert!(
            (e.pads[0].tune_st - 7.01955).abs() < 1e-3,
            "{}",
            e.pads[0].tune_st
        );
        assert_eq!(e.pads[0].level, 0.7);
        assert_eq!(e.pads[0].label.as_deref(), Some("HIT"));
        // Custom audio present: 8 clean samples → 12-bit @26.04 kHz.
        assert!(!e.pads[0].sample.is_empty());
        assert!(msg.contains("1 CUSTOM PAD"), "{msg}");
        // Pads without customs keep their synths.
        assert_eq!(e.pads[1].label, None);
    }

    #[test]
    fn web_import_rejects_bad_input() {
        assert!(parse_web("{oops").is_err());
        assert!(parse_web(r#"{"version":999}"#).is_err());
        assert!(parse_web(r#"{"version":2,"pads":[]}"#).is_err());
    }

    #[test]
    fn web_import_tape_audio() {
        // Stereo planar base64: 4 frames L/R.
        let frames = [0.5f32, -0.5, 0.25, -0.25];
        let mut bytes = Vec::new();
        for &v in &frames {
            let s = (v * 32767.0).round() as i16;
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        for &v in &frames {
            let s = (v * 32767.0).round() as i16;
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut b64 = String::new();
        for ch in bytes.chunks(3) {
            let b0 = ch[0] as u32;
            let b1 = *ch.get(1).unwrap_or(&0) as u32;
            let b2 = *ch.get(2).unwrap_or(&0) as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;
            b64.push(ABC[((n >> 18) & 63) as usize] as char);
            b64.push(ABC[((n >> 12) & 63) as usize] as char);
            b64.push(if ch.len() > 1 {
                ABC[((n >> 6) & 63) as usize] as char
            } else {
                '='
            });
            b64.push(if ch.len() > 2 {
                ABC[(n & 63) as usize] as char
            } else {
                '='
            });
        }
        let json = format!(
            r#"{{"version":1,"pads":[],"tapes":[{{"name":"loop","audio":{{"sr":44100,"len":4,"pcm":"{b64}"}}}}]}}"#
        );
        let wp = parse_web(&json).unwrap();
        let prep = prepare_web(&wp).unwrap();
        let (name, audio) = prep.tape.unwrap();
        assert_eq!(name, "LOOP");
        // 4 mono frames @44.1k → 12-bit @26.04 kHz: floor(4 / (44100/26040)) = 2
        assert_eq!(audio.len(), 2, "{audio:?}");
    }
}
