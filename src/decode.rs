//! Audio file decoding: WAV goes through the zero-dependency reader in
//! `wav.rs`; MP3 through minimp3 (bundled C decoder, no system deps).
//!
//! Everything lands as mono f32, roughly -1.0..1.0, at the file's native
//! sample rate — the 12-bit SP path (`dsp::sp1200ize_i16`) handles the rest.
//! Dispatch is by file extension; MP3s with ID3 tags are fine (minimp3
//! skips them).

use crate::wav;

/// Decoded mono audio, roughly -1.0..1.0.
pub struct AudioData {
    pub sample_rate: u32,
    pub samples: Vec<f32>,
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Decode an audio file by extension (.wav / .mp3).
pub fn decode_file(path: &str) -> Result<AudioData, String> {
    let data = std::fs::read(path).map_err(|e| format!("can't read {}: {e}", file_name(path)))?;
    decode_bytes(&data, path)
}

/// Decode raw bytes; `path` is only used for the extension + messages.
pub fn decode_bytes(data: &[u8], path: &str) -> Result<AudioData, String> {
    let lower = path.to_lowercase();
    if lower.ends_with(".mp3") {
        decode_mp3(data).map_err(|e| format!("{}: {e}", file_name(path)))
    } else if lower.ends_with(".wav") {
        let w = wav::decode_wav(data).map_err(|e| format!("{}: {e}", file_name(path)))?;
        Ok(AudioData {
            sample_rate: w.sample_rate,
            samples: w.samples,
        })
    } else {
        Err(format!("unsupported audio: {}", file_name(path)))
    }
}

fn decode_mp3(data: &[u8]) -> Result<AudioData, String> {
    let mut dec = minimp3::Decoder::new(data);
    let mut samples: Vec<f32> = Vec::new();
    let mut sample_rate = 0u32;
    loop {
        match dec.next_frame() {
            Ok(frame) => {
                if sample_rate == 0 {
                    sample_rate = frame.sample_rate as u32;
                }
                // Interleaved i16 -> mono f32.
                let ch = frame.channels.max(1) as usize;
                samples.reserve(frame.data.len() / ch);
                for chunk in frame.data.chunks(ch) {
                    let mut acc = 0i32;
                    for &s in chunk {
                        acc += s as i32;
                    }
                    samples.push(acc as f32 / (chunk.len() as f32 * 32768.0));
                }
            }
            Err(minimp3::Error::Eof) => break,
            Err(e) => return Err(format!("mp3 decode: {e:?}")),
        }
    }
    if samples.is_empty() {
        return Err("mp3: no audio frames".to_string());
    }
    Ok(AudioData {
        sample_rate,
        samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal mono 16-bit WAV in memory.
    fn wav_bytes(samples: &[i16]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&((36 + samples.len() * 2) as u32).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&44100u32.to_le_bytes());
        b.extend_from_slice(&88200u32.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&((samples.len() * 2) as u32).to_le_bytes());
        for s in samples {
            b.extend_from_slice(&s.to_le_bytes());
        }
        b
    }

    #[test]
    fn dispatch_by_extension() {
        let wav = wav_bytes(&[0, 1000, -1000, 0]);
        let a = decode_bytes(&wav, "x.wav").unwrap();
        assert_eq!(a.sample_rate, 44100);
        assert_eq!(a.samples.len(), 4);
        let a = decode_bytes(&wav, "x.WAV").unwrap();
        assert_eq!(a.samples.len(), 4);
        assert!(decode_bytes(&wav, "x.ogg").is_err());
        assert!(decode_bytes(&[0, 1, 2, 3], "x.mp3").is_err()); // not an mp3
    }

    #[test]
    fn mp3_fixture_decodes() {
        let data = std::fs::read(fixture("sine.mp3")).unwrap();
        let a = decode_bytes(&data, "sine.mp3").unwrap();
        assert_eq!(a.sample_rate, 44100);
        // 1 s of audio, minus encoder padding/delay slop.
        assert!(
            (a.samples.len() as i32 - 44100).abs() < 6000, // encoder delay+padding
            "len {}",
            a.samples.len()
        );
        assert!(a.samples.iter().all(|v| v.is_finite()));
        let peak = a.samples.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        assert!(peak > 0.5, "too quiet: {peak}");
        assert!(peak <= 1.0, "clipped: {peak}");
    }

    #[test]
    fn mp3_with_id3_tag_decodes() {
        let data = std::fs::read(fixture("tagged.mp3")).unwrap();
        let a = decode_bytes(&data, "tagged.mp3").unwrap();
        assert!(!a.samples.is_empty());
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests");
        p.push("data");
        p.push(name);
        p
    }
}
