//! Project save/load.
//!
//! Two formats:
//! - **Native** `sp1200-rs` JSON (versioned, we define it): kit + step
//!   pattern + tape chop markers. Tape *audio* is not embedded — drop the
//!   .wav after loading and the saved chops apply to it.
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
use crate::engine::{Engine, Tape, N_PADS};

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

// ---------- native format ----------

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NativePad {
    pub tune: f32, // semitones, like the engine
    pub level: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chop: Option<usize>, // chop this pad plays; default = pad index
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NativeTape {
    pub name: String,
    pub chops: Vec<[usize; 2]>,
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
    pub pattern: [[bool; 16]; 8],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tape: Option<NativeTape>,
}

fn def_bpm() -> f64 {
    100.0
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
    eng.bpm = p.bpm.clamp(60.0, 200.0);
    eng.swing = p.swing.clamp(50.0, 75.0);
    eng.master = p.master.clamp(0.0, 1.0);
    for (i, pad) in p.pads.iter().enumerate() {
        eng.pads[i].tune_st = pad.tune.clamp(-24.0, 24.0);
        eng.pads[i].level = pad.level.clamp(0.0, 1.0);
    }
    eng.patterns = p.pattern;
    match &p.tape {
        Some(t) => {
            let chops: Vec<(usize, usize)> = t
                .chops
                .iter()
                .map(|&[a, b]| (a.min(b), b.max(a)))
                .filter(|&(a, b)| b > a)
                .collect();
            eng.tape = Some(Tape {
                name: t.name.clone(),
                audio: Vec::new(), // markers only; drop the .wav to fill
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
    let pads: [NativePad; 8] = std::array::from_fn(|i| NativePad {
        tune: eng.pads[i].tune_st,
        level: eng.pads[i].level,
        chop: eng.chop_map[i],
    });
    let tape = eng.tape.as_ref().map(|t| NativeTape {
        name: t.name.clone(),
        chops: t.chops.iter().map(|&(a, b)| [a, b]).collect(),
    });
    let p = NativeProject {
        format: "sp1200-rs".to_string(),
        version: 1,
        name: name.to_string(),
        bpm: eng.bpm,
        swing: eng.swing,
        master: eng.master,
        pads,
        pattern: eng.patterns,
        tape,
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
        bpm: p.bpm.unwrap_or(100.0).clamp(60.0, 200.0),
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
