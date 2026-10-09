//! SP-1200-style 2D faceplate: pads, step LEDs, transport, readouts.
//!
//! Text uses a tiny built-in 5x7 bitmap font — no font libraries, no extra
//! system dependencies. Drawing goes through [`Painter`] so the same code
//! renders to the window canvas and to an offscreen surface (for `--shot`).

use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::mouse::MouseButton;
use sdl2::pixels::Color;
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::surface::Surface;
use sdl2::video::Window;
use std::time::Instant;

use crate::engine::{Engine, N_PADS, N_STEPS};

pub const W: u32 = 1024;
pub const H: u32 = 640;

// ---------- palette: dark plate, warm accents ----------

const BG: (u8, u8, u8) = (18, 14, 10);
const PLATE: (u8, u8, u8) = (28, 22, 16);
const PAD: (u8, u8, u8) = (44, 35, 26);
const PAD_EDGE: (u8, u8, u8) = (78, 62, 46);
const PAD_HIT: (u8, u8, u8) = (255, 157, 46);
const TEXT: (u8, u8, u8) = (232, 220, 200);
const DIM: (u8, u8, u8) = (150, 132, 110);
const LED_ON: (u8, u8, u8) = (255, 179, 71);
const LED_OFF: (u8, u8, u8) = (56, 45, 36);
const STEP_ON: (u8, u8, u8) = (96, 70, 44);
const STEP_OFF: (u8, u8, u8) = (36, 29, 22);
const PLAYHEAD: (u8, u8, u8) = (255, 96, 64);
const SEL: (u8, u8, u8) = (255, 157, 46);

// ---------- 5x7 bitmap font ----------

/// Glyph rows, top to bottom; low 5 bits of each byte are the pixels.
fn glyph(c: char) -> Option<[u8; 7]> {
    Some(match c {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x19, 0x15, 0x13, 0x13, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x1B, 0x11],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0A, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        ' ' => [0, 0, 0, 0, 0, 0, 0],
        '-' => [0, 0, 0, 0x1F, 0, 0, 0],
        '.' => [0, 0, 0, 0, 0, 0x0C, 0x0C],
        ':' => [0, 0x0C, 0x0C, 0, 0x0C, 0x0C, 0],
        '/' => [0x01, 0x01, 0x02, 0x04, 0x08, 0x10, 0x10],
        '%' => [0x19, 0x1A, 0x02, 0x04, 0x08, 0x14, 0x13],
        '+' => [0, 0x04, 0x04, 0x1F, 0x04, 0x04, 0],
        '<' => [0x02, 0x04, 0x08, 0x10, 0x08, 0x04, 0x02],
        '>' => [0x08, 0x04, 0x02, 0x01, 0x02, 0x04, 0x08],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        '!' => [0x04, 0x04, 0x04, 0x04, 0x04, 0, 0x04],
        '?' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0, 0x04],
        '#' => [0x0A, 0x0A, 0x1F, 0x0A, 0x1F, 0x0A, 0x0A],
        '=' => [0, 0, 0x1F, 0, 0x1F, 0, 0],
        _ => return None,
    })
}

// ---------- painter abstraction ----------

pub trait Painter {
    fn rect(&mut self, x: i32, y: i32, w: u32, h: u32, c: (u8, u8, u8));

    fn text(&mut self, x: i32, y: i32, s: &str, scale: u32, c: (u8, u8, u8)) {
        let mut cx = x;
        for ch in s.to_uppercase().chars() {
            if let Some(g) = glyph(ch) {
                for (row, bits) in g.iter().enumerate() {
                    for col in 0..5 {
                        if bits & (1 << (4 - col)) != 0 {
                            self.rect(
                                cx + col as i32 * scale as i32,
                                y + row as i32 * scale as i32,
                                scale,
                                scale,
                                c,
                            );
                        }
                    }
                }
            }
            cx += 6 * scale as i32;
        }
    }
}

/// Pixel width of a string at a scale (no Painter needed).
pub fn text_w(s: &str, scale: u32) -> u32 {
    s.chars().count() as u32 * 6 * scale
}

impl Painter for Canvas<Window> {
    fn rect(&mut self, x: i32, y: i32, w: u32, h: u32, c: (u8, u8, u8)) {
        self.set_draw_color(Color::RGB(c.0, c.1, c.2));
        let _ = self.fill_rect(Rect::new(x, y, w, h));
    }
}

impl Painter for Surface<'_> {
    fn rect(&mut self, x: i32, y: i32, w: u32, h: u32, c: (u8, u8, u8)) {
        let _ = self.fill_rect(Rect::new(x, y, w, h), Color::RGB(c.0, c.1, c.2));
    }
}

// ---------- layout ----------

fn pad_rect(i: usize) -> (i32, i32, u32, u32) {
    let col = (i % 4) as i32;
    let row = (i / 4) as i32;
    let w = 232u32;
    let h = 118u32;
    (
        24 + col * (w as i32 + 16),
        100 + row * (h as i32 + 16),
        w,
        h,
    )
}

fn step_rect(i: usize) -> (i32, i32, u32, u32) {
    let w = 56u32;
    let gap = 6u32;
    let total = N_STEPS as u32 * w + (N_STEPS as u32 - 1) * gap;
    let x0 = (W - total) as i32 / 2;
    (x0 + i as i32 * (w as i32 + gap as i32), 452, w, 54)
}

fn hit_pad(x: i32, y: i32) -> Option<usize> {
    (0..N_PADS).find(|&i| {
        let (px, py, w, h) = pad_rect(i);
        x >= px && x < px + w as i32 && y >= py && y < py + h as i32
    })
}

fn hit_step(x: i32, y: i32) -> Option<usize> {
    (0..N_STEPS).find(|&i| {
        let (px, py, w, h) = step_rect(i);
        x >= px && x < px + w as i32 && y >= py && y < py + h as i32
    })
}

// ---------- ui state ----------

pub struct Ui {
    pub selected_pad: usize,
    pub preset: usize,
    taps: Vec<Instant>,
}

impl Ui {
    pub fn new() -> Self {
        Self {
            selected_pad: 0,
            preset: 0,
            taps: Vec::new(),
        }
    }
}

/// Handle one SDL event. Returns true when the app should quit.
pub fn handle_event(ui: &mut Ui, eng: &mut Engine, ev: &Event) -> bool {
    match ev {
        Event::Quit { .. } => return true,
        Event::KeyDown {
            keycode: Some(k), ..
        } => match *k {
            Keycode::Escape | Keycode::Q => return true,
            Keycode::Space => {
                if eng.playing {
                    eng.stop();
                } else {
                    eng.play();
                }
            }
            Keycode::Num1 => {
                ui.selected_pad = 0;
                eng.trigger(0);
            }
            Keycode::Num2 => {
                ui.selected_pad = 1;
                eng.trigger(1);
            }
            Keycode::Num3 => {
                ui.selected_pad = 2;
                eng.trigger(2);
            }
            Keycode::Num4 => {
                ui.selected_pad = 3;
                eng.trigger(3);
            }
            Keycode::Num5 => {
                ui.selected_pad = 4;
                eng.trigger(4);
            }
            Keycode::Num6 => {
                ui.selected_pad = 5;
                eng.trigger(5);
            }
            Keycode::Num7 => {
                ui.selected_pad = 6;
                eng.trigger(6);
            }
            Keycode::Num8 => {
                ui.selected_pad = 7;
                eng.trigger(7);
            }
            Keycode::T => {
                let now = Instant::now();
                ui.taps
                    .retain(|t| now.duration_since(*t).as_secs_f64() < 2.0);
                ui.taps.push(now);
                if ui.taps.len() >= 2 {
                    let iv: Vec<f64> = ui
                        .taps
                        .windows(2)
                        .map(|w| w[1].duration_since(w[0]).as_secs_f64())
                        .collect();
                    let avg = iv.iter().sum::<f64>() / iv.len() as f64;
                    if avg > 0.0 {
                        eng.bpm = (60.0 / avg).clamp(60.0, 200.0).round();
                    }
                }
            }
            Keycode::Left => eng.swing = (eng.swing - 1.0).clamp(50.0, 75.0),
            Keycode::Right => eng.swing = (eng.swing + 1.0).clamp(50.0, 75.0),
            Keycode::Up => eng.master = (eng.master + 0.05).clamp(0.0, 1.0),
            Keycode::Down => eng.master = (eng.master - 0.05).clamp(0.0, 1.0),
            Keycode::Comma => eng.bpm = (eng.bpm - 1.0).clamp(60.0, 200.0),
            Keycode::Period => eng.bpm = (eng.bpm + 1.0).clamp(60.0, 200.0),
            Keycode::F1 => {
                eng.apply_preset(0);
                ui.preset = 0;
            }
            Keycode::F2 => {
                eng.apply_preset(1);
                ui.preset = 1;
            }
            Keycode::F3 => {
                eng.apply_preset(2);
                ui.preset = 2;
            }
            _ => {}
        },
        Event::MouseButtonDown {
            mouse_btn: MouseButton::Left,
            x,
            y,
            ..
        } => {
            if let Some(i) = hit_pad(*x, *y) {
                ui.selected_pad = i;
                eng.trigger(i);
            } else if let Some(s) = hit_step(*x, *y) {
                let p = ui.selected_pad;
                eng.patterns[p][s] = !eng.patterns[p][s];
            }
        }
        _ => {}
    }
    false
}

// ---------- drawing ----------

const DRUM_LABELS: [&str; 8] = [
    "KICK", "SNARE", "CLAP", "RIM", "CHAT", "OHAT", "TOM", "SHAKER",
];

fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    (
        (a.0 as f32 + (b.0 as f32 - a.0 as f32) * t) as u8,
        (a.1 as f32 + (b.1 as f32 - a.1 as f32) * t) as u8,
        (a.2 as f32 + (b.2 as f32 - a.2 as f32) * t) as u8,
    )
}

pub fn draw(p: &mut dyn Painter, ui: &Ui, eng: &Engine) {
    p.rect(0, 0, W, H, BG);
    // header plate
    p.rect(0, 0, W, 76, PLATE);
    p.text(24, 22, "SP-1200 RS", 3, TEXT);
    p.text(24, 52, "12-BIT DRUM MACHINE", 1, DIM);

    let bpm_s = format!("BPM {:03.0}", eng.bpm);
    let sw_s = format!("SWING {:02.0}%", eng.swing);
    let mst_s = format!("MASTER {:02.0}", eng.master * 100.0);
    let mut rx = W as i32 - 24;
    for s in [&mst_s, &sw_s, &bpm_s] {
        let w = text_w(s, 2) as i32;
        rx -= w;
        p.text(rx, 26, s, 2, TEXT);
        rx -= 36;
    }
    p.text(24, 84, "PADS", 1, DIM);

    // pads
    for i in 0..N_PADS {
        let (x, y, w, h) = pad_rect(i);
        let age = eng.clock().saturating_sub(eng.pad_hit_at[i]) as f32;
        let flash = 1.0 - age / (0.14 * crate::dsp::SP_RATE as f32);
        let fill = mix(PAD, PAD_HIT, flash);
        p.rect(x, y, w, h, fill);
        p.rect(x, y, w, 3, PAD_EDGE);
        p.rect(x, y + h as i32 - 3, w, 3, PAD_EDGE);
        p.rect(x, y, 3, h, PAD_EDGE);
        p.rect(x + w as i32 - 3, y, 3, h, PAD_EDGE);
        if i == ui.selected_pad {
            p.rect(x - 4, y - 4, w + 8, 4, SEL);
            p.rect(x - 4, y + h as i32, w + 8, 4, SEL);
        }
        let label = DRUM_LABELS[i];
        let tw = text_w(label, 2) as i32;
        p.text(x + (w as i32 - tw) / 2, y + 34, label, 2, TEXT);
        let key = format!("{}", i + 1);
        p.text(x + 10, y + 10, &key, 1, DIM);
    }

    // steps
    p.text(24, 384, "STEPS", 1, DIM);
    let pat = eng.patterns[ui.selected_pad];
    for i in 0..N_STEPS {
        let (x, y, w, h) = step_rect(i);
        let is_head = eng.playing && eng.last_step as usize == i;
        let led = if pat[i] { LED_ON } else { LED_OFF };
        let led_c = if is_head { PLAYHEAD } else { led };
        p.rect(x, y - 16, w, 10, led_c);
        let fill = if pat[i] { STEP_ON } else { STEP_OFF };
        p.rect(x, y, w, h, fill);
        if is_head {
            p.rect(x, y, w, 4, PLAYHEAD);
        }
        let num = format!("{}", i + 1);
        let tw = text_w(&num, 1) as i32;
        p.text(x + (w as i32 - tw) / 2, y + 20, &num, 1, DIM);
    }
    let sel_name = DRUM_LABELS[ui.selected_pad];
    p.text(120, 384, &format!("EDITING: {}", sel_name), 1, SEL);

    // presets
    let mut px = 24;
    p.text(px, 532, "PRESET", 1, DIM);
    px += text_w("PRESET  ", 1) as i32;
    for i in 0..3 {
        let nm = format!("F{} {}", i + 1, Engine::preset_name(i));
        let c = if i == ui.preset { SEL } else { DIM };
        p.text(px, 532, &nm, 1, c);
        px += text_w(&format!("{}   ", nm), 1) as i32;
    }
    if eng.playing {
        p.text(W as i32 - 200, 532, "> PLAYING", 2, PLAYHEAD);
    } else {
        p.text(W as i32 - 200, 532, "STOPPED", 2, DIM);
    }

    // footer help
    p.rect(0, 566, W, H - 566, PLATE);
    p.text(
        24,
        584,
        "SPACE PLAY/STOP  T TAP  </> SWING  UP/DN MASTER  ,/. BPM  1-8 PADS  F1-F3 PRESETS  ESC QUIT",
        1,
        DIM,
    );
    p.text(
        24,
        606,
        "CLICK PADS TO HIT - CLICK STEPS TO PROGRAM",
        1,
        DIM,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_covers_needed_chars() {
        for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 -.:/%+<>()!?#=".chars() {
            assert!(glyph(c).is_some(), "missing glyph for {c:?}");
        }
    }

    #[test]
    fn text_width_math() {
        assert_eq!(text_w("AB", 1), 12);
        assert_eq!(text_w("AB", 2), 24);
    }
}
