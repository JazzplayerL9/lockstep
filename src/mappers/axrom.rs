// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones

//! Mapper 7: AxROM (32 KB PRG switching + one-screen mirroring select).

use super::{CartBase, Mapper};
use crate::cartridge::Mirroring;

/// AxROM board (ANROM/AMROM/AOROM): one register at $8000-$FFFF. Bits 0-3 select the
/// 32 KB PRG-ROM bank at $8000 (bits 0-2 on real boards; the extra bit allows
/// oversize ROMs), bit 4 selects the one-screen nametable page (0 = lower CIRAM
/// page, 1 = upper). CHR is 8 KB of unbanked RAM. Bus conflicts are not modelled.
#[derive(Clone)]
pub struct AxRom {
    /// Shared PRG/CHR/nametable state; PRG bank and mirroring are written straight into it.
    base: CartBase,
}

impl AxRom {
    /// Maps PRG bank 0 at $8000-$FFFF and selects the lower one-screen page, the
    /// power-on state the emulator assumes (real hardware is unspecified).
    pub fn new(mut base: CartBase) -> Self {
        base.set_prg_32k(0);
        base.set_mirroring(Mirroring::OneScreenLower);
        Self { base }
    }
}

impl Mapper for AxRom {
    fn base(&self) -> &CartBase { &self.base }
    fn base_mut(&mut self) -> &mut CartBase { &mut self.base }
    fn name(&self) -> &'static str { "AxROM (7)" }

    fn write_register(&mut self, _addr: u16, val: u8, _cycle: u64) {
        self.base.set_prg_32k((val & 0x0F) as i32);
        self.base.set_mirroring(if val & 0x10 != 0 { Mirroring::OneScreenUpper } else { Mirroring::OneScreenLower });
    }
}
