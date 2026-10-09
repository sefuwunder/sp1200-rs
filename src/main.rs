//! sp1200-rs: native Rust + SDL2 port of the SP-1200 drum machine.
//!
//! - No args: launch the playable faceplate UI.
//! - `--render out.wav [seconds]`: render the fixed demo pattern to WAV
//!   (deterministic; used by the byte-parity test against the JS core).
//! - `--shot out.bmp`: render one UI frame offscreen to BMP (headless check).

#![deny(warnings)]

mod audio;
mod dsp;
mod engine;
mod ui;

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sdl2::pixels::PixelFormatEnum;
use sdl2::surface::Surface;

use crate::engine::{fit_to_seconds, render_demo_i16, Engine, RENDER_SEED};
use crate::ui::{draw, handle_event, Painter, Ui, H, W};

fn write_wav(path: &str, samples: &[i16], sample_rate: u32) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    let data_len = samples.len() * 2;
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&((36 + data_len) as u32).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
    h[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    h[28..32].copy_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    h[32..34].copy_from_slice(&2u16.to_le_bytes()); // block align
    h[34..36].copy_from_slice(&16u16.to_le_bytes()); // bits
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&(data_len as u32).to_le_bytes());
    f.write_all(&h)?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}

fn cmd_render(out: &str, seconds: Option<f64>) -> i32 {
    let demo = render_demo_i16(RENDER_SEED);
    let total_s = demo.len() as f64 / dsp::SP_RATE;
    let secs = seconds.unwrap_or(total_s);
    let fitted = fit_to_seconds(&demo, secs);
    match write_wav(out, &fitted, dsp::SP_RATE as u32) {
        Ok(()) => {
            let peak = fitted
                .iter()
                .map(|v| v.unsigned_abs() as i32)
                .max()
                .unwrap_or(0);
            println!(
                "rendered {} samples ({:.2}s @ {} Hz), peak {}",
                fitted.len(),
                fitted.len() as f64 / dsp::SP_RATE,
                dsp::SP_RATE as u32,
                peak
            );
            0
        }
        Err(e) => {
            eprintln!("render failed: {e}");
            1
        }
    }
}

fn cmd_shot(out: &str) -> i32 {
    let mut eng = Engine::new();
    eng.play();
    let mut warm = vec![0.0f32; 20000];
    eng.render(&mut warm);
    let ui = Ui::new();
    let mut surf = match Surface::new(W, H, PixelFormatEnum::RGB24) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("surface failed: {e}");
            return 1;
        }
    };
    {
        let p: &mut dyn Painter = &mut surf;
        draw(p, &ui, &eng);
    }
    match surf.save_bmp(out) {
        Ok(()) => {
            println!("wrote {out}");
            0
        }
        Err(e) => {
            eprintln!("bmp failed: {e}");
            1
        }
    }
}

fn run_app() -> i32 {
    run_app_for(None)
}

/// Like [`run_app`], but exits 0 after `secs` seconds (headless smoke test).
fn run_app_for(smoke_secs: Option<f64>) -> i32 {
    let sdl = match sdl2::init() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("SDL init failed: {e}");
            return 1;
        }
    };
    let video = match sdl.video() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("video failed: {e}");
            return 1;
        }
    };
    let window = match video.window("SP-1200 RS", W, H).position_centered().build() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("window failed: {e}");
            return 1;
        }
    };
    let mut canvas = match window.into_canvas().present_vsync().build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("canvas failed: {e}");
            return 1;
        }
    };
    let audio_sub = match sdl.audio() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("audio failed: {e}");
            return 1;
        }
    };

    let engine = Arc::new(Mutex::new(Engine::new()));
    let _device = match audio::open_device(&audio_sub, Arc::clone(&engine)) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("audio device failed: {e}");
            return 1;
        }
    };

    let mut ui = Ui::new();
    let mut pump = match sdl.event_pump() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("event pump failed: {e}");
            return 1;
        }
    };
    println!("SP-1200 RS — SPACE play/stop, 1-8 pads, F1-F3 presets, ESC quit");
    let t0 = std::time::Instant::now();
    'run: loop {
        if let Some(secs) = smoke_secs {
            if t0.elapsed().as_secs_f64() >= secs {
                break 'run;
            }
        }
        for ev in pump.poll_iter() {
            let eng = &mut *engine.lock().unwrap();
            if handle_event(&mut ui, eng, &ev) {
                break 'run;
            }
        }
        {
            let eng = engine.lock().unwrap();
            let p: &mut dyn Painter = &mut canvas;
            draw(p, &ui, &eng);
        }
        canvas.present();
        // The canvas is vsync'd; this is just a floor for non-vsync drivers.
        std::thread::sleep(Duration::from_millis(2));
    }
    0
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = match args.get(1).map(|s| s.as_str()) {
        Some("--render") => {
            let path = args.get(2).map(|s| s.as_str()).unwrap_or("out.wav");
            let secs = args.get(3).and_then(|s| s.parse::<f64>().ok());
            // Allow `--render out.wav 4.8` (path first).
            if path.starts_with("--") {
                eprintln!("usage: sp1200-rs --render out.wav [seconds]");
                1
            } else {
                cmd_render(path, secs)
            }
        }
        Some("--shot") => {
            let path = args.get(2).map(|s| s.as_str()).unwrap_or("shot.bmp");
            cmd_shot(path)
        }
        Some("--smoke") => {
            let secs = args
                .get(2)
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(3.0);
            run_app_for(Some(secs))
        }
        Some("--help") | Some("-h") => {
            println!("sp1200-rs — SP-1200 drum machine (Rust + SDL2)");
            println!("  sp1200-rs                  launch the faceplate UI");
            println!("  sp1200-rs --render out.wav [seconds]");
            println!("                             render the fixed demo pattern to WAV");
            println!("  sp1200-rs --shot out.bmp   render one UI frame offscreen to BMP");
            println!("  sp1200-rs --smoke [secs]   run the UI headless for N seconds, exit 0");
            0
        }
        Some(other) => {
            eprintln!("unknown arg: {other} (try --help)");
            1
        }
        None => run_app(),
    };
    std::process::exit(code);
}
