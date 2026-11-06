// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones

//! Mapper 3: CNROM (8 KB CHR bank switching, with bus conflicts).

use super::{CartBase, Mapper};

/// CNROM board: fixed 16/32 KB PRG-ROM and a single CHR latch at $8000-$FFFF that
/// selects the 8 KB CHR-ROM bank. The register is not decoded from PRG-ROM, so the
/// written value is ANDed with the ROM byte at the same address (bus conflict).
#[derive(Clone)]
pub struct CnRom {
    /// Shared PRG/CHR/nametable state; the CHR latch is applied directly to its CHR slots.
    base: CartBase,
}

impl CnRom {
    /// Wraps `base`, keeping the default power-on mapping (CHR bank 0).
    pub fn new(base: CartBase) -> Self {
        Self { base }
    }
}

impl Mapper for CnRom {
    fn base(&self) -> &CartBase { &self.base }
    fn base_mut(&mut self) -> &mut CartBase { &mut self.base }
    fn name(&self) -> &'static str { "CNROM (3)" }

    fn write_register(&mut self, addr: u16, val: u8, _cycle: u64) {
        let v = val & self.base.read_prg(addr).unwrap_or(0xFF);
        self.base.set_chr_8k(v as usize);
    }
}
