//! Minimal zero-dependency WAV decoder.
//!
//! Handles 16-bit PCM and 32-bit float, mono or more channels (mixed down to
//! mono), any sane sample rate. Unknown chunks are skipped. Every malformed
//! input produces a [`WavError`] — never a panic.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WavError(pub String);

impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "wav: {}", self.0)
    }
}

impl std::error::Error for WavError {}

/// Decoded mono audio, roughly -1.0..1.0.
pub struct WavData {
    pub sample_rate: u32,
    pub samples: Vec<f32>,
}

fn le_u16(b: &[u8]) -> Option<u16> {
    b.get(0..2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}

fn le_u32(b: &[u8]) -> Option<u32> {
    b.get(0..4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn le_i16(b: &[u8]) -> Option<i16> {
    b.get(0..2).map(|s| i16::from_le_bytes([s[0], s[1]]))
}

fn le_f32(b: &[u8]) -> Option<f32> {
    b.get(0..4)
        .map(|s| f32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

struct Format {
    audio_format: u16,
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
}

pub fn decode_wav(data: &[u8]) -> Result<WavData, WavError> {
    let err = |m: &str| WavError(m.to_string());
    if data.len() < 12 {
        return Err(err("too short for a RIFF header"));
    }
    if &data[0..4] != b"RIFF" {
        return Err(err("not a RIFF file"));
    }
    if &data[8..12] != b"WAVE" {
        return Err(err("not a WAVE file"));
    }

    let mut fmt: Option<Format> = None;
    let mut samples: Option<Vec<f32>> = None;

    // Walk subchunks; each is id(4) + size(4) + body, word-aligned.
    let mut pos = 12usize;
    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let size = le_u32(&data[pos + 4..pos + 8]).unwrap_or(0) as usize;
        let body_start = pos + 8;
        // Clamp the body to the actual input; the *next* chunk is still
        // located by the declared size so truncated files error, not panic.
        let next = body_start.saturating_add(size).saturating_add(size & 1);
        if next <= pos {
            break; // overflow guard
        }
        let body_end = body_start.saturating_add(size).min(data.len());
        let body = &data[body_start..body_end];

        match id {
            b"fmt " => {
                if body.len() < 16 {
                    return Err(err("truncated fmt chunk"));
                }
                let f = Format {
                    audio_format: le_u16(&body[0..2]).unwrap_or(0),
                    channels: le_u16(&body[2..4]).unwrap_or(0),
                    sample_rate: le_u32(&body[4..8]).unwrap_or(0),
                    bits_per_sample: le_u16(&body[14..16]).unwrap_or(0),
                };
                if f.channels == 0 || f.channels > 32 {
                    return Err(err("bad channel count"));
                }
                if f.sample_rate == 0 || f.sample_rate > 384_000 {
                    return Err(err("bad sample rate"));
                }
                match (f.audio_format, f.bits_per_sample) {
                    (1, 16) | (3, 32) => {}
                    _ => {
                        return Err(err(&format!(
                            "unsupported format (tag {}, {}-bit); need 16-bit PCM or 32-bit float",
                            f.audio_format, f.bits_per_sample
                        )));
                    }
                }
                fmt = Some(f);
            }
            b"data" => {
                let f = fmt
                    .as_ref()
                    .ok_or_else(|| err("data chunk before fmt chunk"))?;
                let decoded = match (f.audio_format, f.bits_per_sample) {
                    (1, 16) => decode_pcm16(body, f.channels)?,
                    (3, 32) => decode_f32(body, f.channels)?,
                    _ => unreachable!("format validated above"),
                };
                if decoded.is_empty() {
                    return Err(err("no audio samples"));
                }
                samples = Some(decoded);
            }
            _ => {} // LIST, fact, bext, cue …: skip
        }
        pos = next.min(data.len() + 1);
        if pos > data.len() {
            break;
        }
    }

    let f = fmt.ok_or_else(|| err("no fmt chunk"))?;
    let samples = samples.ok_or_else(|| err("no data chunk"))?;
    Ok(WavData {
        sample_rate: f.sample_rate,
        samples,
    })
}

/// 16-bit PCM frames → mono f32 (channels averaged).
fn decode_pcm16(body: &[u8], channels: u16) -> Result<Vec<f32>, WavError> {
    let ch = channels as usize;
    let frame = ch * 2;
    if body.len() < frame {
        return Err(WavError("truncated pcm16 data".to_string()));
    }
    let n_frames = body.len() / frame;
    let mut out = Vec::with_capacity(n_frames);
    for i in 0..n_frames {
        let mut acc = 0.0f32;
        for c in 0..ch {
            let off = i * frame + c * 2;
            let s = le_i16(&body[off..off + 2])
                .ok_or_else(|| WavError("truncated pcm16 sample".to_string()))?;
            acc += s as f32 / 32768.0;
        }
        out.push(acc / ch as f32);
    }
    Ok(out)
}

/// 32-bit float frames → mono f32 (channels averaged).
fn decode_f32(body: &[u8], channels: u16) -> Result<Vec<f32>, WavError> {
    let ch = channels as usize;
    let frame = ch * 4;
    if body.len() < frame {
        return Err(WavError("truncated float32 data".to_string()));
    }
    let n_frames = body.len() / frame;
    let mut out = Vec::with_capacity(n_frames);
    for i in 0..n_frames {
        let mut acc = 0.0f32;
        for c in 0..ch {
            let off = i * frame + c * 4;
            let s = le_f32(&body[off..off + 4])
                .ok_or_else(|| WavError("truncated float32 sample".to_string()))?;
            acc += s;
        }
        out.push(acc / ch as f32);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal WAV in memory.
    fn wav_bytes(
        audio_format: u16,
        channels: u16,
        sample_rate: u32,
        bits: u16,
        frames: &[Vec<f32>],
    ) -> Vec<u8> {
        let mut data = Vec::new();
        let bytes_per_sample = bits as usize / 8;
        for fr in frames {
            for c in 0..channels as usize {
                let v = fr[c % fr.len()].clamp(-1.0, 1.0);
                match (audio_format, bits) {
                    (1, 16) => {
                        let s = (v * 32767.0).round() as i16;
                        data.extend_from_slice(&s.to_le_bytes());
                    }
                    (1, 8) => {
                        // Unsigned 8-bit, for the unsupported-format test.
                        data.push((v * 127.0 + 128.0).round() as u8);
                    }
                    (3, 32) => data.extend_from_slice(&v.to_le_bytes()),
                    _ => panic!("test only"),
                }
            }
        }
        let _ = bytes_per_sample;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&0u32.to_le_bytes()); // size (ignored)
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&audio_format.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        let byte_rate = sample_rate * channels as u32 * bits as u32 / 8;
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    #[test]
    fn decode_pcm16_mono() {
        let w = wav_bytes(1, 1, 44100, 16, &[vec![0.5], vec![-0.25], vec![0.0]]);
        let d = decode_wav(&w).unwrap();
        assert_eq!(d.sample_rate, 44100);
        assert_eq!(d.samples.len(), 3);
        assert!((d.samples[0] - 0.5).abs() < 0.001);
        assert!((d.samples[1] + 0.25).abs() < 0.001);
        assert_eq!(d.samples[2], 0.0);
    }

    #[test]
    fn decode_pcm16_stereo_mixes_to_mono() {
        // L = +0.5, R = -0.5 → mono 0.0
        let w = wav_bytes(1, 2, 48000, 16, &[vec![0.5, -0.5], vec![1.0, 1.0]]);
        let d = decode_wav(&w).unwrap();
        assert_eq!(d.samples.len(), 2);
        assert!(d.samples[0].abs() < 0.002);
        assert!((d.samples[1] - 1.0).abs() < 0.002);
    }

    #[test]
    fn decode_float32() {
        let w = wav_bytes(3, 1, 22050, 32, &[vec![0.75], vec![-0.75]]);
        let d = decode_wav(&w).unwrap();
        assert_eq!(d.sample_rate, 22050);
        assert!((d.samples[0] - 0.75).abs() < 1e-6);
        assert!((d.samples[1] + 0.75).abs() < 1e-6);
    }

    #[test]
    fn skips_unknown_chunks() {
        let mut w = wav_bytes(1, 1, 44100, 16, &[vec![0.5]]);
        // Splice a JUNK chunk between "WAVE" and "fmt ".
        let mut with_junk = w[..12].to_vec();
        with_junk.extend_from_slice(b"JUNK");
        with_junk.extend_from_slice(&6u32.to_le_bytes());
        with_junk.extend_from_slice(b"abcdef"); // 6 bytes, no pad needed
        with_junk.extend_from_slice(&w[12..]);
        w = with_junk;
        let d = decode_wav(&w).unwrap();
        assert_eq!(d.samples.len(), 1);
    }

    #[test]
    fn garbage_is_err_not_panic() {
        assert!(decode_wav(b"").is_err());
        assert!(decode_wav(b"hello world").is_err());
        assert!(decode_wav(b"RIFF\x00\x00\x00\x00WAVE").is_err()); // no chunks
                                                                   // Truncated header mid-chunk
        let mut w = wav_bytes(1, 1, 44100, 16, &[vec![0.5]]);
        w.truncate(20);
        assert!(decode_wav(&w).is_err());
        // 8-bit PCM is unsupported
        let w8 = wav_bytes(1, 1, 44100, 8, &[vec![0.5]]);
        assert!(decode_wav(&w8).is_err());
        // Unknown chunk id where "data" should be → no data chunk
        let mut w = wav_bytes(1, 1, 44100, 16, &[vec![0.5]]);
        let at = w.windows(4).position(|s| s == b"data").unwrap();
        w[at..at + 4].copy_from_slice(b"datX");
        assert!(decode_wav(&w).is_err());
    }
}
