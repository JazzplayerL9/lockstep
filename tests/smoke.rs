// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones

//! Smoke tests against commercial ROMs from a local library. ROMs are not distributed with
//! Lockstep; point `LOCKSTEP_ROM_DIR` at your own dumps. Missing ROMs are skipped.

use lockstep_core::cartridge::Cartridge;
use lockstep_core::cpu::System;
use lockstep_core::input::{BTN_A, BTN_START};

/// ROM library location: `$LOCKSTEP_ROM_DIR`, else `../NES` next to the crate.
fn rom_dir() -> std::path::PathBuf {
    std::env::var_os("LOCKSTEP_ROM_DIR")
        .map(Into::into)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../NES"))
}

fn load(name: &str) -> Option<System> {
    let path = rom_dir().join(name);
    if !path.exists() {
        eprintln!("skipping: {} not found", path.display());
        return None;
    }
    Some(System::new(Cartridge::from_file(&path).expect("load")).expect("mapper"))
}

fn run(sys: &mut System, frames: u32) {
    for f in 0..frames {
        sys.joypad.buttons_p1 = if f % 60 < 3 { BTN_START } else if f % 60 == 30 { BTN_A } else { 0 };
        sys.run_frame();
        assert!(!sys.jammed, "CPU jammed at frame {f}, pc={:04X}", sys.pc);
    }
}

#[test]
fn boots_one_game_per_mapper() {
    for name in [
        "1942 (Japan, USA).zip",
        "Abadox (USA).zip",
        "1943 - The Battle of Midway (USA).zip",
        "Arkanoid (USA).zip",
        "8 Eyes (USA).zip",
        "Castlevania III - Dracula's Curse (USA).zip",
        "Battletoads (USA).zip",
        "Mike Tyson's Punch-Out!! (USA).zip",
        "Batman - Return of the Joker (USA).zip",
    ] {
        if let Some(mut sys) = load(name) {
            run(&mut sys, 300);
        }
    }
}

#[test]
fn save_state_clone_is_deterministic() {
    let Some(mut sys) = load("Kirby's Adventure (USA).zip") else { return };
    run(&mut sys, 200);
    let mut snap = sys.clone();
    run(&mut sys, 120);
    run(&mut snap, 120);
    assert_eq!(sys.ppu.framebuffer[..], snap.ppu.framebuffer[..]);
    assert_eq!(sys.ram, snap.ram);
}

#[test]
fn overclock_mode_runs_games() {
    for name in ["Kirby's Adventure (USA).zip", "Super Mario Bros. 3 (USA).zip", "Battletoads (USA).zip"] {
        if let Some(mut sys) = load(name) {
            sys.set_overclock(true);
            run(&mut sys, 600);
        }
    }
}

#[test]
fn run_ahead_shows_the_next_frame_early() {
    let Some(base) = load("Super Mario Bros. (Japan, USA).zip") else { return };
    let input = |f: u32| if (100..104).contains(&f) { BTN_START } else if f > 200 { lockstep_core::input::BTN_RIGHT | if f % 40 < 15 { BTN_A } else { 0 } } else { 0 };
    // Reference: plain emulation, recording every frame.
    let mut plain = base.clone();
    let mut frames = Vec::new();
    for f in 0..400 {
        plain.joypad.buttons_p1 = input(f);
        plain.run_frame();
        frames.push(plain.ppu.framebuffer.to_vec());
    }
    // Run-ahead 1 with input held constant across the speculative frame: the displayed
    // image for frame f must equal reference frame f+1 whenever input(f) == input(f+1).
    let mut ra = base.clone();
    let mut out = vec![0u8; frames[0].len()];
    let mut checked = 0;
    for f in 0..399 {
        ra.joypad.buttons_p1 = input(f);
        ra.run_frame_ahead(1, &mut out);
        assert_eq!(ra.ppu.framebuffer[..], frames[f as usize][..], "real state diverged at {f}");
        if input(f) == input(f + 1) {
            assert!(out == frames[f as usize + 1], "run-ahead frame mismatch at {f}");
            checked += 1;
        }
    }
    assert!(checked > 300);
}

#[test]
fn smb_duck_hunt_zapper_and_disk_save_state() {
    let Some(mut sys) = load("Super Mario Bros. + Duck Hunt (USA).zip") else { return };
    assert!(sys.joypad.zapper_connected, "SMB + Duck Hunt should auto-connect Zapper");

    // Frame 0..600: on SMB + Duck Hunt multicart menu, press Select once at frame 60 to move
    // cursor to DUCK HUNT, press Start at frame 90 to boot Duck Hunt, press Start at frame 180
    // to start Game A (1 Duck), and wait until frame 500 when the first duck is flying!
    // Frame 0..550: on SMB + Duck Hunt multicart menu, press Down at frame 20 to select
    // DUCK HUNT, press Start at frame 50 to boot Duck Hunt, press Start at frame 150
    // to start Game A (1 Duck), and wait until frame 550 when the first duck is flying!
    for f in 0..680 {
        sys.joypad.buttons_p1 = match f {
            20..=25 => lockstep_core::input::BTN_DOWN,
            50..=55 => BTN_START,
            150..=155 => BTN_START,
            _ => 0,
        };
        sys.run_frame();
        assert!(!sys.jammed);
    }
    assert_eq!(sys.mapper.base().prg[1].offset, 0x8000, "Should be running Duck Hunt PRG bank");
    assert_eq!(sys.mapper.base().chr[0].1, 0x2000, "Should be running Duck Hunt CHR bank");

    // Verify disk save state round-trip on SMB + Duck Hunt (Mapper 66 / GxROM).
    let state_bytes = sys.to_state_bytes();
    let mut clone_sys = sys.clone();

    // Probe where Duck Hunt draws the white target hitbox when the trigger is pulled on this frame.
    let mut probe = sys.clone();
    probe.joypad.set_zapper(-1, -1, 2);
    let mut target_center = None;
    for _ in 0..10 {
        probe.run_frame();
        // On the hitbox frame, the sky (top 120 scanlines) is black [0, 0, 0] except for the white target box [236, 238, 236].
        let mut black_sky = 0usize;
        let mut sum_x = 0usize;
        let mut sum_y = 0usize;
        let mut white_count = 0usize;
        for y in 16..150 {
            for x in 8..248 {
                let idx = (y * 256 + x) * 4;
                let (r, g, b) = (
                    probe.ppu.framebuffer[idx],
                    probe.ppu.framebuffer[idx + 1],
                    probe.ppu.framebuffer[idx + 2],
                );
                if r == 0 && g == 0 && b == 0 {
                    black_sky += 1;
                } else if r == 236 && g == 238 && b == 236 {
                    sum_x += x;
                    sum_y += y;
                    white_count += 1;
                }
            }
        }
        if black_sky > 20_000 && white_count > 50 {
            target_center = Some(((sum_x / white_count) as i32, (sum_y / white_count) as i32));
            break;
        }
    }
    let (tx, ty) = target_center.expect("Duck Hunt should draw a white target box on a black sky when trigger is pulled");

    // Now fire the Zapper aimed at the duck's target box `(tx, ty)` vs aimed off-screen `(-1, -1)`.
    let mut miss_sys = sys.clone();
    sys.joypad.set_zapper(tx, ty, 2);
    miss_sys.joypad.set_zapper(-1, -1, 2);
    for _ in 0..30 {
        sys.run_frame();
        miss_sys.run_frame();
    }
    assert_ne!(
        &sys.ppu.framebuffer[..],
        &miss_sys.ppu.framebuffer[..],
        "Zapper hit at ({tx}, {ty}) should shoot the duck down vs off-screen miss"
    );

    // And restoring `state_bytes` and firing at `(tx, ty)` must produce the exact same hit result.
    clone_sys.load_state_bytes(&state_bytes).expect("load_state_bytes");
    clone_sys.joypad.set_zapper(tx, ty, 2);
    for _ in 0..30 {
        clone_sys.run_frame();
    }
    assert_eq!(&clone_sys.ppu.framebuffer[..], &sys.ppu.framebuffer[..]);
    assert_eq!(clone_sys.ram, sys.ram);
}
