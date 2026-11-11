// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones

//! `lockstep`: the graphical front end. A ROM browser plus a window showing the running
//! game, driven by the real-time emulation thread in [`lockstep_core::rt`].
//! Run `lockstep --help` for usage, or see README.md.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use macroquad::prelude::*;
use lockstep_core::cartridge::Cartridge;
use lockstep_core::input::{
    BTN_A, BTN_B, BTN_DOWN, BTN_LEFT, BTN_RIGHT, BTN_SELECT, BTN_START, BTN_UP,
};
use lockstep_core::mappers;
use lockstep_core::ppu::{SCREEN_HEIGHT, SCREEN_WIDTH};
use lockstep_core::rt::{
    DisplayClock, EmulatorCommand, RtSharedState, spawn_emulator_worker, spawn_gamepad_poller,
};

/// Picks the directory the ROM browser opens in, in order of preference:
/// 1. the command-line argument (a directory, or the directory of a ROM file),
/// 2. `$LOCKSTEP_ROM_DIR`,
/// 3. a `roms`, `ROMs` or `NES` folder in (or next to) the current directory,
/// 4. the current directory.
fn initial_rom_dir(arg: Option<&Path>) -> PathBuf {
    if let Some(p) = arg {
        if p.is_dir() {
            return p.to_path_buf();
        }
        if let Some(parent) = p.parent().filter(|d| d.is_dir()) {
            return parent.to_path_buf();
        }
    }
    if let Some(dir) = env::var_os("LOCKSTEP_ROM_DIR").map(PathBuf::from).filter(|d| d.is_dir()) {
        return dir;
    }
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for candidate in ["roms", "ROMs", "NES", "../roms", "../ROMs", "../NES"] {
        let d = cwd.join(candidate);
        if d.is_dir() {
            return d.canonicalize().unwrap_or(d);
        }
    }
    cwd
}

/// Validates that the cartridge's board is emulated and returns "Title [Mapper]".
fn describe_cart(cart: &Cartridge) -> Result<String, String> {
    let m = mappers::create_mapper(cart)?;
    Ok(format!("{} [{}]", cart.title, m.name()))
}

/// Initial window: 3x the NES resolution, resizable.
fn window_conf() -> Conf {
    Conf {
        window_title: "Lockstep".to_string(),
        window_width: (SCREEN_WIDTH * 3) as i32,
        window_height: (SCREEN_HEIGHT * 3) as i32,
        window_resizable: true,
        high_dpi: false,
        ..Default::default()
    }
}

/// Keyboard state mapped to NES buttons (`BTN_*` bits) that are currently held.
fn poll_joypad_buttons() -> u8 {
    let mut mask = 0u8;

    if is_key_down(KeyCode::Z) || is_key_down(KeyCode::J) || is_key_down(KeyCode::Space) {
        mask |= BTN_A;
    }
    if is_key_down(KeyCode::X) || is_key_down(KeyCode::K) || is_key_down(KeyCode::LeftShift) {
        mask |= BTN_B;
    }
    if is_key_down(KeyCode::RightShift) || is_key_down(KeyCode::Tab) {
        mask |= BTN_SELECT;
    }
    if is_key_down(KeyCode::Enter) {
        mask |= BTN_START;
    }
    if is_key_down(KeyCode::Up) || is_key_down(KeyCode::W) {
        mask |= BTN_UP;
    }
    if is_key_down(KeyCode::Down) || is_key_down(KeyCode::S) {
        mask |= BTN_DOWN;
    }
    if is_key_down(KeyCode::Left) || is_key_down(KeyCode::A) {
        mask |= BTN_LEFT;
    }
    if is_key_down(KeyCode::Right) || is_key_down(KeyCode::D) {
        mask |= BTN_RIGHT;
    }

    mask
}

/// NES buttons whose key went down this UI frame (latched so short taps aren't lost).
fn poll_joypad_pressed() -> u8 {
    let mut mask = 0u8;

    if is_key_pressed(KeyCode::Z) || is_key_pressed(KeyCode::J) || is_key_pressed(KeyCode::Space) {
        mask |= BTN_A;
    }
    if is_key_pressed(KeyCode::X)
        || is_key_pressed(KeyCode::K)
        || is_key_pressed(KeyCode::LeftShift)
    {
        mask |= BTN_B;
    }
    if is_key_pressed(KeyCode::RightShift) || is_key_pressed(KeyCode::Tab) {
        mask |= BTN_SELECT;
    }
    if is_key_pressed(KeyCode::Enter) {
        mask |= BTN_START;
    }
    if is_key_pressed(KeyCode::Up) || is_key_pressed(KeyCode::W) {
        mask |= BTN_UP;
    }
    if is_key_pressed(KeyCode::Down) || is_key_pressed(KeyCode::S) {
        mask |= BTN_DOWN;
    }
    if is_key_pressed(KeyCode::Left) || is_key_pressed(KeyCode::A) {
        mask |= BTN_LEFT;
    }
    if is_key_pressed(KeyCode::Right) || is_key_pressed(KeyCode::D) {
        mask |= BTN_RIGHT;
    }

    mask
}

/// Auto-repeat for a held key: fires on press, then after a short delay at an
/// accelerating rate (so long lists can be scrolled by holding Up/Down).
#[derive(Default)]
struct KeyRepeat {
    /// Seconds the key has been held (0 = not held).
    held: f64,
    /// Hold time at which the next repeat fires.
    next: f64,
}

impl KeyRepeat {
    /// Advances by `dt` seconds with the key `down`; returns true when the action should fire.
    fn fire(&mut self, down: bool, dt: f64) -> bool {
        if !down {
            self.held = 0.0;
            self.next = 0.0;
            return false;
        }
        let first = self.held == 0.0;
        self.held += dt.max(1e-6);
        if first {
            self.next = 0.30;
            return true;
        }
        if self.held >= self.next {
            // 12 steps/s at first, ramping to ~40 steps/s after ~1.5 s held.
            let rate = if self.held > 1.5 { 40.0 } else { 12.0 };
            self.next = self.held + 1.0 / rate;
            return true;
        }
        false
    }
}

/// One-line summary of the real-time pipeline for the browser panel.
fn rt_status_lines(shared: &RtSharedState) -> (String, String) {
    let period = shared.display_period_ns.load(Ordering::Acquire);
    let sync = if shared.display_sync_active.load(Ordering::Acquire) {
        format!(
            "Display-synced {:.2} Hz (JIT margin {:.1} ms)",
            1e9 / period as f64,
            shared.present_margin_ns.load(Ordering::Acquire) as f64 / 1e6
        )
    } else if period > 0 {
        format!("Free-run 60.10 Hz (display {:.1} Hz)", 1e9 / period as f64)
    } else {
        "Free-run 60.10 Hz".to_string()
    };
    let prio = shared.sched_fifo_prio.load(Ordering::Acquire);
    let rt_on = shared.rt_mode_enabled.load(Ordering::Acquire);
    let sched_label = if prio > 0 {
        format!("SCHED_FIFO {prio}")
    } else if cfg!(target_os = "windows") && rt_on {
        "Win32 High-Prio (1 ms)".to_string()
    } else if cfg!(target_os = "linux") {
        "SCHED_OTHER".to_string()
    } else {
        "Standard".to_string()
    };
    let audio = match shared.audio_stats.get() {
        Some(a) => format!(
            "Audio buffer {:.1}-{:.1} ms, DRC {:+} ppm, {} underruns{}",
            a.low_water_us.load(Ordering::Relaxed) as f64 / 1000.0,
            a.buffered_us.load(Ordering::Relaxed) as f64 / 1000.0,
            a.drc_ppm.load(Ordering::Relaxed) as i64,
            a.underruns.load(Ordering::Relaxed),
            if a.rt_promoted.load(Ordering::Relaxed) { " (RT)" } else { "" }
        ),
        None => "audio off".to_string(),
    };
    let line1 = format!(
        "{} | frame cost {:.1} ms | run-ahead {} | {} | max jitter {:.0} us",
        sync,
        shared.frame_cost_ns.load(Ordering::Relaxed) as f64 / 1e6,
        shared.run_ahead.load(Ordering::Acquire),
        sched_label,
        shared.max_abs_jitter_ns.load(Ordering::Relaxed) as f64 / 1000.0,
    );
    (line1, audio)
}

/// Truncates `text` with an ellipsis so it fits in `max_w` pixels at `size`.
fn fit_text(text: &str, max_w: f32, size: u16) -> String {
    if measure_text(text, None, size, 1.0).width <= max_w {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let t: String = chars.iter().collect::<String>() + "...";
        if measure_text(&t, None, size, 1.0).width <= max_w {
            return t;
        }
    }
    String::new()
}

/// One row of the ROM browser.
#[derive(Clone, Debug)]
struct DirEntryItem {
    /// Display text (directories are prefixed with `[DIR]`).
    name: String,
    /// Full path of the file or directory.
    path: PathBuf,
    /// Whether Enter descends into it instead of loading it.
    is_dir: bool,
}

/// The ROM browser overlay: a directory listing of `.nes`/`.zip` files and folders.
struct RomBrowser {
    /// Directory being listed.
    current_dir: PathBuf,
    /// Parent link, then ROMs, then sub-directories, each sorted case-insensitively.
    entries: Vec<DirEntryItem>,
    /// Index of the highlighted entry.
    selected: usize,
    /// Message shown above the key help (load errors, unsupported mappers).
    status_msg: String,
}

impl RomBrowser {
    /// Opens the browser at `start_dir`.
    fn new(start_dir: PathBuf) -> Self {
        let mut browser = Self {
            current_dir: start_dir,
            entries: Vec::new(),
            selected: 0,
            status_msg: "Select a .nes ROM and press Enter to load".to_string(),
        };
        browser.refresh();
        browser
    }

    /// Re-reads `current_dir` (hidden files are skipped) and clamps the selection.
    fn refresh(&mut self) {
        self.entries.clear();
        if let Some(parent) = self.current_dir.parent() {
            self.entries.push(DirEntryItem {
                name: "[..] Parent Directory".to_string(),
                path: parent.to_path_buf(),
                is_dir: true,
            });
        }

        if let Ok(read_dir) = fs::read_dir(&self.current_dir) {
            let mut dirs = Vec::new();
            let mut roms = Vec::new();

            for entry in read_dir.flatten() {
                let path = entry.path();
                let file_name = entry.file_name().to_string_lossy().into_owned();
                if file_name.starts_with('.') {
                    continue;
                }
                if path.is_dir() {
                    dirs.push(DirEntryItem {
                        name: format!("[DIR] {}", file_name),
                        path,
                        is_dir: true,
                    });
                } else if path.extension().is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("nes") || ext.eq_ignore_ascii_case("zip")
                }) {
                    roms.push(DirEntryItem {
                        name: file_name,
                        path,
                        is_dir: false,
                    });
                }
            }

            dirs.sort_by_key(|a| a.name.to_lowercase());
            roms.sort_by_key(|a| a.name.to_lowercase());

            // Put .nes ROMs first for quick access, followed by subdirectories
            self.entries.extend(roms);
            self.entries.extend(dirs);
        }

        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
    }
}

/// `--help` text.
const HELP: &str = "\
Lockstep - cycle-accurate NES emulator with low-latency real-time scheduling

USAGE:
    lockstep [ROM_FILE | ROM_DIRECTORY]

    With a ROM (.nes, or a .zip containing one) the game starts immediately; with a
    directory (or nothing) the ROM browser opens there.

ENVIRONMENT:
    LOCKSTEP_ROM_DIR   Directory the ROM browser opens in by default
    LOCKSTEP_RT_PRIO   SCHED_FIFO priority of the emulation thread (default 45, Linux)
    LOCKSTEP_PAD_SWAP  If set, swap South/East gamepad buttons (Nintendo A/B layout)
    LOCKSTEP_STATS     If set, print real-time statistics to stderr every 2 s

KEYS & GAMEPADS:
    USB / Bluetooth controllers (P1 & P2) are detected automatically via plug-and-play.
    D-pad / Left Stick: directions   South / North / R1 / R2: A   West / East / L1 / L2: B
    Start / Select: Start / Select   Guide (or Select+Start): ROM browser

    Arrows / WASD   D-pad          Z / J / Space   A
    X / K / LShift  B              Enter           Start
    RShift / Tab    Select         R               Reset
    Backspace       Rewind (hold)  ` / ~           Fast-forward (hold)
    P / .           Pause / Step   1 - 9           Select save-state slot (1-9)
    Mouse L / R     Zapper light-gun aim + fire / off-screen shot (Port 2)
    Esc / F1  ROM browser   F2 Unload   F3 RT scheduling   F4 Overclock
    F5 / F8   Save / load   F6 Crop     F7 Run-ahead       F9 Pad A/B   F10 CRT/4:3   F11 Fullscreen

See README.md and docs/ for details.";

/// Short GPL notice printed by `--help`/`--version` (as the GPL recommends for interactive programs).
const LICENSE_NOTICE: &str = "\
Lockstep  Copyright (C) 2026 Zach Jones
This program comes with ABSOLUTELY NO WARRANTY. This is free software, and you are
welcome to redistribute it under the terms of the GNU General Public License, version 3
or (at your option) any later version. See the LICENSE file for details.";

fn main() {
    match env::args().nth(1).as_deref() {
        Some("-h" | "--help") => {
            println!("{HELP}\n\n{LICENSE_NOTICE}");
            return;
        }
        Some("-V" | "--version") => {
            println!("lockstep {}\n\n{LICENSE_NOTICE}", env!("CARGO_PKG_VERSION"));
            return;
        }
        _ => {}
    }
    macroquad::Window::from_config(window_conf(), run());
}

/// The GUI main loop, run once per display refresh:
/// hotkeys → browser or controller input → wait for the just-in-time frame →
/// draw → swap → measure the refresh (`DisplayClock`).
async fn run() {
    let shared = Arc::new(RtSharedState::new());
    shared.register_ui_thread();
    shared
        .pad_swap_ab
        .store(env::var_os("LOCKSTEP_PAD_SWAP").is_some(), Ordering::Relaxed);
    let (cmd_tx, _worker_handle) = spawn_emulator_worker(Arc::clone(&shared));
    let _pad_handle = spawn_gamepad_poller(Arc::clone(&shared));

    let mut image = Image {
        bytes: vec![0; SCREEN_WIDTH * SCREEN_HEIGHT * 4],
        width: SCREEN_WIDTH as u16,
        height: SCREEN_HEIGHT as u16,
    };
    let texture = Texture2D::from_image(&image);
    texture.set_filter(FilterMode::Nearest);

    let arg = env::args().nth(1).map(PathBuf::from);
    let initial_dir = initial_rom_dir(arg.as_deref());

    let mut browser = RomBrowser::new(initial_dir);
    let mut show_browser = true;
    let mut rep_up = KeyRepeat::default();
    let mut rep_down = KeyRepeat::default();
    let mut rep_pgup = KeyRepeat::default();
    let mut rep_pgdn = KeyRepeat::default();
    // Crop 8 px of NTSC overscan on every edge (hides the PPU left-column mask and
    // top/bottom garbage lines that CRTs never displayed). Toggle with F6.
    let mut crop_overscan = true;
    let mut current_rom_info = String::from("No ROM Loaded");
    let mut toast_text = String::new();
    let mut toast_timer = 0.0f64;

    // If a .nes file path was passed on the command line, load it immediately
    if let Some(arg_path) = arg.filter(|p| p.is_file()) {
        match Cartridge::from_file(&arg_path) {
            Ok(cart) => match describe_cart(&cart) {
                Ok(info) => {
                    current_rom_info = info;
                    let _ = cmd_tx.send(EmulatorCommand::LoadRom(Box::new(cart)));
                    show_browser = false;
                    toast_text = format!("Loaded {}", current_rom_info);
                    toast_timer = 3.0;
                }
                Err(e) => browser.status_msg = e,
            },
            Err(e) => {
                browser.status_msg = e;
            }
        }
    }

    let mut last_time = get_time();
    let mut display_clock = DisplayClock::new();
    let mut last_drawn_seq = 0u64;
    let mut fullscreen = false;
    let mut state_slot: u8 = 1;
    let mut zapper_reticle_timer = 0.0f64;
    // Display filter mode (0 = Sharp 8:7 pixel-exact, 1 = NTSC 4:3, 2 = CRT Scanlines 4:3).
    let mut display_filter: u8 = if env::var_os("LOCKSTEP_CRT").is_some() { 2 } else { 0 };

    loop {
        let now = get_time();
        let dt = (now - last_time).clamp(0.0, 0.1);
        last_time = now;
        if toast_timer > 0.0 {
            toast_timer = (toast_timer - dt).max(0.0);
        }
        if zapper_reticle_timer > 0.0 {
            zapper_reticle_timer = (zapper_reticle_timer - dt).max(0.0);
        }

        for note in shared.drain_toasts() {
            toast_text = note;
            toast_timer = 3.0;
        }
        let pad_held = shared.pad_buttons_held.load(Ordering::Acquire);
        let pad_pressed = shared.pad_browser_pressed.swap(0, Ordering::AcqRel);
        let pad_page_up = shared.pad_page_up.load(Ordering::Relaxed);
        let pad_page_down = shared.pad_page_down.load(Ordering::Relaxed);
        let pad_toggle_browser = shared.pad_toggle_browser.swap(false, Ordering::AcqRel);

        // Global Hotkeys (keyboard or gamepad Guide / Select+Start chord)
        if is_key_pressed(KeyCode::F1) || is_key_pressed(KeyCode::Escape) || pad_toggle_browser {
            if shared.rom_loaded.load(Ordering::Acquire) {
                show_browser = !show_browser;
                if show_browser {
                    browser.refresh();
                }
            } else {
                show_browser = true;
            }
        }

        if is_key_pressed(KeyCode::F2) && shared.rom_loaded.load(Ordering::Acquire) {
            let _ = cmd_tx.send(EmulatorCommand::UnloadRom);
            current_rom_info = String::from("No ROM Loaded");
            show_browser = true;
            browser.refresh();
            toast_text = "ROM Unloaded".to_string();
            toast_timer = 2.5;
        }

        if is_key_pressed(KeyCode::F3) {
            let next_rt = !shared.rt_mode_enabled.load(Ordering::Acquire);
            let _ = cmd_tx.send(EmulatorCommand::SetRtMode(next_rt));
            toast_text = if next_rt {
                if cfg!(target_os = "linux") {
                    "Scheduling: PREEMPT_RT / SCHED_FIFO Enabled".to_string()
                } else {
                    "Scheduling: High Priority + 1 ms Timer + Spin Guard".to_string()
                }
            } else {
                "Scheduling: Standard".to_string()
            };
            toast_timer = 3.0;
        }

        if is_key_pressed(KeyCode::F4) {
            let next_oc = !shared.overclock_enabled.load(Ordering::Acquire);
            let _ = cmd_tx.send(EmulatorCommand::SetOverclock(next_oc));
            toast_text = if next_oc {
                "CPU Overclock: ON (+100 idle scanlines before VBlank)".to_string()
            } else {
                "CPU Overclock: OFF (accurate timing)".to_string()
            };
            toast_timer = 3.0;
        }

        for (slot, key) in [
            (1u8, KeyCode::Key1),
            (2, KeyCode::Key2),
            (3, KeyCode::Key3),
            (4, KeyCode::Key4),
            (5, KeyCode::Key5),
            (6, KeyCode::Key6),
            (7, KeyCode::Key7),
            (8, KeyCode::Key8),
            (9, KeyCode::Key9),
        ] {
            if is_key_pressed(key) {
                state_slot = slot;
                toast_text = format!("Save State Slot {} selected", state_slot);
                toast_timer = 1.5;
            }
        }

        if is_key_pressed(KeyCode::F5) && shared.rom_loaded.load(Ordering::Acquire) {
            let _ = cmd_tx.send(EmulatorCommand::SaveState(state_slot));
        }

        if is_key_pressed(KeyCode::F8) && shared.rom_loaded.load(Ordering::Acquire) {
            let _ = cmd_tx.send(EmulatorCommand::LoadState(state_slot));
        }

        if is_key_pressed(KeyCode::F9) {
            let next_swap = !shared.pad_swap_ab.load(Ordering::Relaxed);
            shared.pad_swap_ab.store(next_swap, Ordering::Relaxed);
            toast_text = if next_swap {
                "Gamepad A/B Layout: Swapped (East=A, South=B)".to_string()
            } else {
                "Gamepad A/B Layout: Default (South=A, West=B)".to_string()
            };
            toast_timer = 2.5;
        }

        if is_key_pressed(KeyCode::F10) {
            display_filter = (display_filter + 1) % 3;
            toast_text = match display_filter {
                0 => "Display: Sharp 8:7 (Pixel-Exact)".to_string(),
                1 => "Display: NTSC 4:3 Aspect Ratio".to_string(),
                _ => "Display: CRT Scanlines + NTSC 4:3".to_string(),
            };
            toast_timer = 2.5;
        }

        if is_key_pressed(KeyCode::F7) {
            let next = (shared.run_ahead.load(Ordering::Acquire) + 1) % 3;
            let _ = cmd_tx.send(EmulatorCommand::SetRunAhead(next));
            toast_text = match next {
                0 => "Run-ahead: OFF".to_string(),
                n => format!("Run-ahead: {n} frame{} (lower input lag)", if n > 1 { "s" } else { "" }),
            };
            toast_timer = 2.0;
        }

        if is_key_pressed(KeyCode::F11) {
            fullscreen = !fullscreen;
            set_fullscreen(fullscreen);
        }

        if is_key_pressed(KeyCode::F6) {
            crop_overscan = !crop_overscan;
            toast_text = format!("Overscan crop: {}", if crop_overscan { "ON" } else { "OFF" });
            toast_timer = 1.5;
        }

        if show_browser {
            // Pause keyboard input to the game while browsing files
            shared.update_input(0, 0);
            shared.rewind_active.store(false, Ordering::Release);
            shared.fast_forward.store(false, Ordering::Release);

            let n = browser.entries.len();
            let up_down = is_key_down(KeyCode::Up) || (pad_held & BTN_UP != 0);
            let down_down = is_key_down(KeyCode::Down) || (pad_held & BTN_DOWN != 0);
            let pgup_down = is_key_down(KeyCode::PageUp)
                || is_key_down(KeyCode::Left)
                || (pad_held & BTN_LEFT != 0)
                || pad_page_up;
            let pgdn_down = is_key_down(KeyCode::PageDown)
                || is_key_down(KeyCode::Right)
                || (pad_held & BTN_RIGHT != 0)
                || pad_page_down;

            if rep_up.fire(up_down, dt) && browser.selected > 0 {
                browser.selected -= 1;
            }
            if rep_down.fire(down_down, dt) && browser.selected + 1 < n {
                browser.selected += 1;
            }
            if rep_pgup.fire(pgup_down, dt) {
                browser.selected = browser.selected.saturating_sub(20);
            }
            if rep_pgdn.fire(pgdn_down, dt) && n > 0 {
                browser.selected = (browser.selected + 20).min(n - 1);
            }
            if is_key_pressed(KeyCode::Home) {
                browser.selected = 0;
            }
            if is_key_pressed(KeyCode::End) && !browser.entries.is_empty() {
                browser.selected = browser.entries.len() - 1;
            }
            if (is_key_pressed(KeyCode::Backspace) || (pad_pressed & BTN_B != 0))
                && let Some(parent) = browser.current_dir.parent()
            {
                browser.current_dir = parent.to_path_buf();
                browser.selected = 0;
                browser.refresh();
            }
            if (is_key_pressed(KeyCode::Enter)
                || (pad_pressed & (BTN_A | BTN_START) != 0 && !pad_toggle_browser))
                && let Some(item) = browser.entries.get(browser.selected).cloned()
            {
                if item.is_dir {
                    browser.current_dir = item.path;
                    browser.selected = 0;
                    browser.refresh();
                } else {
                    match Cartridge::from_file(&item.path) {
                        Ok(cart) => match describe_cart(&cart) {
                            Ok(info) => {
                                current_rom_info = info;
                                let _ = cmd_tx.send(EmulatorCommand::LoadRom(Box::new(cart)));
                                show_browser = false;
                                toast_text = format!("Loaded {}", current_rom_info);
                                toast_timer = 3.0;
                            }
                            Err(e) => browser.status_msg = e,
                        },
                        Err(e) => {
                            browser.status_msg = e;
                        }
                    }
                }
            }
        } else {
            if is_key_pressed(KeyCode::R) {
                let _ = cmd_tx.send(EmulatorCommand::Reset);
                toast_text = "NES Reset".to_string();
                toast_timer = 1.5;
            }
            if is_key_pressed(KeyCode::P) && shared.rom_loaded.load(Ordering::Acquire) {
                let next_p = !shared.paused.load(Ordering::Acquire);
                shared.paused.store(next_p, Ordering::Release);
                toast_text = if next_p { "Emulation Paused (press . to step frame)".to_string() } else { "Emulation Resumed".to_string() };
                toast_timer = 2.0;
            }
            if is_key_pressed(KeyCode::Period) && shared.rom_loaded.load(Ordering::Acquire) {
                shared.paused.store(true, Ordering::Release);
                let _ = shared
                    .step_frames
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| Some(v.saturating_add(1)));
            }
            let rew = is_key_down(KeyCode::Backspace)
                || ((pad_held & BTN_SELECT != 0) && pad_page_up);
            let ff = is_key_down(KeyCode::GraveAccent)
                || ((pad_held & BTN_SELECT != 0) && pad_page_down);
            shared.rewind_active.store(rew, Ordering::Release);
            shared.fast_forward.store(ff && !rew, Ordering::Release);

            let held = poll_joypad_buttons();
            let pressed = poll_joypad_pressed();
            shared.update_input(held, pressed);
        }

        // Display sync: wait (bounded) for the frame the worker is producing just in
        // time for the upcoming refresh, so it is shown with minimum delay.
        shared.wait_for_jit_frame(last_drawn_seq);
        last_drawn_seq = shared.frame_seq.load(Ordering::Acquire);
        if shared
            .framebuffer
            .with_latest(|fb| image.bytes.copy_from_slice(fb))
            .is_some()
        {
            texture.update(&image);
        }

        clear_background(BLACK);

        let sw = screen_width();
        let sh = screen_height();

        if shared.rom_loaded.load(Ordering::Acquire) {
            let crop = if crop_overscan { 8.0 } else { 0.0 };
            let src = Rect::new(
                crop,
                crop,
                SCREEN_WIDTH as f32 - 2.0 * crop,
                SCREEN_HEIGHT as f32 - 2.0 * crop,
            );
            let par = if display_filter >= 1 { 8.0 / 7.0 } else { 1.0 };
            let aspect = (src.w * par) / src.h;
            let (draw_w, draw_h) = if sw / sh > aspect {
                (sh * aspect, sh)
            } else {
                (sw, sw / aspect)
            };
            let draw_x = ((sw - draw_w) * 0.5).floor();
            let draw_y = ((sh - draw_h) * 0.5).floor();

            draw_texture_ex(
                &texture,
                draw_x,
                draw_y,
                WHITE,
                DrawTextureParams {
                    dest_size: Some(vec2(draw_w.round(), draw_h.round())),
                    source: Some(src),
                    ..Default::default()
                },
            );

            // CRT scanline & phosphor aperture overlay when `display_filter == 2`.
            if display_filter == 2 {
                let scanlines = src.h as usize;
                let line_step = draw_h / (scanlines as f32);
                if line_step >= 2.0 {
                    let bar_h = (line_step * 0.34).clamp(1.0, 3.0);
                    let scan_col = Color::new(0.0, 0.0, 0.0, 0.36);
                    for i in 0..scanlines {
                        let y = draw_y + (i as f32 + 0.66) * line_step;
                        draw_rectangle(draw_x, y, draw_w, bar_h, scan_col);
                    }
                }
                // Subtle CRT bezel frame around the 4:3 tube area.
                draw_rectangle_lines(
                    draw_x - 1.0,
                    draw_y - 1.0,
                    draw_w + 2.0,
                    draw_h + 2.0,
                    2.0,
                    Color::new(0.18, 0.20, 0.25, 0.85),
                );
            }

            if !show_browser {
                let badge = if shared.rewind_active.load(Ordering::Acquire) {
                    Some(("<< REWIND", Color::new(0.95, 0.65, 0.20, 0.90)))
                } else if shared.fast_forward.load(Ordering::Acquire) {
                    Some((">> FAST-FWD 2x", Color::new(0.35, 0.85, 0.55, 0.90)))
                } else if shared.paused.load(Ordering::Acquire) {
                    Some(("|| PAUSED", Color::new(0.90, 0.85, 0.35, 0.90)))
                } else {
                    None
                };
                if let Some((label, col)) = badge {
                    let tw = measure_text(label, None, 18, 1.0).width;
                    let bx = draw_x + draw_w - tw - 24.0;
                    let by = draw_y + 14.0;
                    draw_rectangle(bx - 8.0, by, tw + 16.0, 26.0, Color::new(0.05, 0.07, 0.12, 0.80));
                    draw_text(label, bx, by + 19.0, 18.0, col);
                }
            }

            if !show_browser {
                let (mx, my) = mouse_position();
                let rel_x = (mx - draw_x) / draw_w.max(1.0);
                let rel_y = (my - draw_y) / draw_h.max(1.0);
                let in_bounds = (0.0..1.0).contains(&rel_x) && (0.0..1.0).contains(&rel_y);
                let (nes_x, nes_y) = if in_bounds {
                    (
                        (src.x + rel_x * src.w) as i32,
                        (src.y + rel_y * src.h) as i32,
                    )
                } else {
                    (-1, -1)
                };
                if is_mouse_button_pressed(MouseButton::Left) {
                    shared.update_zapper(nes_x, nes_y, 2);
                    zapper_reticle_timer = 3.0;
                } else if is_mouse_button_pressed(MouseButton::Right) {
                    shared.update_zapper(-1, -1, 2);
                    zapper_reticle_timer = 3.0;
                } else {
                    shared.update_zapper(nes_x, nes_y, 0);
                }
                if zapper_reticle_timer > 0.0 && in_bounds {
                    let c = Color::new(1.0, 0.25, 0.25, 0.85);
                    draw_circle_lines(mx, my, 9.0, 1.5, c);
                    draw_line(mx - 14.0, my, mx - 4.0, my, 1.5, c);
                    draw_line(mx + 4.0, my, mx + 14.0, my, 1.5, c);
                    draw_line(mx, my - 14.0, mx, my - 4.0, 1.5, c);
                    draw_line(mx, my + 4.0, mx, my + 14.0, 1.5, c);
                }
            }
        }

        // Brief notification after an action (load, save state, toggles...), sized to fit.
        if toast_timer > 0.0 && !toast_text.is_empty() {
            let size = 18u16;
            let text = fit_text(&toast_text, sw - 40.0, size);
            let w = measure_text(&text, None, size, 1.0).width;
            let alpha = toast_timer.min(0.5) as f32 * 2.0;
            let (x, y) = (((sw - w) * 0.5).floor(), sh - 40.0);
            draw_rectangle(x - 10.0, y - 20.0, w + 20.0, 30.0, Color::new(0.05, 0.07, 0.12, 0.80 * alpha));
            draw_text(&text, x, y, size as f32, Color::new(1.0, 1.0, 1.0, alpha));
        }

        // Draw ROM Browser Overlay when active
        if show_browser {
            let panel_x = 16.0;
            let panel_y = 16.0;
            let panel_w = (sw - 32.0).max(200.0);
            let panel_h = (sh - 32.0).max(200.0);
            let text_w = panel_w - 36.0;

            draw_rectangle(
                panel_x,
                panel_y,
                panel_w,
                panel_h,
                Color::new(0.08, 0.10, 0.16, 0.94),
            );
            draw_rectangle_lines(
                panel_x,
                panel_y,
                panel_w,
                panel_h,
                2.0,
                Color::new(0.35, 0.55, 0.95, 1.0),
            );

            draw_text(
                "Lockstep - NES",
                panel_x + 18.0,
                panel_y + 28.0,
                24.0,
                Color::new(0.55, 0.80, 1.0, 1.0),
            );
            let host_info = if cfg!(target_os = "linux") {
                format!(
                    "Kernel {} (PREEMPT_RT={}, RTPRIO={})",
                    shared.caps.kernel_release, shared.caps.preempt_rt_kernel, shared.caps.max_rtprio
                )
            } else {
                shared.caps.kernel_release.clone()
            };
            let pad_summary = shared
                .pad_status
                .lock()
                .map(|s| s.clone())
                .unwrap_or_default();
            draw_text(
                &fit_text(
                    &format!(
                        "{}  |  Slot {}  |  {}  |  {}",
                        current_rom_info,
                        state_slot,
                        host_info,
                        pad_summary
                    ),
                    text_w,
                    16,
                ),
                panel_x + 18.0,
                panel_y + 50.0,
                16.0,
                LIGHTGRAY,
            );
            let (rt1, rt2) = rt_status_lines(&shared);
            for (i, line) in [rt1, rt2].iter().enumerate() {
                draw_text(
                    &fit_text(line, text_w, 16),
                    panel_x + 18.0,
                    panel_y + 94.0 + i as f32 * 20.0,
                    16.0,
                    Color::new(0.6, 0.9, 0.7, 1.0),
                );
            }
            draw_text(
                &fit_text(&format!("Directory: {}", browser.current_dir.display()), text_w, 16),
                panel_x + 18.0,
                panel_y + 72.0,
                16.0,
                GRAY,
            );

            let list_top = panel_y + 142.0;
            let row_h = 24.0;
            let max_visible = ((panel_h - 228.0) / row_h).max(1.0) as usize;
            let start_idx = browser
                .selected
                .saturating_sub(max_visible / 2)
                .min(browser.entries.len().saturating_sub(max_visible));
            let end_idx = (start_idx + max_visible).min(browser.entries.len());

            for (vis_i, idx) in (start_idx..end_idx).enumerate() {
                let entry = &browser.entries[idx];
                let y = list_top + (vis_i as f32) * row_h;
                if idx == browser.selected {
                    draw_rectangle(
                        panel_x + 12.0,
                        y - 16.0,
                        panel_w - 24.0,
                        row_h - 2.0,
                        Color::new(0.20, 0.38, 0.72, 0.90),
                    );
                }
                let color = if entry.is_dir {
                    Color::new(0.95, 0.82, 0.45, 1.0)
                } else {
                    WHITE
                };
                draw_text(&fit_text(&entry.name, text_w - 8.0, 18), panel_x + 22.0, y, 18.0, color);
            }

            let footer_y = panel_y + panel_h - 12.0;
            draw_text(
                &fit_text(&browser.status_msg, text_w, 16),
                panel_x + 18.0,
                footer_y - 38.0,
                16.0,
                Color::new(1.0, 0.85, 0.55, 1.0),
            );
            let help = Color::new(0.70, 0.85, 0.95, 1.0);
            draw_text(
                &fit_text(
                    concat!("Lockstep ", env!("CARGO_PKG_VERSION"), " - free software under GPL-3.0-or-later, ABSOLUTELY NO WARRANTY (see LICENSE)"),
                    text_w,
                    14,
                ),
                panel_x + 18.0,
                footer_y - 58.0,
                14.0,
                GRAY,
            );
            draw_text(
                &fit_text("Up/Down (hold to scroll)  Left/Right/PgUp/PgDn: Page  Home/End  Enter: Load  Backspace: Up Dir", text_w, 15),
                panel_x + 18.0,
                footer_y - 18.0,
                15.0,
                help,
            );
            draw_text(
                &fit_text("Esc/F1: Game  F2: Unload  F3: RT  F4: OC  1-9: Slot  F5/F8: State  F6: Crop  F7: Run-ahead  F9: A/B  F10: CRT  Bksp: Rewind  P/.: Pause", text_w, 15),
                panel_x + 18.0,
                footer_y,
                15.0,
                help,
            );
        }

        next_frame().await;
        display_clock.tick(&shared, shared.display_sync_active.load(Ordering::Acquire));
    }
}

