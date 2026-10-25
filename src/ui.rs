//! SP-1200-style 2D faceplate: pads, step LEDs, transport, readouts.
//!
//! Text uses a tiny built-in 5x7 bitmap font — no font libraries, no extra
//! system dependencies. Drawing goes through [`Painter`] so the same code
//! renders to the window canvas and to an offscreen surface (for `--shot`).

use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::keyboard::Mod;
use sdl2::mouse::MouseButton;
use sdl2::pixels::Color;
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::surface::Surface;
use sdl2::video::Window;
use std::time::Instant;

use crate::dsp::SP_RATE;
use crate::engine::{Engine, N_CHOKE_GROUPS, N_PADS, N_STEPS};
use crate::fourtrack::{TrackSource, N_TRACKS};

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
const REC: (u8, u8, u8) = (255, 64, 48);
const LOOPC: (u8, u8, u8) = (255, 190, 80);

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
        '[' => [0x0E, 0x08, 0x08, 0x08, 0x08, 0x08, 0x0E],
        ']' => [0x0E, 0x02, 0x02, 0x02, 0x02, 0x02, 0x0E],
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

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum Page {
    Pads,
    Edit,
    Tape,
}

pub struct Browser {
    files: Vec<String>,
    sel: usize,
}

pub struct Ui {
    pub selected_pad: usize,
    pub preset: usize,
    taps: Vec<Instant>,
    /// Where `W` saves the project (set by --project or a dropped .json).
    pub project_path: Option<String>,
    /// Transient status line: (text, show-until, is-error).
    msg: Option<(String, Instant, bool)>,
    /// Pads faceplate vs sample editor (Tab toggles).
    pub page: Page,
    /// Sample browser overlay (B opens, Enter loads into the selected pad).
    pub browser: Option<Browser>,
    /// Directory scanned by the browser (--samples-dir, default ./samples).
    pub samples_dir: String,
    /// Vim-style command line: Some(buf) while ':' editing is open.
    pub cmdline: Option<String>,
}

impl Ui {
    pub fn new() -> Self {
        Self {
            selected_pad: 0,
            preset: 0,
            taps: Vec::new(),
            project_path: None,
            msg: None,
            page: Page::Pads,
            browser: None,
            samples_dir: "./samples".to_string(),
            cmdline: None,
        }
    }

    /// Show a transient info message (4 s).
    pub fn say(&mut self, text: impl Into<String>) {
        let t: String = text.into().chars().take(110).collect();
        self.msg = Some((t, Instant::now() + std::time::Duration::from_secs(4), false));
    }

    /// Show a transient error message (6 s).
    pub fn say_err(&mut self, text: impl Into<String>) {
        let t: String = text.into().chars().take(110).collect();
        self.msg = Some((t, Instant::now() + std::time::Duration::from_secs(6), true));
    }
}

/// Handle one SDL event. Returns true when the app should quit.
pub fn handle_event(ui: &mut Ui, eng: &mut Engine, ev: &Event) -> bool {
    match ev {
        Event::Quit { .. } => return true,
        Event::KeyDown {
            keycode: Some(k),
            keymod,
            ..
        } => {
            // Sample browser overlay eats its own keys.
            if ui.browser.is_some() {
                match *k {
                    Keycode::Escape | Keycode::B => ui.browser = None,
                    Keycode::Up => browser_move(ui, -1),
                    Keycode::Down => browser_move(ui, 1),
                    Keycode::Return | Keycode::KpEnter => browser_load(ui, eng),
                    _ => {}
                }
                return false;
            }
            // Vim-style command line eats every key while open: typing
            // fills it, Return runs it, Esc cancels (never quits).
            if ui.cmdline.is_some() {
                match *k {
                    Keycode::Return | Keycode::KpEnter => {
                        let input = ui.cmdline.take().unwrap();
                        return run_command(ui, eng, &input);
                    }
                    Keycode::Escape => {
                        ui.cmdline = None;
                    }
                    Keycode::Backspace => {
                        if let Some(buf) = &mut ui.cmdline {
                            buf.pop();
                        }
                    }
                    _ => {
                        if let Some(c) = cmd_char(*k) {
                            if let Some(buf) = &mut ui.cmdline {
                                if buf.len() < 80 {
                                    buf.push(c);
                                }
                            }
                        }
                    }
                }
                return false;
            }
            // ':' opens the command line on any page.
            if *k == Keycode::Colon {
                ui.cmdline = Some(String::new());
                return false;
            }
            // Sample editor page keys (Tab/Up/Down handled here, the rest
            // falls through to the global keys below).
            if ui.page == Page::Edit {
                let i = ui.selected_pad;
                let shifted = keymod.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD);
                let handled = match *k {
                    Keycode::Tab => {
                        ui.page = Page::Pads;
                        true
                    }
                    Keycode::Z => {
                        if shifted {
                            eng.nudge_loop(i, 0, -0.01);
                        } else {
                            eng.nudge_trim(i, 0, -0.01);
                        }
                        true
                    }
                    Keycode::X => {
                        if shifted {
                            eng.nudge_loop(i, 0, 0.01);
                        } else {
                            eng.nudge_trim(i, 0, 0.01);
                        }
                        true
                    }
                    Keycode::C => {
                        if shifted {
                            eng.nudge_loop(i, 1, -0.01);
                        } else {
                            eng.nudge_trim(i, 1, -0.01);
                        }
                        true
                    }
                    Keycode::V => {
                        if shifted {
                            eng.nudge_loop(i, 1, 0.01);
                        } else {
                            eng.nudge_trim(i, 1, 0.01);
                        }
                        true
                    }
                    Keycode::L => {
                        let on = eng.toggle_loop(i);
                        ui.say(format!(
                            "PAD {} LOOP {}",
                            i + 1,
                            if on { "ON" } else { "OFF" }
                        ));
                        true
                    }
                    Keycode::R => {
                        eng.toggle_reverse(i);
                        true
                    }
                    Keycode::N => {
                        eng.normalize_pad(i);
                        ui.say("NORMALIZED");
                        true
                    }
                    Keycode::B => {
                        open_browser(ui);
                        true
                    }
                    Keycode::G => {
                        let g = (eng.pads[i].choke + 1) % (N_CHOKE_GROUPS + 1);
                        eng.set_choke(i, g);
                        true
                    }
                    Keycode::F => {
                        let kind = eng.pads[i].filter_kind.cycle();
                        let cutoff = eng.pads[i].filter_cutoff;
                        eng.set_filter(i, kind, cutoff);
                        true
                    }
                    Keycode::M => {
                        let mono = !eng.pads[i].mono;
                        eng.set_mono(i, mono);
                        ui.say(format!(
                            "PAD {} {}",
                            i + 1,
                            if mono { "MONO" } else { "POLY" }
                        ));
                        true
                    }
                    Keycode::LeftBracket => {
                        eng.nudge_pitch(i, -1.0);
                        true
                    }
                    Keycode::RightBracket => {
                        eng.nudge_pitch(i, 1.0);
                        true
                    }
                    Keycode::Minus => {
                        eng.nudge_res(i, false);
                        true
                    }
                    Keycode::Equals => {
                        eng.nudge_res(i, true);
                        true
                    }
                    Keycode::Up => {
                        eng.nudge_cutoff(i, true);
                        true
                    }
                    Keycode::Down => {
                        eng.nudge_cutoff(i, false);
                        true
                    }
                    _ => false,
                };
                if handled {
                    return false;
                }
            }
            // 4-track tape page keys (Tab/Esc handled here too).
            if ui.page == Page::Tape {
                // R / U / B need the whole engine (pads + four-track),
                // so they run before the four-track borrow below.
                match *k {
                    Keycode::R => {
                        tape_record(ui, eng, false);
                        return false;
                    }
                    Keycode::U => {
                        tape_record(ui, eng, true);
                        return false;
                    }
                    Keycode::B => {
                        let t = eng.four_track.selected;
                        match eng.resample_track_to_pad(t, t) {
                            Ok(m) => ui.say(m),
                            Err(e) => ui.say_err(e),
                        }
                        return false;
                    }
                    _ => {}
                }
                let ft = &mut eng.four_track;
                // Trim step: 0.1 s of tape.
                let step = (SP_RATE as f32 * 0.1) as isize;
                let handled = match *k {
                    Keycode::Tab | Keycode::Escape => {
                        ui.page = Page::Pads;
                        true
                    }
                    Keycode::Num1 => {
                        ft.select(0);
                        true
                    }
                    Keycode::Num2 => {
                        ft.select(1);
                        true
                    }
                    Keycode::Num3 => {
                        ft.select(2);
                        true
                    }
                    Keycode::Num4 => {
                        ft.select(3);
                        true
                    }
                    Keycode::A => {
                        let m = ft.toggle_arm();
                        ui.say(m);
                        true
                    }
                    Keycode::I => {
                        let m = ft.toggle_source();
                        ui.say(m);
                        true
                    }
                    Keycode::M => {
                        let m = ft.toggle_mute();
                        ui.say(m);
                        true
                    }
                    Keycode::X => {
                        let m = ft.clear_selected();
                        ui.say(m);
                        true
                    }
                    Keycode::Return | Keycode::KpEnter => {
                        let m = ft.toggle_play();
                        ui.say(m);
                        true
                    }
                    Keycode::Comma => {
                        let n = ft.selected + 1;
                        ft.tracks[ft.selected].nudge_trim_start(-step);
                        let l = ft.tracks[ft.selected].trim_label();
                        ui.say(format!("TRACK {n} TRIM {l}"));
                        true
                    }
                    Keycode::Period => {
                        let n = ft.selected + 1;
                        ft.tracks[ft.selected].nudge_trim_start(step);
                        let l = ft.tracks[ft.selected].trim_label();
                        ui.say(format!("TRACK {n} TRIM {l}"));
                        true
                    }
                    Keycode::Semicolon => {
                        let n = ft.selected + 1;
                        ft.tracks[ft.selected].nudge_trim_end(-step);
                        let l = ft.tracks[ft.selected].trim_label();
                        ui.say(format!("TRACK {n} TRIM {l}"));
                        true
                    }
                    Keycode::Quote => {
                        let n = ft.selected + 1;
                        ft.tracks[ft.selected].nudge_trim_end(step);
                        let l = ft.tracks[ft.selected].trim_label();
                        ui.say(format!("TRACK {n} TRIM {l}"));
                        true
                    }
                    Keycode::S => {
                        let m = ft.stop_rewind();
                        ui.say(m);
                        true
                    }
                    Keycode::L => {
                        let m = ft.toggle_loop();
                        ui.say(m);
                        true
                    }
                    _ => false,
                };
                if handled {
                    return false;
                }
            } else if *k == Keycode::Tab {
                ui.page = Page::Edit;
                return false;
            }
            match *k {
                Keycode::Escape => return true,
                Keycode::A => {
                    ui.page = Page::Tape;
                }
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
                Keycode::S => match eng.splice_onset() {
                    Some(n) => ui.say(format!("SPLICED: {n} CHOPS")),
                    None => ui.say_err("NO TAPE (DROP AUDIO)"),
                },
                Keycode::E => match eng.splice_equal() {
                    Some(n) => ui.say(format!("SLICED: {n} EQUAL PARTS")),
                    None => ui.say_err("NO TAPE (DROP AUDIO)"),
                },
                Keycode::D => {
                    eng.clear_tape();
                    ui.say("TAPE CLEARED");
                }
                Keycode::P => {
                    eng.panic();
                    ui.say("PANIC - ALL VOICES KILLED");
                }
                Keycode::K => {
                    let i = ui.selected_pad;
                    let locked = !eng.pads[i].locked;
                    eng.set_locked(i, locked);
                    ui.say(format!(
                        "PAD {} {}",
                        i + 1,
                        if locked { "LOCKED" } else { "UNLOCKED" }
                    ));
                }
                Keycode::W => {
                    save_project(ui, eng);
                }
                _ => {}
            }
        }
        Event::DropFile { filename, .. } => {
            drop_path(ui, eng, filename);
        }
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

/// Scan a directory for audio files (.wav / .mp3, case-insensitive),
/// sorted by name.
fn scan_audio(dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension()
                .and_then(|x| x.to_str())
                .map(|x| x.eq_ignore_ascii_case("wav") || x.eq_ignore_ascii_case("mp3"))
                .unwrap_or(false)
            {
                out.push(p.to_string_lossy().to_string());
            }
        }
    }
    out.sort();
    out
}

fn open_browser(ui: &mut Ui) {
    let files = scan_audio(&ui.samples_dir);
    if files.is_empty() {
        ui.say_err(format!("NO AUDIO IN {}", ui.samples_dir.to_uppercase()));
        return;
    }
    ui.browser = Some(Browser { files, sel: 0 });
}

fn browser_move(ui: &mut Ui, dir: i32) {
    if let Some(b) = &mut ui.browser {
        let n = b.files.len() as i32;
        b.sel = (b.sel as i32 + dir).rem_euclid(n) as usize;
    }
}

fn browser_load(ui: &mut Ui, eng: &mut Engine) {
    let file = match &ui.browser {
        Some(b) => match b.files.get(b.sel) {
            Some(f) => f.clone(),
            None => {
                ui.say_err("NO FILES");
                return;
            }
        },
        None => return,
    };
    let pad = ui.selected_pad;
    match eng.load_pad_sample(pad, &file) {
        Ok(m) => {
            ui.browser = None;
            ui.say(m);
        }
        Err(e) => ui.say_err(e),
    }
}

fn save_project(ui: &mut Ui, eng: &mut Engine) {
    let path = ui
        .project_path
        .clone()
        .unwrap_or_else(|| "./sp1200-project.json".to_string());
    let stem = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("sp1200-project.json");
    let name = stem.rsplit_once('.').map(|(s, _)| s).unwrap_or(stem);
    match crate::project::save_native_file(eng, &path, name) {
        Ok(()) => {
            ui.project_path = Some(path.clone());
            ui.say(format!("SAVED {path}"));
        }
        Err(e) => ui.say_err(e),
    }
}

/// Tape record, normal (`R`: armed tracks only) or universal (`U`:
/// every track). Appends the no-input-device warning when an INPUT
/// track is armed but no capture device exists.
fn tape_record(ui: &mut Ui, eng: &mut Engine, all: bool) {
    let m = if all {
        eng.four_track.toggle_record_all()
    } else {
        match eng.four_track.toggle_record() {
            Ok(m) => m,
            Err(e) => {
                ui.say_err(e);
                return;
            }
        }
    };
    if !eng.has_input()
        && eng
            .four_track
            .tracks
            .iter()
            .any(|t| t.armed && t.source == TrackSource::Input)
    {
        ui.say(format!("{m} (NO INPUT DEVICE)"));
    } else {
        ui.say(m);
    }
}

/// Map a key press to a command-line character. Letters come through
/// lowercased (commands are case-insensitive); only the punctuation
/// commands use is accepted, so shifted symbols can't sneak in.
fn cmd_char(k: Keycode) -> Option<char> {
    Some(match k {
        Keycode::A => 'a',
        Keycode::B => 'b',
        Keycode::C => 'c',
        Keycode::D => 'd',
        Keycode::E => 'e',
        Keycode::F => 'f',
        Keycode::G => 'g',
        Keycode::H => 'h',
        Keycode::I => 'i',
        Keycode::J => 'j',
        Keycode::K => 'k',
        Keycode::L => 'l',
        Keycode::M => 'm',
        Keycode::N => 'n',
        Keycode::O => 'o',
        Keycode::P => 'p',
        Keycode::Q => 'q',
        Keycode::R => 'r',
        Keycode::S => 's',
        Keycode::T => 't',
        Keycode::U => 'u',
        Keycode::V => 'v',
        Keycode::W => 'w',
        Keycode::X => 'x',
        Keycode::Y => 'y',
        Keycode::Z => 'z',
        Keycode::Num0 => '0',
        Keycode::Num1 => '1',
        Keycode::Num2 => '2',
        Keycode::Num3 => '3',
        Keycode::Num4 => '4',
        Keycode::Num5 => '5',
        Keycode::Num6 => '6',
        Keycode::Num7 => '7',
        Keycode::Num8 => '8',
        Keycode::Num9 => '9',
        Keycode::Space => ' ',
        Keycode::Minus => '-',
        Keycode::Period => '.',
        _ => return None,
    })
}

/// Next whitespace-separated command argument, or "" when absent.
fn next_arg<'a>(parts: &mut std::str::SplitWhitespace<'a>) -> &'a str {
    parts.next().unwrap_or("")
}

/// Run one `:command` line. Returns true when the app should quit.
/// Times are seconds; pad/track numbers are 1-based, like the UI.
fn run_command(ui: &mut Ui, eng: &mut Engine, input: &str) -> bool {
    let mut parts = input.split_whitespace();
    let cmd = match parts.next() {
        Some(c) => c.to_ascii_lowercase(),
        None => return false,
    };
    let num = |s: &str| s.parse::<f64>().ok();
    match cmd.as_str() {
        "help" => ui.say("W SAVE Q QUIT BPM SWING PAD TRACK LOOP TTRIM RESAMPLE REC STOP PLAY - SEE README"),
        "w" => save_project(ui, eng),
        "q" => return true,
        "bpm" => match num(next_arg(&mut parts)) {
            Some(n) => {
                eng.bpm = n.clamp(60.0, 200.0);
                ui.say(format!("BPM {:03.0}", eng.bpm));
            }
            None => ui.say_err("USAGE: :BPM 120"),
        },
        "swing" => match num(next_arg(&mut parts)) {
            Some(n) => {
                eng.swing = n.clamp(50.0, 75.0);
                ui.say(format!("SWING {:02.0}%", eng.swing));
            }
            None => ui.say_err("USAGE: :SWING 62"),
        },
        "pad" => match next_arg(&mut parts).parse::<usize>() {
            Ok(n) if (1..=8).contains(&n) => {
                ui.selected_pad = n - 1;
                ui.say(format!("PAD {n}"));
            }
            _ => ui.say_err("USAGE: :PAD 1-8"),
        },
        "track" => match next_arg(&mut parts).parse::<usize>() {
            Ok(n) if (1..=4).contains(&n) => {
                eng.four_track.select(n - 1);
                ui.say(format!("TRACK {n}"));
            }
            _ => ui.say_err("USAGE: :TRACK 1-4"),
        },
        "loop" => {
            let i = ui.selected_pad;
            match next_arg(&mut parts) {
                "" => {
                    let on = eng.toggle_loop(i);
                    ui.say(format!(
                        "PAD {} LOOP {}",
                        i + 1,
                        if on { "ON" } else { "OFF" }
                    ));
                }
                "on" => {
                    eng.pads[i].loop_enabled = true;
                    ui.say(format!("PAD {} LOOP ON", i + 1));
                }
                "off" => {
                    eng.pads[i].loop_enabled = false;
                    ui.say(format!("PAD {} LOOP OFF", i + 1));
                }
                s => match (num(s), num(next_arg(&mut parts))) {
                    (Some(a), Some(b)) => match eng.set_loop_secs(i, a as f32, b as f32) {
                        Ok(m) => ui.say(m),
                        Err(e) => ui.say_err(e),
                    },
                    _ => ui.say_err("USAGE: :LOOP ON/OFF OR :LOOP START_S END_S"),
                },
            }
        }
        "ttrim" => match next_arg(&mut parts) {
            "full" => {
                let t = eng.four_track.selected;
                eng.four_track.tracks[t].reset_trim();
                ui.say(format!("TRACK {} TRIM FULL", t + 1));
            }
            s => match (num(s), num(next_arg(&mut parts))) {
                (Some(a), Some(b)) => {
                    let t = eng.four_track.selected;
                    let tr = &mut eng.four_track.tracks[t];
                    if tr.samples.is_empty() {
                        ui.say_err("TRACK EMPTY");
                    } else {
                        let n = tr.samples.len();
                        let sa = ((a * SP_RATE as f64) as usize).min(n);
                        let eb = ((b * SP_RATE as f64) as usize).min(n);
                        if eb <= sa {
                            ui.say_err("USAGE: :TTRIM NEEDS START < END");
                        } else {
                            tr.set_trim(sa, Some(eb));
                            let l = tr.trim_label();
                            ui.say(format!("TRACK {} TRIM {l}", t + 1));
                        }
                    }
                }
                _ => ui.say_err("USAGE: :TTRIM START_S END_S OR :TTRIM FULL"),
            },
        },
        "resample" => {
            let sel = eng.four_track.selected;
            let a1 = next_arg(&mut parts);
            let a2 = next_arg(&mut parts);
            let parsed = if a1.is_empty() {
                Some((sel, sel))
            } else if a2.is_empty() {
                a1.parse::<usize>().ok().and_then(|n| {
                    if (1..=8).contains(&n) {
                        Some((sel, n - 1))
                    } else {
                        None
                    }
                })
            } else {
                match (a1.parse::<usize>(), a2.parse::<usize>()) {
                    (Ok(t), Ok(p)) if (1..=4).contains(&t) && (1..=8).contains(&p) => {
                        Some((t - 1, p - 1))
                    }
                    _ => None,
                }
            };
            match parsed {
                Some((t, p)) => match eng.resample_track_to_pad(t, p) {
                    Ok(m) => ui.say(m),
                    Err(e) => ui.say_err(e),
                },
                None => ui.say_err("USAGE: :RESAMPLE [TRACK] [PAD]"),
            }
        }
        "rec" => match next_arg(&mut parts) {
            "all" => tape_record(ui, eng, true),
            "" => tape_record(ui, eng, false),
            _ => ui.say_err("USAGE: :REC OR :REC ALL"),
        },
        "stop" => {
            let ft = &mut eng.four_track;
            ft.playing = false;
            ft.recording = false;
            ui.say("TAPE STOP");
        }
        "rewind" => {
            let m = eng.four_track.stop_rewind();
            ui.say(m);
        }
        "play" => {
            let m = eng.four_track.toggle_play();
            ui.say(m);
        }
        _ => ui.say_err("UNKNOWN COMMAND - :HELP"),
    }
    false
}

fn drop_path(ui: &mut Ui, eng: &mut Engine, path: &str) {
    let lower = path.to_lowercase();
    if lower.ends_with(".wav") || lower.ends_with(".mp3") {
        match crate::engine::load_tape_file(eng, path) {
            Ok(m) => ui.say(m),
            Err(e) => ui.say_err(e),
        }
    } else if lower.ends_with(".json") {
        match crate::project::load_project_file(eng, path) {
            Ok(m) => {
                ui.project_path = Some(path.to_string());
                ui.say(m);
            }
            Err(e) => ui.say_err(e),
        }
    } else {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        ui.say_err(format!("can't open {name} (drop .wav/.mp3 or .json)"));
    }
}

fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    (
        (a.0 as f32 + (b.0 as f32 - a.0 as f32) * t) as u8,
        (a.1 as f32 + (b.1 as f32 - a.1 as f32) * t) as u8,
        (a.2 as f32 + (b.2 as f32 - a.2 as f32) * t) as u8,
    )
}

pub fn draw(p: &mut dyn Painter, ui: &Ui, eng: &Engine) {
    if ui.page == Page::Edit {
        draw_edit(p, ui, eng);
        return;
    }
    if ui.page == Page::Tape {
        draw_tape(p, ui, eng);
        return;
    }
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
    if let Some(ts) = eng.tape_status() {
        p.text(110, 84, &ts, 1, SEL);
    }

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
        let label = eng.pad_label(i);
        let tw = text_w(&label, 2) as i32;
        p.text(x + (w as i32 - tw) / 2, y + 34, &label, 2, TEXT);
        // choke + filter readout under the label
        let mut sub = String::new();
        if eng.pads[i].mono {
            sub.push_str("M ");
        }
        if eng.pads[i].loop_enabled {
            sub.push_str("LOOP ");
        }
        if eng.pads[i].choke != 0 {
            sub.push_str(&format!("G{} ", eng.pads[i].choke));
        }
        if eng.pads[i].filter_kind != crate::dsp::FilterKind::Off {
            sub.push_str(&format!(
                "{}{:04.0}",
                eng.pads[i].filter_kind.abbrev(),
                eng.pads[i].filter_cutoff
            ));
        }
        if !sub.is_empty() {
            let tw2 = text_w(&sub, 1) as i32;
            p.text(x + (w as i32 - tw2) / 2, y + 60, &sub, 1, DIM);
        }
        let key = format!("{}", i + 1);
        p.text(x + 10, y + 10, &key, 1, DIM);
        if eng.pads[i].locked {
            p.rect(x + w as i32 - 18, y + 8, 10, 10, TEXT);
        }
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
    let sel_name = eng.pad_label(ui.selected_pad);
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

    draw_msg(p, ui);

    // footer help
    p.rect(0, 566, W, H - 566, PLATE);
    p.text(
        24,
        584,
        "SPACE PLAY/STOP  T TAP  TAB EDIT  A 4-TRACK  P PANIC  K LOCK  S/E SPLICE  D TAPE OFF  W SAVE  : CMDS  ESC QUIT",
        1,
        DIM,
    );
    p.text(
        24,
        606,
        "1-8 PADS  F1-F3 PRESETS  CLICK PADS/STEPS  DROP .WAV/.MP3 TAPE - DROP .JSON PROJECT",
        1,
        DIM,
    );
}

fn draw_tape(p: &mut dyn Painter, ui: &Ui, eng: &Engine) {
    p.rect(0, 0, W, H, BG);
    p.rect(0, 0, W, 76, PLATE);
    p.text(24, 22, "SP-1200 RS", 3, TEXT);
    p.text(24, 52, "4-TRACK TAPE", 1, DIM);

    let ft = &eng.four_track;
    // Transport status, top-right.
    let st = ft.status();
    let stc = if ft.recording { REC } else { TEXT };
    let secs = ft.playhead as f32 / SP_RATE as f32;
    let time = format!("{:02}:{:04.1}", (secs / 60.0) as u32, secs % 60.0);
    let in_s = if eng.has_input() { "IN OK" } else { "IN NONE" };
    let mut rx = W as i32 - 24;
    for (s, c) in [(time.as_str(), TEXT), (in_s, DIM), (st, stc)] {
        let w = text_w(s, 2) as i32;
        rx -= w;
        p.text(rx, 26, s, 2, c);
        rx -= 24;
    }
    if ft.loop_enabled {
        let ls = if ft.loop_len > 0 {
            format!("LOOP {:.1}S", ft.loop_len_secs())
        } else {
            "LOOP".to_string()
        };
        let w = text_w(&ls, 2) as i32;
        rx -= w;
        p.text(rx, 26, &ls, 2, LOOPC);
    }

    // Track strips.
    for t in 0..N_TRACKS {
        let tr = &ft.tracks[t];
        let y0 = 100 + t as i32 * 112;
        if t == ft.selected {
            p.rect(16, y0 - 8, W - 32, 104, PLATE);
        }
        // REC dot for armed tracks.
        if tr.armed {
            let c = if ft.recording { REC } else { DIM };
            p.rect(24, y0 + 4, 10, 10, c);
        }
        p.text(
            44,
            y0,
            &format!("TRACK {}", t + 1),
            2,
            if t == ft.selected { TEXT } else { DIM },
        );
        let mut sy = y0 + 30;
        p.text(
            44,
            sy,
            &format!(
                "{}  {}  {}",
                if tr.armed { "ARMED" } else { "SAFE" },
                tr.source.label(),
                if tr.muted { "MUTED" } else { "" },
            ),
            1,
            DIM,
        );
        sy += 22;
        p.text(44, sy, &format!("{:.1}S", tr.len_secs()), 1, DIM);
        sy += 22;
        p.text(44, sy, &format!("TRIM {}", tr.trim_label()), 1, DIM);

        // Waveform overview.
        let (wx, wy, ww, wh) = (300i32, y0, 700u32, 88u32);
        p.rect(wx, wy, ww, wh, BG);
        let mid = wy + wh as i32 / 2;
        let amp = wh as f32 / 2.0 - 2.0;
        if tr.samples.is_empty() {
            p.rect(wx, mid, ww, 1, DIM);
        } else {
            let n = tr.samples.len();
            for cx in 0..ww {
                let a = (cx as usize * n) / ww as usize;
                let b = (((cx as usize + 1) * n) / ww as usize).max(a + 1).min(n);
                let step = ((b - a) / 64).max(1);
                let mut mn = f32::INFINITY;
                let mut mx = f32::NEG_INFINITY;
                for s in tr.samples[a..b].iter().step_by(step) {
                    if *s < mn {
                        mn = *s;
                    }
                    if *s > mx {
                        mx = *s;
                    }
                }
                let y1 = mid - (mx * amp) as i32;
                let y2 = mid - (mn * amp) as i32;
                let (y1, y2) = (y1.min(y2), y1.max(y2));
                p.rect(wx + cx as i32, y1, 1, (y2 - y1).max(1) as u32, DIM);
            }
        }
        // Playhead marker.
        if !tr.samples.is_empty() {
            let px = wx
                + ((ft.playhead.min(tr.samples.len()) as u64 * ww as u64) / tr.samples.len() as u64)
                    as i32;
            p.rect(px, wy, 2, wh, PLAYHEAD);
        }
    }

    p.text(
        24,
        580,
        "1-4 SELECT  A ARM  I INPUT/MASTER  M MUTE  X CLEAR TRACK  U REC ALL",
        1,
        DIM,
    );
    p.text(
        24,
        602,
        "ENTER PLAY/STOP  R RECORD  S REWIND  L LOOP  ,/. ;/' TRIM  B TO PAD  : CMDS  TAB PADS",
        1,
        DIM,
    );
    draw_msg(p, ui);
}

fn draw_msg(p: &mut dyn Painter, ui: &Ui) {
    // The command line replaces the message row while it is open.
    if let Some(buf) = &ui.cmdline {
        let line = format!(":{buf}");
        p.text(24, 550, &line, 1, SEL);
        let cx = 24 + text_w(&line, 1) as i32;
        p.rect(cx, 550, 6, 10, SEL);
        return;
    }
    if let Some((text, until, is_err)) = &ui.msg {
        if Instant::now() <= *until {
            p.text(24, 550, text, 1, if *is_err { PLAYHEAD } else { TEXT });
        }
    }
}

/// Sample editor page: waveform of the selected pad's source with the trim
/// region, plus choke / filter / reverse / normalize controls.
fn draw_edit(p: &mut dyn Painter, ui: &Ui, eng: &Engine) {
    p.rect(0, 0, W, H, BG);
    p.rect(0, 0, W, 76, PLATE);
    p.text(24, 22, "SP-1200 RS", 3, TEXT);
    p.text(24, 52, "SAMPLE EDITOR", 1, DIM);

    let i = ui.selected_pad;
    let pad = &eng.pads[i];
    p.text(
        24,
        88,
        &format!("PAD {} {}", i + 1, eng.pad_label(i)),
        2,
        SEL,
    );

    // waveform
    let (wx, wy, ww, wh) = (24i32, 124i32, 976u32, 190u32);
    p.rect(wx, wy, ww, wh, PLATE);
    let src = &pad.source;
    if src.is_empty() {
        p.text(wx + 20, wy + 88, "NO SAMPLE", 2, DIM);
    } else {
        let n = src.len();
        let mid = wy + wh as i32 / 2;
        p.rect(wx, mid, ww, 1, DIM);
        let per = n as f64 / ww as f64;
        let amp = wh as f32 / 2.0 - 6.0;
        for px in 0..ww {
            let a = (px as f64 * per) as usize;
            let b = ((px as f64 + 1.0) * per) as usize + 1;
            let (mut mn, mut mx) = (i16::MAX, i16::MIN);
            for s in &src[a.min(n)..b.min(n)] {
                mn = mn.min(*s);
                mx = mx.max(*s);
            }
            let y1 = mid - (mx as f32 / 2047.0 * amp) as i32;
            let y2 = mid - (mn as f32 / 2047.0 * amp) as i32;
            let inside = px as f32 / ww as f32 >= pad.trim.0 && px as f32 / ww as f32 <= pad.trim.1;
            p.rect(
                wx + px as i32,
                y1.min(y2),
                1,
                (y2 - y1).abs() as u32 + 1,
                if inside { TEXT } else { DIM },
            );
        }
        let mx0 = wx + (pad.trim.0 * ww as f32) as i32;
        let mx1 = wx + (pad.trim.1 * ww as f32) as i32;
        p.rect(mx0, wy, 2, wh, SEL);
        p.rect(mx1, wy, 2, wh, SEL);
    }

    // params
    let y0 = 336;
    let left = [
        format!("SOURCE {} SAMPLES", src.len()),
        format!(
            "TRIM {:02.0}% - {:02.0}%",
            pad.trim.0 * 100.0,
            pad.trim.1 * 100.0
        ),
        if pad.loop_enabled {
            format!(
                "LOOP ON {:02.0}% - {:02.0}%",
                pad.loop_pts.0 * 100.0,
                pad.loop_pts.1 * 100.0
            )
        } else {
            "LOOP OFF".to_string()
        },
        format!("REVERSE {}", if pad.reverse { "ON" } else { "OFF" }),
        format!(
            "CHOKE {}",
            if pad.choke == 0 {
                "--".to_string()
            } else {
                format!("G{}", pad.choke)
            }
        ),
        format!("LOCK {}", if pad.locked { "ON" } else { "OFF" }),
        format!("MODE {}", if pad.mono { "MONO" } else { "POLY" }),
        format!("PITCH {:+.1} ST", pad.tune_st),
    ];
    for (r, s) in left.iter().enumerate() {
        p.text(24, y0 + r as i32 * 24, s, 1, TEXT);
    }
    let right = [
        format!("FILTER {}", pad.filter_kind.abbrev()),
        format!("CUTOFF {:04.0} HZ", pad.filter_cutoff),
        format!("RES Q{:.2}", pad.filter_res),
    ];
    for (r, s) in right.iter().enumerate() {
        p.text(560, y0 + r as i32 * 24, s, 1, TEXT);
    }

    // sample browser overlay
    if let Some(b) = &ui.browser {
        let (bw, bh) = (460i32, 320u32);
        let (bx, by) = ((W as i32 - bw) / 2, 160);
        p.rect(bx, by, bw as u32, bh, PLATE);
        p.rect(bx, by, bw as u32, 3, SEL);
        p.rect(bx, by + bh as i32 - 3, bw as u32, 3, SEL);
        p.text(bx + 16, by + 12, "LOAD SAMPLE - ENTER", 1, SEL);
        let show = 10usize;
        let start = if b.files.len() > show {
            b.sel.saturating_sub(show / 2).min(b.files.len() - show)
        } else {
            0
        };
        for (r, f) in b.files.iter().skip(start).take(show).enumerate() {
            let name = f.rsplit(['/', '\\']).next().unwrap_or(f);
            let disp: String = name.chars().take(36).collect();
            let c = if start + r == b.sel { SEL } else { DIM };
            p.text(bx + 16, by + 44 + r as i32 * 24, &disp.to_uppercase(), 1, c);
        }
    }

    draw_msg(p, ui);

    // footer help
    p.rect(0, 566, W, H - 566, PLATE);
    p.text(
        24,
        584,
        "TAB PADS  Z/X TRIM START  C/V TRIM END  R REVERSE  N NORMALIZE  L LOOP  SHIFT+TRIM LOOP PTS",
        1,
        DIM,
    );
    p.text(
        24,
        606,
        "B BROWSE  G CHOKE  F FILTER  M MONO  K LOCK  UP/DN CUTOFF  -/= RES  [/] PITCH  1-8 PLAY",
        1,
        DIM,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdl2::pixels::PixelFormatEnum;

    #[test]
    fn font_covers_needed_chars() {
        for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 -.:/%+<>()!?#=[]".chars() {
            assert!(glyph(c).is_some(), "missing glyph for {c:?}");
        }
    }

    #[test]
    fn text_width_math() {
        assert_eq!(text_w("AB", 1), 12);
        assert_eq!(text_w("AB", 2), 24);
    }

    #[test]
    fn w_key_saves_project_to_chosen_path() {
        let dir = std::env::temp_dir().join(format!("sp1200rs-w-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mine.json");
        let mut eng = Engine::new();
        eng.bpm = 133.0;
        let mut ui = Ui::new();
        ui.project_path = Some(path.to_str().unwrap().to_string());
        let ev = Event::KeyDown {
            timestamp: 0,
            window_id: 0,
            keycode: Some(Keycode::W),
            scancode: None,
            keymod: sdl2::keyboard::Mod::NOMOD,
            repeat: false,
        };
        assert!(!handle_event(&mut ui, &mut eng, &ev));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"bpm\": 133.0"), "{text}");
        assert!(text.contains("\"format\": \"sp1200-rs\""), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn edit_page_draw_no_panic() {
        let mut eng = Engine::new();
        // Give pad 0 a real sample with edits + filter + choke.
        eng.pads[0].set_source(
            (0..5000)
                .map(|i| ((i as f32 * 0.7).sin() * 1800.0) as i16)
                .collect(),
            None,
        );
        eng.nudge_trim(0, 0, 0.2);
        eng.toggle_reverse(0);
        eng.set_choke(0, 2);
        eng.set_filter(0, crate::dsp::FilterKind::Lowpass, 1200.0);
        let mut ui = Ui::new();
        ui.page = Page::Edit;
        ui.selected_pad = 0;
        let mut surf = Surface::new(W, H, PixelFormatEnum::RGB24).unwrap();
        {
            let p: &mut dyn Painter = &mut surf;
            draw(p, &ui, &eng);
        }
        // And with an empty source.
        eng.pads[1].set_source(Vec::new(), None);
        ui.selected_pad = 1;
        {
            let p: &mut dyn Painter = &mut surf;
            draw(p, &ui, &eng);
        }
    }

    #[test]
    fn browser_open_navigate_load() {
        let dir = std::env::temp_dir().join(format!("sp1200rs-browse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["b.wav", "a.wav", "c.WAV", "d.mp3", "note.txt"] {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(b"RIFF");
            bytes.extend_from_slice(&44u32.to_le_bytes());
            bytes.extend_from_slice(b"WAVEfmt ");
            bytes.extend_from_slice(&16u32.to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes());
            bytes.extend_from_slice(&44100u32.to_le_bytes());
            bytes.extend_from_slice(&88200u32.to_le_bytes());
            bytes.extend_from_slice(&2u16.to_le_bytes());
            bytes.extend_from_slice(&16u16.to_le_bytes());
            bytes.extend_from_slice(b"data");
            bytes.extend_from_slice(&4u32.to_le_bytes());
            bytes.extend_from_slice(&[1u8, 2, 3, 4]);
            std::fs::write(dir.join(name), &bytes).unwrap();
        }
        let mut eng = Engine::new();
        let mut ui = Ui::new();
        ui.samples_dir = dir.to_str().unwrap().to_string();
        ui.page = Page::Edit;
        open_browser(&mut ui);
        let b = ui.browser.as_ref().unwrap();
        // sorted, .wav + .mp3 (case-insensitive), note.txt excluded
        assert_eq!(b.files.len(), 4);
        assert!(b.files[0].ends_with("a.wav"));
        assert!(b.files[3].ends_with("d.mp3"));
        // navigate + draw with overlay
        browser_move(&mut ui, 1);
        assert_eq!(ui.browser.as_ref().unwrap().sel, 1);
        let mut surf = Surface::new(W, H, PixelFormatEnum::RGB24).unwrap();
        {
            let p: &mut dyn Painter = &mut surf;
            draw(p, &ui, &eng);
        }
        // wrap-around
        browser_move(&mut ui, -1);
        browser_move(&mut ui, -1);
        assert_eq!(ui.browser.as_ref().unwrap().sel, 3);
        // wrap forward onto a.wav (a real WAV) and load into pad 3
        browser_move(&mut ui, 1);
        assert_eq!(ui.browser.as_ref().unwrap().sel, 0);
        ui.selected_pad = 2;
        browser_load(&mut ui, &mut eng);
        assert!(ui.browser.is_none());
        assert!(eng.pads[2].custom);
        assert!(!eng.pads[2].source.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn four_track_page_keys() {
        let mut eng = Engine::new();
        let mut ui = Ui::new();
        let key = |kc| Event::KeyDown {
            timestamp: 0,
            window_id: 0,
            keycode: Some(kc),
            scancode: None,
            keymod: sdl2::keyboard::Mod::NOMOD,
            repeat: false,
        };
        // A opens the 4-track page from the pads page.
        handle_event(&mut ui, &mut eng, &key(Keycode::A));
        assert!(matches!(ui.page, Page::Tape));
        // 1-4 select tracks; A/I/M arm/source/mute the selected one.
        handle_event(&mut ui, &mut eng, &key(Keycode::Num3));
        assert_eq!(eng.four_track.selected, 2);
        handle_event(&mut ui, &mut eng, &key(Keycode::A));
        assert!(eng.four_track.tracks[2].armed);
        handle_event(&mut ui, &mut eng, &key(Keycode::I));
        assert_eq!(
            eng.four_track.tracks[2].source,
            crate::fourtrack::TrackSource::Input
        );
        handle_event(&mut ui, &mut eng, &key(Keycode::M));
        assert!(eng.four_track.tracks[2].muted);
        // R without an armed MASTER... track 3 is armed (INPUT): records.
        handle_event(&mut ui, &mut eng, &key(Keycode::R));
        assert!(eng.four_track.recording);
        assert!(eng.four_track.playing);
        handle_event(&mut ui, &mut eng, &key(Keycode::R));
        assert!(!eng.four_track.recording);
        // Enter toggles play; S rewinds.
        handle_event(&mut ui, &mut eng, &key(Keycode::Return));
        assert!(!eng.four_track.playing);
        handle_event(&mut ui, &mut eng, &key(Keycode::Return));
        assert!(eng.four_track.playing);
        handle_event(&mut ui, &mut eng, &key(Keycode::S));
        assert!(!eng.four_track.playing);
        assert_eq!(eng.four_track.playhead, 0);
        // L toggles the tape loop.
        eng.four_track.tracks[0].samples = vec![0.2; 300];
        handle_event(&mut ui, &mut eng, &key(Keycode::L));
        assert!(eng.four_track.loop_enabled);
        assert_eq!(eng.four_track.loop_len, 300);
        handle_event(&mut ui, &mut eng, &key(Keycode::L));
        assert!(!eng.four_track.loop_enabled);
        // X clears the selected track.
        eng.four_track.tracks[2].samples = vec![0.1; 10];
        handle_event(&mut ui, &mut eng, &key(Keycode::X));
        assert!(eng.four_track.tracks[2].samples.is_empty());
        // Tab goes back to pads; A on the tape page arms (doesn't reopen).
        handle_event(&mut ui, &mut eng, &key(Keycode::Tab));
        assert!(matches!(ui.page, Page::Pads));
        // Pads 1-4 still trigger pads on the pads page (no tape hijack).
        handle_event(&mut ui, &mut eng, &key(Keycode::Num1));
        assert_eq!(eng.active_voices(), 1);
        // Draw the tape page with content: no panic.
        eng.four_track.tracks[0].samples = vec![0.2; 5000];
        eng.four_track.tracks[1].samples = vec![-0.3; 200];
        ui.page = Page::Tape;
        let mut surf = Surface::new(W, H, PixelFormatEnum::RGB24).unwrap();
        {
            let p: &mut dyn Painter = &mut surf;
            draw(p, &ui, &eng);
        }
    }

    #[test]
    fn mono_pitch_res_keys() {
        let mut eng = Engine::new();
        let mut ui = Ui::new();
        ui.page = Page::Edit;
        let key = |kc| Event::KeyDown {
            timestamp: 0,
            window_id: 0,
            keycode: Some(kc),
            scancode: None,
            keymod: sdl2::keyboard::Mod::NOMOD,
            repeat: false,
        };
        // M toggles mono/poly.
        handle_event(&mut ui, &mut eng, &key(Keycode::M));
        assert!(eng.pads[0].mono);
        handle_event(&mut ui, &mut eng, &key(Keycode::M));
        assert!(!eng.pads[0].mono);
        // [/] pitch in semitones.
        handle_event(&mut ui, &mut eng, &key(Keycode::RightBracket));
        handle_event(&mut ui, &mut eng, &key(Keycode::RightBracket));
        assert!((eng.pads[0].tune_st - 2.0).abs() < 1e-6);
        handle_event(&mut ui, &mut eng, &key(Keycode::LeftBracket));
        assert!((eng.pads[0].tune_st - 1.0).abs() < 1e-6);
        // -/= resonance.
        let q0 = eng.pads[0].filter_res;
        handle_event(&mut ui, &mut eng, &key(Keycode::Equals));
        assert!(eng.pads[0].filter_res > q0);
        handle_event(&mut ui, &mut eng, &key(Keycode::Minus));
        assert!((eng.pads[0].filter_res - q0).abs() < 1e-6);
        // Draw the edit page with the new params: no panic.
        let mut surf = Surface::new(W, H, PixelFormatEnum::RGB24).unwrap();
        {
            let p: &mut dyn Painter = &mut surf;
            draw(p, &ui, &eng);
        }
    }

    #[test]
    fn panic_and_lock_keys() {
        let mut eng = Engine::new();
        let mut ui = Ui::new();
        let key = |kc| Event::KeyDown {
            timestamp: 0,
            window_id: 0,
            keycode: Some(kc),
            scancode: None,
            keymod: sdl2::keyboard::Mod::NOMOD,
            repeat: false,
        };
        // P kills voices on either page.
        eng.trigger(0);
        eng.trigger(3);
        assert!(eng.active_voices() > 0);
        handle_event(&mut ui, &mut eng, &key(Keycode::P));
        assert_eq!(eng.active_voices(), 0);
        // K toggles the lock on the selected pad.
        assert!(!eng.pads[0].locked);
        handle_event(&mut ui, &mut eng, &key(Keycode::K));
        assert!(eng.pads[0].locked);
        handle_event(&mut ui, &mut eng, &key(Keycode::K));
        assert!(!eng.pads[0].locked);
        // Same on the edit page.
        ui.page = Page::Edit;
        handle_event(&mut ui, &mut eng, &key(Keycode::K));
        assert!(eng.pads[0].locked);
        // Draw both pages with a locked pad: no panic.
        let mut surf = Surface::new(W, H, PixelFormatEnum::RGB24).unwrap();
        {
            let p: &mut dyn Painter = &mut surf;
            draw(p, &ui, &eng);
        }
        ui.page = Page::Pads;
        {
            let p: &mut dyn Painter = &mut surf;
            draw(p, &ui, &eng);
        }
    }

    #[test]
    fn edit_keys_drive_engine() {
        let mut eng = Engine::new();
        let mut ui = Ui::new();
        ui.page = Page::Edit;
        let key = |kc| Event::KeyDown {
            timestamp: 0,
            window_id: 0,
            keycode: Some(kc),
            scancode: None,
            keymod: sdl2::keyboard::Mod::NOMOD,
            repeat: false,
        };
        // G cycles choke 0 -> 1 -> 2
        handle_event(&mut ui, &mut eng, &key(Keycode::G));
        handle_event(&mut ui, &mut eng, &key(Keycode::G));
        assert_eq!(eng.pads[0].choke, 2);
        // F cycles filter off -> lp
        handle_event(&mut ui, &mut eng, &key(Keycode::F));
        assert_eq!(eng.pads[0].filter_kind, crate::dsp::FilterKind::Lowpass);
        // Up/Down move cutoff (not master) on the edit page
        let m0 = eng.master;
        let c0 = eng.pads[0].filter_cutoff;
        handle_event(&mut ui, &mut eng, &key(Keycode::Up));
        assert_eq!(eng.master, m0);
        assert!(eng.pads[0].filter_cutoff > c0);
        // R toggles reverse, Z trims
        handle_event(&mut ui, &mut eng, &key(Keycode::R));
        assert!(eng.pads[0].reverse);
        handle_event(&mut ui, &mut eng, &key(Keycode::X));
        assert!(eng.pads[0].trim.0 > 0.0);
        // Tab goes back to pads
        handle_event(&mut ui, &mut eng, &key(Keycode::Tab));
        assert_eq!(ui.page, Page::Pads);
        // and Tab from pads opens the editor
        handle_event(&mut ui, &mut eng, &key(Keycode::Tab));
        assert_eq!(ui.page, Page::Edit);
    }

    #[test]
    fn draw_tape_mode_no_panic() {
        let mut eng = Engine::new();
        // Synthetic tape: 6 onset-separated regions.
        let mut audio = Vec::new();
        for _ in 0..6 {
            audio.push(2047);
            audio.extend(std::iter::repeat(300i16).take(3000));
        }
        let n = eng.load_tape("TESTTAPE".to_string(), audio);
        assert!(n >= 6);
        eng.play();
        let mut buf = vec![0.0f32; 5000];
        eng.render(&mut buf);
        let mut ui = Ui::new();
        ui.say("TAPE TESTTAPE - 6 CHOPS");
        ui.say_err("SOME ERROR");
        let mut surf = Surface::new(W, H, PixelFormatEnum::RGB24).unwrap();
        {
            let p: &mut dyn Painter = &mut surf;
            draw(p, &ui, &eng);
        }
        // And with markers but no audio (fresh project load).
        eng.clear_tape();
        eng.tape = Some(crate::engine::Tape {
            name: "MARKERS".to_string(),
            audio: Vec::new(),
            chops: vec![(0, 100), (100, 200)],
        });
        eng.refresh_pad_samples();
        {
            let p: &mut dyn Painter = &mut surf;
            draw(p, &ui, &eng);
        }
    }

    #[test]
    fn colon_prompt_typing_escape_and_quit() {
        let mut eng = Engine::new();
        let mut ui = Ui::new();
        let key = |kc, m| Event::KeyDown {
            timestamp: 0,
            window_id: 0,
            keycode: Some(kc),
            scancode: None,
            keymod: m,
            repeat: false,
        };
        let nomod = sdl2::keyboard::Mod::NOMOD;
        // ':' opens; typing fills; Backspace pops; Esc cancels, not quits.
        assert!(!handle_event(&mut ui, &mut eng, &key(Keycode::Colon, nomod)));
        assert_eq!(ui.cmdline.as_deref(), Some(""));
        assert!(!handle_event(&mut ui, &mut eng, &key(Keycode::Q, nomod)));
        assert_eq!(ui.cmdline.as_deref(), Some("q"));
        assert!(!handle_event(&mut ui, &mut eng, &key(Keycode::Backspace, nomod)));
        assert_eq!(ui.cmdline.as_deref(), Some(""));
        assert!(!handle_event(&mut ui, &mut eng, &key(Keycode::Q, nomod)));
        assert!(!handle_event(&mut ui, &mut eng, &key(Keycode::Escape, nomod)));
        assert_eq!(ui.cmdline, None);
        // Reopen and Return on "q" quits.
        handle_event(&mut ui, &mut eng, &key(Keycode::Colon, nomod));
        handle_event(&mut ui, &mut eng, &key(Keycode::Q, nomod));
        assert!(handle_event(
            &mut ui,
            &mut eng,
            &key(Keycode::Return, nomod)
        ));
    }

    #[test]
    fn run_command_parses_everything() {
        let mut eng = Engine::new();
        let mut ui = Ui::new();
        eng.pads[2].set_source(vec![0i16; 26040], None); // pad 3: 1.0 s
        eng.four_track.tracks[0].samples = vec![0.5; 26040];
        eng.four_track.tracks[1].samples = vec![-0.25; 26040];

        assert!(!run_command(&mut ui, &mut eng, "bpm 140"));
        assert_eq!(eng.bpm, 140.0);
        assert!(!run_command(&mut ui, &mut eng, "bpm 999")); // clamped
        assert_eq!(eng.bpm, 200.0);
        assert!(!run_command(&mut ui, &mut eng, "swing 55"));
        assert_eq!(eng.swing, 55.0);
        assert!(!run_command(&mut ui, &mut eng, "pad 3"));
        assert_eq!(ui.selected_pad, 2);
        assert!(!run_command(&mut ui, &mut eng, "track 2"));
        assert_eq!(eng.four_track.selected, 1);
        assert!(!run_command(&mut ui, &mut eng, "loop 0.25 0.75"));
        assert!(eng.pads[2].loop_enabled);
        assert!((eng.pads[2].loop_pts.0 - 0.25).abs() < 1e-6);
        assert!(!run_command(&mut ui, &mut eng, "loop off"));
        assert!(!eng.pads[2].loop_enabled);
        assert!(!run_command(&mut ui, &mut eng, "loop"));
        assert!(eng.pads[2].loop_enabled);
        // Selected track is 2 (set above); ttrim applies to it.
        assert!(!run_command(&mut ui, &mut eng, "ttrim 0.2 0.8"));
        assert_eq!(eng.four_track.tracks[1].trim_start, 5208);
        assert_eq!(eng.four_track.tracks[1].trim_end, Some(20832));
        assert!(!run_command(&mut ui, &mut eng, "ttrim full"));
        assert_eq!(eng.four_track.tracks[1].trim_end, None);
        // Resample: track 1 -> pad 5, then default (selected track 2 -> pad 2).
        assert!(!run_command(&mut ui, &mut eng, "resample 1 5"));
        assert_eq!(eng.pads[4].sample.len(), 26040);
        assert!(eng.pads[4].custom);
        assert!(!run_command(&mut ui, &mut eng, "rec all"));
        assert!(eng.four_track.recording);
        assert!(eng.four_track.tracks.iter().all(|t| t.armed));
        assert!(!run_command(&mut ui, &mut eng, "stop"));
        assert!(!eng.four_track.recording);
        assert!(!eng.four_track.playing);
        assert!(!run_command(&mut ui, &mut eng, "play"));
        assert!(eng.four_track.playing);
        assert!(!run_command(&mut ui, &mut eng, "rewind"));
        assert!(!eng.four_track.playing);
        assert_eq!(eng.four_track.playhead, 0);
        // Errors are clean messages, never panics; unknown -> :help hint.
        assert!(!run_command(&mut ui, &mut eng, "bpm"));
        assert!(ui.msg.as_ref().unwrap().2);
        assert!(!run_command(&mut ui, &mut eng, "pad 9"));
        assert!(ui.msg.as_ref().unwrap().2);
        assert!(!run_command(&mut ui, &mut eng, "frobnicate"));
        assert!(ui.msg.as_ref().unwrap().0.contains("HELP"));
        assert!(!run_command(&mut ui, &mut eng, "help"));
        assert!(!ui.msg.as_ref().unwrap().2);
        assert!(!run_command(&mut ui, &mut eng, ""));
        assert!(run_command(&mut ui, &mut eng, "q"));
    }

    #[test]
    fn tape_u_record_all_and_b_resample_keys() {
        let mut eng = Engine::new();
        let mut ui = Ui::new();
        ui.page = Page::Tape;
        eng.four_track.tracks[0].samples = vec![0.5; 1000];
        let key = |kc| Event::KeyDown {
            timestamp: 0,
            window_id: 0,
            keycode: Some(kc),
            scancode: None,
            keymod: sdl2::keyboard::Mod::NOMOD,
            repeat: false,
        };
        handle_event(&mut ui, &mut eng, &key(Keycode::U));
        assert!(eng.four_track.recording);
        assert!(eng.four_track.tracks.iter().all(|t| t.armed));
        handle_event(&mut ui, &mut eng, &key(Keycode::U));
        assert!(!eng.four_track.recording);
        assert!(eng.four_track.tracks.iter().all(|t| !t.armed));
        // B resamples the selected track (1) onto the same-number pad.
        handle_event(&mut ui, &mut eng, &key(Keycode::B));
        assert_eq!(eng.pads[0].sample, vec![1024i16; 1000]);
        assert!(eng.pads[0].custom);
        assert_eq!(eng.pads[0].label.as_deref(), Some("TRK1"));
    }

    #[test]
    fn edit_loop_keys_and_tape_trim_keys() {
        let mut eng = Engine::new();
        let mut ui = Ui::new();
        let key = |kc, m| Event::KeyDown {
            timestamp: 0,
            window_id: 0,
            keycode: Some(kc),
            scancode: None,
            keymod: m,
            repeat: false,
        };
        let nomod = sdl2::keyboard::Mod::NOMOD;
        let shift = sdl2::keyboard::Mod::LSHIFTMOD;
        // Editor: L toggles loop; Shift+Z/X nudge loop start, plain X
        // nudges trim and leaves the loop alone.
        ui.page = Page::Edit;
        eng.pads[0].loop_pts = (0.5, 0.9);
        handle_event(&mut ui, &mut eng, &key(Keycode::L, nomod));
        assert!(eng.pads[0].loop_enabled);
        handle_event(&mut ui, &mut eng, &key(Keycode::Z, shift));
        assert!((eng.pads[0].loop_pts.0 - 0.49).abs() < 1e-6);
        handle_event(&mut ui, &mut eng, &key(Keycode::X, shift));
        assert!((eng.pads[0].loop_pts.0 - 0.5).abs() < 1e-6);
        handle_event(&mut ui, &mut eng, &key(Keycode::X, nomod));
        assert!((eng.pads[0].loop_pts.0 - 0.5).abs() < 1e-6);
        assert!(eng.pads[0].trim.0 > 0.0);
        // Tape: ,/. trim start, ;/' trim end, in 0.1 s steps.
        ui.page = Page::Tape;
        eng.four_track.tracks[0].samples = vec![0.5; 26040];
        handle_event(&mut ui, &mut eng, &key(Keycode::Period, nomod));
        assert_eq!(eng.four_track.tracks[0].trim_start, 2604);
        handle_event(&mut ui, &mut eng, &key(Keycode::Comma, nomod));
        assert_eq!(eng.four_track.tracks[0].trim_start, 0);
        handle_event(&mut ui, &mut eng, &key(Keycode::Semicolon, nomod));
        assert_eq!(eng.four_track.tracks[0].trim_end, Some(23436));
        handle_event(&mut ui, &mut eng, &key(Keycode::Quote, nomod));
        assert_eq!(eng.four_track.tracks[0].trim_end, Some(26040));
        // Drawing all three pages with loop + trim + cmdline set: no panic.
        eng.pads[0].loop_enabled = true;
        ui.cmdline = Some("bpm 1".to_string());
        let mut surf = Surface::new(W, H, PixelFormatEnum::RGB24).unwrap();
        for page in [Page::Pads, Page::Edit, Page::Tape] {
            ui.page = page;
            let p: &mut dyn Painter = &mut surf;
            draw(p, &ui, &eng);
        }
    }
}
