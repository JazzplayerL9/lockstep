
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use macroquad::prelude::*;
use nes_core::cartridge::Cartridge;
use nes_core::input::{BTN_A, BTN_B, BTN_DOWN, BTN_LEFT, BTN_RIGHT, BTN_SELECT, BTN_START, BTN_UP};
use nes_core::mappers;
use nes_core::ppu::{SCREEN_HEIGHT, SCREEN_WIDTH};
use nes_core::rt::{EmulatorCommand, RtSharedState, spawn_emulator_worker};

/// Validates that the cartridge's board is emulated and returns "Title [Mapper]".
fn describe_cart(cart: &Cartridge) -> Result<String, String> {
    let m = mappers::create_mapper(cart)?;
    Ok(format!("{} [{}]", cart.title, m.name()))
}

fn window_conf() -> Conf {
    Conf {
        window_title: "NES-RT Emulator".to_string(),
        window_width: (SCREEN_WIDTH * 3) as i32,
        window_height: (SCREEN_HEIGHT * 3) as i32,
        window_resizable: true,
        high_dpi: false,
        ..Default::default()
    }
}

fn poll_joypad_buttons() -> u8 {
    let mut mask = 0u8;

    if is_key_down(KeyCode::Z) || is_key_down(KeyCode::J) || is_key_down(KeyCode::Space) {
        mask |= BTN_A;
    }
    if is_key_down(KeyCode::X) || is_key_down(KeyCode::K) || is_key_down(KeyCode::LeftShift) {
        mask |= BTN_B;
    }
    if is_key_down(KeyCode::RightShift)
        || is_key_down(KeyCode::Tab)
        || is_key_down(KeyCode::Backspace)
    {
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
    if is_key_pressed(KeyCode::RightShift)
        || is_key_pressed(KeyCode::Tab)
        || is_key_pressed(KeyCode::Backspace)
    {
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
    held: f64,
    next: f64,
}

impl KeyRepeat {
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

#[derive(Clone, Debug)]
struct DirEntryItem {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

struct RomBrowser {
    current_dir: PathBuf,
    entries: Vec<DirEntryItem>,
    selected: usize,
    status_msg: String,
}

impl RomBrowser {
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

#[macroquad::main(window_conf)]
async fn main() {
    let shared = Arc::new(RtSharedState::new());
    let (cmd_tx, _worker_handle) = spawn_emulator_worker(Arc::clone(&shared));

    let mut image = Image {
        bytes: vec![0; SCREEN_WIDTH * SCREEN_HEIGHT * 4],
        width: SCREEN_WIDTH as u16,
        height: SCREEN_HEIGHT as u16,
    };
    let texture = Texture2D::from_image(&image);
    texture.set_filter(FilterMode::Nearest);

    let default_nes_dir = PathBuf::from("/home/zajones/Projects/NES");
    let initial_dir = if default_nes_dir.is_dir() {
        default_nes_dir
    } else {
        env::current_dir()
            .ok()
            .and_then(|d| {
                if d.ends_with("nes-rt-emu") {
                    d.parent().map(Path::to_path_buf)
                } else {
                    Some(d)
                }
            })
            .unwrap_or_else(|| PathBuf::from("."))
    };

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
    if let Some(arg_path) = env::args().nth(1) {
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

    loop {
        let now = get_time();
        let dt = (now - last_time).clamp(0.0, 0.1);
        last_time = now;
        if toast_timer > 0.0 {
            toast_timer = (toast_timer - dt).max(0.0);
        }

        // Global Hotkeys
        if is_key_pressed(KeyCode::F1) || is_key_pressed(KeyCode::Escape) {
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
                "Scheduling: PREEMPT_RT / SCHED_FIFO Enabled".to_string()
            } else {
                "Scheduling: Standard (SCHED_OTHER)".to_string()
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

        if is_key_pressed(KeyCode::F5) && shared.rom_loaded.load(Ordering::Acquire) {
            let _ = cmd_tx.send(EmulatorCommand::SaveState);
            toast_text = "State Saved (Slot 1)".to_string();
            toast_timer = 2.0;
        }

        if is_key_pressed(KeyCode::F8) && shared.rom_loaded.load(Ordering::Acquire) {
            let _ = cmd_tx.send(EmulatorCommand::LoadState);
            toast_text = "State Loaded (Slot 1)".to_string();
            toast_timer = 2.0;
        }

        if is_key_pressed(KeyCode::F6) {
            crop_overscan = !crop_overscan;
            toast_text = format!("Overscan crop: {}", if crop_overscan { "ON" } else { "OFF" });
            toast_timer = 1.5;
        }

        if show_browser {
            // Pause controller input to the game while browsing files
            shared.update_input(0, 0);

            let n = browser.entries.len();
            if rep_up.fire(is_key_down(KeyCode::Up), dt) && browser.selected > 0 {
                browser.selected -= 1;
            }
            if rep_down.fire(is_key_down(KeyCode::Down), dt) && browser.selected + 1 < n {
                browser.selected += 1;
            }
            if rep_pgup.fire(is_key_down(KeyCode::PageUp) || is_key_down(KeyCode::Left), dt) {
                browser.selected = browser.selected.saturating_sub(20);
            }
            if rep_pgdn.fire(is_key_down(KeyCode::PageDown) || is_key_down(KeyCode::Right), dt) && n > 0 {
                browser.selected = (browser.selected + 20).min(n - 1);
            }
            if is_key_pressed(KeyCode::Home) {
                browser.selected = 0;
            }
            if is_key_pressed(KeyCode::End) && !browser.entries.is_empty() {
                browser.selected = browser.entries.len() - 1;
            }
            if is_key_pressed(KeyCode::Backspace)
                && let Some(parent) = browser.current_dir.parent() {
                    browser.current_dir = parent.to_path_buf();
                    browser.selected = 0;
                    browser.refresh();
                }
            if is_key_pressed(KeyCode::Enter)
                && let Some(item) = browser.entries.get(browser.selected).cloned() {
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
            let held = poll_joypad_buttons();
            let pressed = poll_joypad_pressed();
            shared.update_input(held, pressed);
        }

        if let Some(latest_fb) = shared.framebuffer.acquire_latest() {
            image.bytes.copy_from_slice(latest_fb);
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
            let aspect = src.w / src.h;
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
                "NES-RT ROM Launcher & Manager",
                panel_x + 18.0,
                panel_y + 28.0,
                24.0,
                Color::new(0.55, 0.80, 1.0, 1.0),
            );
            draw_text(
                &fit_text(
                    &format!(
                        "{}  |  Kernel {} (PREEMPT_RT={}, RTPRIO={})",
                        current_rom_info, shared.caps.kernel_release, shared.caps.preempt_rt_kernel, shared.caps.max_rtprio
                    ),
                    text_w,
                    16,
                ),
                panel_x + 18.0,
                panel_y + 50.0,
                16.0,
                LIGHTGRAY,
            );
            draw_text(
                &fit_text(&format!("Directory: {}", browser.current_dir.display()), text_w, 16),
                panel_x + 18.0,
                panel_y + 72.0,
                16.0,
                GRAY,
            );

            let list_top = panel_y + 92.0;
            let row_h = 24.0;
            let max_visible = ((panel_h - 160.0) / row_h).max(1.0) as usize;
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
                footer_y - 40.0,
                16.0,
                Color::new(1.0, 0.85, 0.55, 1.0),
            );
            let help = Color::new(0.70, 0.85, 0.95, 1.0);
            draw_text(
                &fit_text("Up/Down (hold to scroll)  Left/Right/PgUp/PgDn: Page  Home/End  Enter: Load  Backspace: Up Dir", text_w, 15),
                panel_x + 18.0,
                footer_y - 18.0,
                15.0,
                help,
            );
            draw_text(
                &fit_text("Esc/F1: Back to game  F2: Unload  F3: RT sched  F4: Overclock  F5/F8: Save/Load state  F6: Crop  R: Reset", text_w, 15),
                panel_x + 18.0,
                footer_y,
                15.0,
                help,
            );
        }

        next_frame().await;
    }
}

