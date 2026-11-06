//! NES emulation core (cycle-accurate CPU/PPU/APU + cartridge mappers), shared by the
//! macroquad front end and the headless test runner.

pub mod apu;
pub mod cartridge;
pub mod cpu;
pub mod input;
pub mod mappers;
pub mod palette;
pub mod png;
pub mod ppu;
pub mod rt;
