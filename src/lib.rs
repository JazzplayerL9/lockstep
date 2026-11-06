// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones

//! # Lockstep core
//!
//! A cycle-accurate NES / Famicom emulation library, plus the real-time machinery that
//! drives it with minimal input-to-photon latency. It powers both binaries in this crate:
//! the `lockstep` GUI and the `lockstep-headless` test runner.
//!
//! ## Design in one paragraph
//!
//! The CPU is the scheduler. Every CPU bus access is exactly one CPU cycle; around each
//! access the PPU is advanced to a shared *master clock* (12 master clocks per CPU cycle,
//! 4 per PPU dot) and the APU is clocked once. Because the CPU, PPU, APU and cartridge
//! all move in lockstep, mid-scanline register writes, sprite-0 polling loops, MMC3
//! scanline IRQs and DMA cycle stealing behave as on hardware without per-game hacks.
//! On top of that, [`rt`] locks emulated frames to the host display's refresh and starts
//! each one just in time, [`audio`] keeps sound in sync with dynamic rate control, and
//! [`cpu::System::run_frame_ahead`] can hide a game's own input lag ("run-ahead").
//!
//! ## Modules
//!
//! | Module | Contents |
//! |---|---|
//! | [`cpu`] | [`cpu::System`]: the 2A03 CPU, system bus, OAM/DMC DMA, and the whole machine |
//! | [`ppu`] | The 2C02 picture processor, emulated dot by dot |
//! | [`apu`] | The 2A03 sound channels, frame counter and mixer |
//! | [`mappers`] | Cartridge boards (NROM, MMC1, MMC3, MMC5, ...) behind the [`mappers::Mapper`] trait |
//! | [`cartridge`] | iNES / NES 2.0 / zip / headerless ROM loading, header fixups, battery saves |
//! | [`input`] | Standard controllers |
//! | [`palette`] | NTSC palette and colour emphasis |
//! | [`rt`] | Real-time scheduling, display sync, frame hand-off to the UI |
//! | [`audio`] | Low-latency audio output with dynamic rate control |
//! | [`png`] | Minimal PNG writer for screenshots |
//!
//! ## Example: run a game headlessly
//!
//! ```no_run
//! use lockstep_core::cartridge::Cartridge;
//! use lockstep_core::cpu::System;
//! use lockstep_core::input::BTN_START;
//!
//! let cart = Cartridge::from_file("game.nes").expect("readable iNES file");
//! let mut nes = System::new(cart).expect("supported mapper");
//! for frame in 0..600 {
//!     nes.joypad.buttons_p1 = if frame % 120 < 5 { BTN_START } else { 0 };
//!     nes.run_frame();
//!     let _rgba: &[u8] = &nes.ppu.framebuffer; // 256x240 RGBA
//!     nes.apu.audio_samples.clear(); // mono f32 samples, if you want them
//! }
//! nes.save_sram(); // persist battery-backed RAM, if the cartridge has any
//! ```
//!
//! See `README.md` and the `docs/` directory for architecture, real-time tuning,
//! accuracy status and the supported mapper list.

#![warn(missing_docs)]
#![warn(clippy::missing_docs_in_private_items)]

pub mod apu;
pub mod audio;
pub mod cartridge;
pub mod cpu;
pub mod input;
pub mod mappers;
pub mod palette;
pub mod png;
pub mod ppu;
pub mod rt;
