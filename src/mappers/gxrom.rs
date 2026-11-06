// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones

//! 32 KB PRG + 8 KB CHR latch boards: GxROM (66), Color Dreams (11),
//! BNROM / NINA-001 (34), NINA-03/06 (79).

use super::{CartBase, Mapper};

/// Simple discrete-logic boards with a 32 KB PRG bank and an 8 KB (or 2x4 KB) CHR
/// bank, distinguished by `id`:
/// - 66 GxROM: $8000-$FFFF, bits 4-5 = PRG bank, bits 0-1 = CHR bank (bus conflicts).
/// - 11 Color Dreams: $8000-$FFFF, bits 0-1 = PRG bank, bits 4-7 = CHR bank (bus conflicts).
/// - 34 BNROM: $8000-$FFFF selects the 32 KB PRG bank (bus conflicts), CHR-RAM.
/// - 34 NINA-001 (detected by > 8 KB CHR-ROM): $7FFD = PRG bank (bit 0),
///   $7FFE / $7FFF = 4 KB CHR banks for $0000 / $1000 (bits 0-3); the writes also
///   land in the underlying PRG-RAM.
/// - 79 NINA-03/06: $4100-$5FFF with A8 set, bit 3 = PRG bank, bits 0-2 = CHR bank.
#[derive(Clone)]
pub struct GxRom {
    /// Shared PRG/CHR/nametable state; bank registers are applied directly to it.
    base: CartBase,
    /// iNES mapper number (11, 34, 66 or 79) selecting the register layout above.
    id: u16,
}

impl GxRom {
    /// Maps PRG bank 0 and CHR bank 0, taking the board variant from `base.mapper_id`.
    pub fn new(mut base: CartBase) -> Self {
        let id = base.mapper_id;
        base.set_prg_32k(0);
        base.set_chr_8k(0);
        Self { base, id }
    }

    /// True for the mapper 34 NINA-001 variant, told apart from BNROM by having more
    /// than 8 KB of CHR-ROM (BNROM uses CHR-RAM).
    fn nina001(&self) -> bool {
        self.id == 34 && self.base.chr_rom.len() > 0x2000
    }
}

impl Mapper for GxRom {
    fn base(&self) -> &CartBase { &self.base }
    fn base_mut(&mut self) -> &mut CartBase { &mut self.base }
    fn name(&self) -> &'static str {
        match self.id {
            11 => "Color Dreams (11)",
            34 => "BNROM (34)",
            79 => "NINA-03/06 (79)",
            _ => "GxROM (66)",
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8, _cycle: u64) {
        match self.id {
            79 if (0x4100..0x6000).contains(&addr) && addr & 0x100 != 0 => {
                self.base.set_prg_32k(((val >> 3) & 1) as i32);
                self.base.set_chr_8k((val & 7) as usize);
            }
            34 if self.nina001() && (0x7FFD..=0x7FFF).contains(&addr) => {
                self.base.write_prg_ram(addr, val);
                match addr {
                    0x7FFD => self.base.set_prg_32k((val & 1) as i32),
                    0x7FFE => self.base.set_chr_4k(0, (val & 0x0F) as usize),
                    _ => self.base.set_chr_4k(1, (val & 0x0F) as usize),
                }
            }
            _ if addr >= 0x8000 => {
                let v = val & self.base.read_prg(addr).unwrap_or(0xFF);
                match self.id {
                    11 => {
                        self.base.set_prg_32k((v & 3) as i32);
                        self.base.set_chr_8k((v >> 4) as usize);
                    }
                    34 => {
                        if !self.nina001() {
                            self.base.set_prg_32k(v as i32);
                        }
                    }
                    66 => {
                        self.base.set_prg_32k(((v >> 4) & 3) as i32);
                        self.base.set_chr_8k((v & 3) as usize);
                    }
                    _ => {}
                }
            }
            _ => self.base.write_prg_ram(addr, val),
        }
    }
}
