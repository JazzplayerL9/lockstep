mod apu;
mod cartridge;
mod cpu;
mod input;
mod mapper;
mod palette;
mod ppu;
pub mod rt;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use cartridge::Cartridge;
use input::{BTN_A, BTN_B, BTN_DOWN, BTN_LEFT, BTN_RIGHT, BTN_SELECT, BTN_START, BTN_UP};
use macroquad::prelude::*;
use mapper::Mapper;
use ppu::{SCREEN_HEIGHT, SCREEN_WIDTH};
use rt::{EmulatorCommand, RtSharedState, spawn_emulator_worker};

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
    let mut show_hud = true;
    // Crop 8 px of NTSC overscan on every edge (hides the PPU left-column mask and
    // top/bottom garbage lines that CRTs never displayed). Toggle with F6.
    let mut crop_overscan = true;
    let mut current_rom_info = String::from("No ROM Loaded");
    let mut toast_text = String::new();
    let mut toast_timer = 0.0f64;

    // If a .nes file path was passed on the command line, load it immediately
    if let Some(arg_path) = env::args().nth(1) {
        match Cartridge::from_file(&arg_path) {
            Ok(cart) => {
                let mapper_probe = Mapper::from_cartridge(cart.clone());
                current_rom_info = format!("{} [{}]", cart.title, mapper_probe.mapper_name());
                let _ = cmd_tx.send(EmulatorCommand::LoadRom(cart));
                show_browser = false;
                toast_text = format!("Loaded {}", current_rom_info);
                toast_timer = 3.0;
            }
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
                "Scanline-240 6502 Overclock: ON (No Lag Frames)".to_string()
            } else {
                "Scanline-240 6502 Overclock: OFF (Stock 1.79 MHz)".to_string()
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

        if is_key_pressed(KeyCode::F10) {
            show_hud = !show_hud;
        }

        if is_key_pressed(KeyCode::F6) {
            crop_overscan = !crop_overscan;
            toast_text = format!("Overscan crop: {}", if crop_overscan { "ON" } else { "OFF" });
            toast_timer = 1.5;
        }

        if show_browser {
            // Pause controller input to the game while browsing files
            shared.update_input(0, 0);

            if is_key_pressed(KeyCode::Up) && browser.selected > 0 {
                browser.selected -= 1;
            }
            if is_key_pressed(KeyCode::Down)
                && browser.selected + 1 < browser.entries.len()
            {
                browser.selected += 1;
            }
            if is_key_pressed(KeyCode::PageUp) || is_key_pressed(KeyCode::Left) {
                browser.selected = browser.selected.saturating_sub(20);
            }
            if (is_key_pressed(KeyCode::PageDown) || is_key_pressed(KeyCode::Right))
                && !browser.entries.is_empty() {
                    browser.selected = (browser.selected + 20).min(browser.entries.len() - 1);
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
                            Ok(cart) => {
                                let mapper_probe = Mapper::from_cartridge(cart.clone());
                                current_rom_info =
                                    format!("{} [{}]", cart.title, mapper_probe.mapper_name());
                                let _ = cmd_tx.send(EmulatorCommand::LoadRom(cart));
                                show_browser = false;
                                toast_text = format!("Loaded {}", current_rom_info);
                                toast_timer = 3.0;
                            }
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

        // Draw top telemetry HUD if enabled or if toast is active
        if show_hud || toast_timer > 0.0 {
            draw_rectangle(0.0, 0.0, sw, 26.0, Color::new(0.05, 0.07, 0.12, 0.85));
            let rt_on = shared.rt_mode_enabled.load(Ordering::Acquire);
            let prio = shared.sched_fifo_prio.load(Ordering::Acquire);
            let oc_on = shared.overclock_enabled.load(Ordering::Acquire);
            let max_jit_us = (shared.max_abs_jitter_ns.load(Ordering::Relaxed) as f64) / 1000.0;

            let sched_badge = if rt_on && prio > 0 {
                format!(
                    "RT: SCHED_FIFO({}) [PREEMPT_RT={}]",
                    prio, shared.caps.preempt_rt_kernel
                )
            } else if rt_on {
                format!(
                    "RT: TimerSlack=1ns [PREEMPT_RT={}]",
                    shared.caps.preempt_rt_kernel
                )
            } else {
                "Std: SCHED_OTHER".to_string()
            };

            let status_line = if toast_timer > 0.0 {
                format!("{}  |  {}", toast_text, sched_badge)
            } else {
                format!(
                    "{} | {} | MaxJitter: {:.2} us | 6502-OC: {} | F1:ROMs F2:Unload F3:RT F4:OC F5/F8:State F6:Crop F10:HUD",
                    current_rom_info,
                    sched_badge,
                    max_jit_us,
                    if oc_on { "ON" } else { "OFF" }
                )
            };
            draw_text(&status_line, 10.0, 18.0, 16.0, WHITE);
        }

        // Draw ROM Browser Overlay when active
        if show_browser {
            let panel_x = 40.0;
            let panel_y = 44.0;
            let panel_w = (sw - 80.0).max(320.0);
            let panel_h = (sh - 70.0).max(260.0);

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
                format!(
                    "Kernel: {} (PREEMPT_RT={}) | RLIMIT_RTPRIO={}",
                    shared.caps.kernel_release,
                    shared.caps.preempt_rt_kernel,
                    shared.caps.max_rtprio
                ),
                panel_x + 18.0,
                panel_y + 50.0,
                16.0,
                LIGHTGRAY,
            );
            draw_text(
                format!("Directory: {}", browser.current_dir.display()),
                panel_x + 18.0,
                panel_y + 72.0,
                16.0,
                GRAY,
            );

            let list_top = panel_y + 92.0;
            let row_h = 24.0;
            let max_visible = ((panel_h - 140.0) / row_h).max(1.0) as usize;
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
                draw_text(&entry.name, panel_x + 22.0, y, 18.0, color);
            }

            let footer_y = panel_y + panel_h - 16.0;
            draw_text(
                "Up/Down: Select | Left/Right/PgUp/PgDn: Page | Enter: Load .nes/.zip | Backspace: Up Dir | F2: Unload | F3: RT | F4: OC",
                panel_x + 18.0,
                footer_y,
                15.0,
                Color::new(0.70, 0.85, 0.95, 1.0),
            );
        }

        next_frame().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rt::{MonotonicTimer, NTSC_FRAME_NS};

    /// Helper to build a synthetic iNES ROM image for any mapper ID.
    fn build_synthetic_ines(mapper_id: u8, prg_banks_16k: u8, chr_banks_8k: u8) -> Vec<u8> {
        let mut rom = Vec::new();
        rom.extend_from_slice(b"NES\x1A");
        rom.push(prg_banks_16k);
        rom.push(chr_banks_8k);
        let flags6 = (mapper_id & 0x0F) << 4;
        let flags7 = mapper_id & 0xF0;
        rom.push(flags6);
        rom.push(flags7);
        rom.extend_from_slice(&[0u8; 8]);

        let prg_size = (prg_banks_16k as usize) * 16384;
        let mut prg = vec![0xEAu8; prg_size]; // Fill with NOPs

        // Populate every 16 KB bank's upper 8 KB ($E000-$FFFF when mapped at $C000-$FFFF)
        // so mappers that boot in bank 0 (like AxROM 32 KB mode) have valid vectors at $FFFA-$FFFF.
        for bank in 0..(prg_banks_16k as usize) {
            let bank_end = (bank + 1) * 16384;
            let code_off = bank_end - 0x2000;
            let prog: [u8; 12] = [
                0xA9, 0x42, // LDA #$42
                0x85, 0x00, // STA $00
                0xA9, 0x80, // LDA #$80 (enable NMI)
                0x8D, 0x00, 0x20, // STA $2000
                0xE6, 0x01, // INC $01
                0x4C, // JMP $E009
            ];
            prg[code_off..code_off + 12].copy_from_slice(&prog);
            prg[code_off + 12] = 0x09;
            prg[code_off + 13] = 0xE0;

            let nmi_off = bank_end - 0x1F00;
            prg[nmi_off] = 0xE6;
            prg[nmi_off + 1] = 0x02;
            prg[nmi_off + 2] = 0x40;

            prg[bank_end - 6] = 0x00;
            prg[bank_end - 5] = 0xE1;
            prg[bank_end - 4] = 0x00;
            prg[bank_end - 3] = 0xE0;
            prg[bank_end - 2] = 0x00;
            prg[bank_end - 1] = 0xE1;
        }

        rom.extend_from_slice(&prg);
        rom.extend(vec![0u8; (chr_banks_8k as usize) * 8192]);
        rom
    }

    #[test]
    fn test_multi_mapper_loading_nrom_mmc1_uxrom_cnrom_mmc3_axrom() {
        for &(mapper_id, prg_banks, chr_banks) in &[
            (0u8, 2u8, 1u8), // Mapper 0: NROM
            (1u8, 8u8, 2u8), // Mapper 1: MMC1
            (2u8, 8u8, 0u8), // Mapper 2: UxROM (CHR-RAM)
            (3u8, 2u8, 4u8), // Mapper 3: CNROM
            (4u8, 8u8, 8u8), // Mapper 4: MMC3
            (7u8, 4u8, 0u8), // Mapper 7: AxROM (CHR-RAM)
        ] {
            let bytes = build_synthetic_ines(mapper_id, prg_banks, chr_banks);
            let cart =
                Cartridge::from_ines_bytes(&bytes, format!("TestMapper{}", mapper_id)).unwrap();
            assert_eq!(cart.mapper_id, mapper_id);
            let mut sys = cpu::System::from_cartridge(cart);
        sys.overclock_enabled = false;
            for _ in 0..5 {
                sys.run_frame();
            }
            assert_eq!(
                sys.ram[0x0000], 0x42,
                "Mapper {} failed to execute reset vector",
                mapper_id
            );
            assert!(
                sys.ram[0x0002] >= 3,
                "Mapper {} failed to fire VBlank NMI",
                mapper_id
            );
        }
    }

    #[test]
    fn test_rt_detection_mode_switching_and_rom_load_unload() {
        let shared = Arc::new(RtSharedState::new());
        let (cmd_tx, handle) = spawn_emulator_worker(Arc::clone(&shared));

        // Load real Abadox ROM into the worker
        let cart = Cartridge::from_file("/home/zajones/Projects/Abadox (USA).nes")
            .expect("Should load Abadox (USA).nes");
        cmd_tx.send(EmulatorCommand::LoadRom(cart)).unwrap();

        // Let worker run ~8 frames in RT mode
        let t1 = MonotonicTimer::now_ns() + NTSC_FRAME_NS * 8;
        let _ = MonotonicTimer::sleep_until_ns(t1, true);
        assert!(shared.rom_loaded.load(Ordering::Acquire));
        assert!(shared.frames_executed.load(Ordering::Acquire) >= 5);

        let rt_jitter_ns = shared.max_abs_jitter_ns.load(Ordering::Relaxed);

        // Test SaveState / LoadState and live switch to Standard Mode (SCHED_OTHER)
        cmd_tx.send(EmulatorCommand::SaveState).unwrap();
        cmd_tx.send(EmulatorCommand::SetRtMode(false)).unwrap();
        let t2 = MonotonicTimer::now_ns() + NTSC_FRAME_NS * 4;
        let _ = MonotonicTimer::sleep_until_ns(t2, false);
        assert!(!shared.rt_mode_enabled.load(Ordering::Acquire));

        // Switch back to RT mode and Unload ROM
        cmd_tx.send(EmulatorCommand::SetRtMode(true)).unwrap();
        cmd_tx.send(EmulatorCommand::LoadState).unwrap();
        cmd_tx.send(EmulatorCommand::UnloadRom).unwrap();
        let t3 = MonotonicTimer::now_ns() + NTSC_FRAME_NS * 2;
        let _ = MonotonicTimer::sleep_until_ns(t3, true);
        assert!(!shared.rom_loaded.load(Ordering::Acquire));

        shared.running.store(false, Ordering::Release);
        handle.join().unwrap();

        println!(
            "NES-RT Worker Verified -> PREEMPT_RT={}, max_rtprio={}, rt_jitter={} ns ({:.2} us)",
            shared.caps.preempt_rt_kernel,
            shared.caps.max_rtprio,
            rt_jitter_ns,
            (rt_jitter_ns as f64) / 1000.0
        );
    }

    #[test]
    fn test_load_zip_roms_from_nes_directory() {
        let zip_samples = [
            ("/home/zajones/Projects/NES/1942 (Japan, USA).zip", 0u8),
            ("/home/zajones/Projects/NES/Abadox (USA).zip", 1u8),
            ("/home/zajones/Projects/NES/1943 - The Battle of Midway (USA).zip", 2u8),
            ("/home/zajones/Projects/NES/Arkanoid (USA).zip", 3u8),
            ("/home/zajones/Projects/NES/8 Eyes (USA).zip", 4u8),
        ];

        for &(zip_path, expected_mapper) in &zip_samples {
            let cart = Cartridge::from_file(zip_path)
                .unwrap_or_else(|e| panic!("Failed to load ZIP ROM '{}': {}", zip_path, e));
            assert_eq!(cart.mapper_id, expected_mapper);
            let mut sys = cpu::System::from_cartridge(cart);
        sys.overclock_enabled = false;
            for _ in 0..30 {
                sys.run_frame();
            }
        }
    }

    #[test]
    fn test_battletoads_double_dragon_stage1() {
        let zip_path =
            "/home/zajones/Projects/NES/Battletoads & Double Dragon - The Ultimate Team (USA).zip";
        for &overclock in &[true, false] {
            let cart = Cartridge::from_file(zip_path).expect("Load Battletoads & Double Dragon");
            let mut sys = cpu::System::from_cartridge(cart);
            sys.overclock_enabled = overclock;

            // Run 2000 frames through title, character select, and Stage 1 combat
            for frame in 0..2000 {
                sys.joypad.buttons_p1 = if frame % 20 == 0 {
                    BTN_START
                } else if frame % 20 == 10 {
                    BTN_A
                } else if frame > 700 && (frame % 4 < 2) {
                    BTN_RIGHT | BTN_B
                } else {
                    0
                };
                sys.run_frame();
                assert!(
                    !(0x8180..=0x8194).contains(&sys.pc),
                    "Battletoads & Double Dragon froze waiting for Sprite 0 Hit at frame {} (overclock={}): PC=${:04X}",
                    frame,
                    overclock,
                    sys.pc
                );
            }
        }
    }

    #[test]
    fn test_kirby_adventure_status_bar_stability() {
        let zip_path = "/home/zajones/Projects/NES/Kirby's Adventure (USA).zip";
        let cart = Cartridge::from_file(zip_path).expect("Load Kirby's Adventure");
        let mut sys = cpu::System::from_cartridge(cart);
        sys.overclock_enabled = false;

        for frame in 0..900 {
            sys.joypad.buttons_p1 = if frame > 120 && frame % 16 == 0 {
                BTN_START
            } else if frame > 300 && frame % 16 == 8 {
                BTN_A
            } else {
                0
            };
            sys.run_frame();
        }

        // The status-bar split IRQ writes $2006 at $FB52 near dot 256 of scanline 159.
        // The resulting VRAM address (sampled at $FB55) must be identical every frame,
        // otherwise the status bar jitters up/down by one scanline.
        let mut seen_v: Vec<u16> = Vec::new();
        for frame in 900..1100 {
            sys.joypad.buttons_p1 = if frame % 8 < 4 {
                crate::input::BTN_RIGHT | crate::input::BTN_UP
            } else {
                crate::input::BTN_LEFT | crate::input::BTN_B
            };
            sys.ppu.frame_complete = false;
            while !sys.ppu.frame_complete {
                if sys.pc == 0xFB55 && !seen_v.contains(&sys.ppu.v) {
                    seen_v.push(sys.ppu.v);
                }
                sys.step_instruction();
            }
        }
        println!("Kirby status-bar split v values: {:04X?}", seen_v);
        println!("Kirby $2001 mask at end of frame: {:08b}, backdrop=${:02X}", sys.ppu.mask, sys.ppu.palette[0]);
        assert_eq!(seen_v.len(), 1, "Status bar scroll split is unstable: {:04X?}", seen_v);
    }

    /// Smoke test: boot every ROM in the library for 300 frames (run with --ignored).
    #[test]
    #[ignore]
    fn smoke_all_roms_in_library() {
        let mut failures = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir("/home/zajones/Projects/NES")
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        entries.sort();
        for path in entries {
            let p = path.to_string_lossy().to_string();
            let r = std::panic::catch_unwind(|| {
                if let Ok(cart) = Cartridge::from_file(&p) {
                    let mut sys = cpu::System::from_cartridge(cart);
                    for f in 0..300 {
                        sys.joypad.buttons_p1 = if f % 30 == 0 { BTN_START } else { 0 };
                        sys.run_frame();
                    }
                }
            });
            if r.is_err() {
                failures.push(p);
            }
        }
        println!("ROM smoke failures: {}", failures.len());
        for f in &failures {
            println!("  FAIL {}", f);
        }
        assert!(failures.is_empty());
    }
}
