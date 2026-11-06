//! Smoke tests against ROMs in the local library (skipped if the files are absent).

use nes_core::cartridge::Cartridge;
use nes_core::cpu::System;
use nes_core::input::{BTN_A, BTN_START};

const NES_DIR: &str = "/home/zajones/Projects/NES";

fn load(name: &str) -> Option<System> {
    let path = format!("{NES_DIR}/{name}");
    if !std::path::Path::new(&path).exists() {
        eprintln!("skipping: {path} not found");
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
