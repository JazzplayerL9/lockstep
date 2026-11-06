// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones

//! Mapper 2: UxROM (UNROM/UOROM), and mapper 71: Camerica BF909x.

use super::{CartBase, Mapper};
use crate::cartridge::Mirroring;

/// UxROM board: switchable 16 KB PRG-ROM bank at $8000-$BFFF selected by a latch at
/// $8000-$FFFF, last 16 KB bank fixed at $C000-$FFFF, 8 KB CHR-RAM, hardwired
/// mirroring. Also implements the Camerica/Codemasters BF909x (mapper 71), whose PRG
/// latch is at $C000-$FFFF and which has no bus conflicts.
#[derive(Clone)]
pub struct UxRom {
    /// Shared PRG/CHR/nametable state; the PRG latch is applied to PRG 16 KB slot 0.
    base: CartBase,
    /// Whether register writes are ANDed with the PRG-ROM byte at the written address
    /// (bus conflict, as on UNROM/UOROM without a decoder). Ignored for Camerica.
    bus_conflicts: bool,
    /// Mapper 71 mode: writes below $C000 never touch the PRG bank; writes to
    /// $9000-$BFFF set one-screen mirroring from bit 4 (BF9097, used by Fire Hawk).
    camerica: bool,
}

impl UxRom {
    /// Maps PRG bank 0 at $8000 and the last bank at $C000. `bus_conflicts` selects
    /// whether latch writes are ANDed with the ROM contents.
    pub fn new(mut base: CartBase, bus_conflicts: bool) -> Self {
        base.set_prg_16k(0, 0);
        base.set_prg_16k(1, -1);
        Self { base, bus_conflicts, camerica: false }
    }

    /// Builds the mapper 71 (Camerica BF909x) variant: same banking, no bus conflicts,
    /// PRG latch at $C000-$FFFF and the Fire Hawk mirroring register below it.
    pub fn new_camerica(base: CartBase) -> Self {
        let mut m = Self::new(base, false);
        m.camerica = true;
        m
    }
}

impl Mapper for UxRom {
    fn base(&self) -> &CartBase { &self.base }
    fn base_mut(&mut self) -> &mut CartBase { &mut self.base }
    fn name(&self) -> &'static str { if self.camerica { "Camerica (71)" } else { "UxROM (2)" } }

    fn write_register(&mut self, addr: u16, mut val: u8, _cycle: u64) {
        if self.camerica {
            if addr < 0xC000 {
                // Fire Hawk one-screen mirroring control.
                if addr >= 0x9000 {
                    self.base.set_mirroring(if val & 0x10 != 0 { Mirroring::OneScreenUpper } else { Mirroring::OneScreenLower });
                }
                return;
            }
        } else if self.bus_conflicts {
            val &= self.base.read_prg(addr).unwrap_or(0xFF);
        }
        self.base.set_prg_16k(0, val as i32);
    }
}
