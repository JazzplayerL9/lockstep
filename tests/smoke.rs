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
