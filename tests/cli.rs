//! Headless CLI tests: --tape + --smoke, --project round-trip via the real binary.
//!
//! The binary is located next to the test executable (target/debug or
//! target/release), so this works regardless of the CARGO_BIN_EXE env name.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop(); // deps/
    p.pop(); // debug/ or release/
    p.join(if cfg!(windows) {
        "sp1200-rs.exe"
    } else {
        "sp1200-rs"
    })
}

/// Minimal 16-bit mono WAV writer.
fn write_wav(path: &std::path::Path, samples: &[i16], sample_rate: u32) {
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(samples.len() as u32 * 2).to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, out).unwrap();
}

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sp1200rs-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Click track: 6 transients, 0.3 s apart @44.1 kHz.
fn click_track() -> Vec<i16> {
    let gap = (0.3 * 44100.0) as usize;
    let mut v = vec![0i16; gap * 6 + 1000];
    for k in 0..6 {
        v[k * gap] = 16000;
    }
    v
}

#[test]
fn tape_smoke_exits_zero() {
    let dir = tmpdir("tape");
    let wav = dir.join("tape.wav");
    write_wav(&wav, &click_track(), 44100);
    let out = Command::new(bin())
        .args(["--tape", wav.to_str().unwrap(), "--smoke", "2"])
        .env("SDL_VIDEODRIVER", "dummy")
        .env("SDL_AUDIODRIVER", "dummy")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "exit {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("CHOPS"), "no tape load message: {stdout}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn project_round_trip_through_cli() {
    let dir = tmpdir("proj");
    let wav = dir.join("loop.wav");
    write_wav(&wav, &click_track(), 44100);
    let proj = dir.join("p.json");
    // Boot with tape+project-less state, save a project via --smoke is not
    // possible (W is interactive), so craft the native JSON directly and
    // verify the binary loads it.
    let json = r#"{"format":"sp1200-rs","version":1,"name":"cli-test","bpm":125.0,"swing":70.0,
        "master":0.9,
        "pads":[{"tune":0.0,"level":0.9},{"tune":2.0,"level":0.8},{"tune":0.0,"level":0.9},
                {"tune":0.0,"level":0.9},{"tune":0.0,"level":0.9},{"tune":0.0,"level":0.9},
                {"tune":0.0,"level":0.9},{"tune":-3.0,"level":0.9}],
        "pattern":[[true,false,false,false,false,false,false,false,false,false,false,false,false,false,false,false],
                  [false],[false],[false],[false],[false],[false],[false]],
        "tape":{"name":"LOOP","chops":[[0,6615],[6615,13230]]}}"#;
    // Note: pattern rows must be length 16; pad the short ones via JSON gen.
    let mut v: serde_json::Value = serde_json::from_str(json).unwrap();
    for row in v["pattern"].as_array_mut().unwrap().iter_mut() {
        while row.as_array().unwrap().len() < 16 {
            row.as_array_mut()
                .unwrap()
                .push(serde_json::Value::Bool(false));
        }
    }
    std::fs::write(&proj, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let out = Command::new(bin())
        .args([
            "--project",
            proj.to_str().unwrap(),
            "--tape",
            wav.to_str().unwrap(),
            "--smoke",
            "2",
        ])
        .env("SDL_VIDEODRIVER", "dummy")
        .env("SDL_AUDIODRIVER", "dummy")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "exit {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Project loads first, then the tape consumes its chop markers.
    assert!(stdout.contains("PROJECT LOADED"), "stdout: {stdout}");
    assert!(stdout.contains("2 CHOPS"), "stdout: {stdout}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn bad_project_is_a_clean_error() {
    let dir = tmpdir("badproj");
    let proj = dir.join("bad.json");
    std::fs::write(&proj, "{oops").unwrap();
    let out = Command::new(bin())
        .args(["--project", proj.to_str().unwrap(), "--smoke", "1"])
        .env("SDL_VIDEODRIVER", "dummy")
        .env("SDL_AUDIODRIVER", "dummy")
        .output()
        .unwrap();
    // Startup continues into the UI (exit 0); the error shows on screen,
    // not as a crash. (Unit tests cover the parse error itself.)
    assert!(out.status.success());
    std::fs::remove_dir_all(&dir).ok();
}
