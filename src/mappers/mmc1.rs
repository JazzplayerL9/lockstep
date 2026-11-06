// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones
//
// Portions derived from Mesen (https://github.com/nesdev-org/MesenCE):
// the MMC1 / NES-EVENT board logic.
// Mesen is Copyright (C) 2014-2026 Sour and contributors, GPL-3.0-or-later.

//! Mapper 1: Nintendo MMC1 (SxROM, including SUROM 512 KB PRG and fixed-32K SEROM),
//! and mapper 105: NES-EVENT (Nintendo World Championships 1990) with its IRQ timer.

use super::{CartBase, Mapper};
use crate::cartridge::Mirroring;

/// Nintendo MMC1. The CPU loads its four 5-bit registers serially: each write to
/// $8000-$FFFF with bit 7 clear shifts bit 0 into a 5-bit shift register (LSB first);
/// the fifth write copies the value into the register chosen by address bits 13-14
/// ($8000 control, $A000 CHR bank 0, $C000 CHR bank 1, $E000 PRG bank) and clears
/// the shift register. A write with bit 7 set resets the shift register and forces
/// PRG mode 3. The chip ignores a write on the CPU cycle right after another write,
/// which matters for read-modify-write instructions (dummy write + real write).
///
/// The NES-EVENT board (mapper 105) reuses the MMC1 registers for its own PRG
/// layout and a CPU-cycle timer IRQ (the NWC 1990 time limit); see [`Mmc1::update`].
#[derive(Clone)]
pub struct Mmc1 {
    /// Shared PRG/CHR/nametable state; registers are applied to it by [`Mmc1::update`].
    base: CartBase,
    /// Serial load shift register. Each data bit enters at bit 4 and moves right,
    /// so after five writes bits 0-4 hold the value in write order.
    shift: u8,
    /// Number of bits shifted in so far (0-4); the fifth write commits and resets it.
    shift_count: u8,
    /// $8000 control (5 bits): bits 0-1 mirroring (0 one-screen lower, 1 one-screen
    /// upper, 2 vertical, 3 horizontal), bits 2-3 PRG mode (0/1 32 KB, 2 fix first
    /// bank at $8000, 3 fix last bank at $C000), bit 4 CHR mode (0 8 KB, 1 two 4 KB).
    /// Powers up as $0C (PRG mode 3).
    control: u8,
    /// $A000 CHR bank 0 (5 bits): 4 KB bank at PPU $0000, or the 8 KB bank (low bit
    /// ignored) in 8 KB mode. On SUROM/SOROM/SXROM its upper bits also select the
    /// 256 KB PRG-ROM half (bit 4) and the PRG-RAM bank (bits 2-3 or bit 3).
    chr0: u8,
    /// $C000 CHR bank 1 (5 bits): 4 KB bank at PPU $1000; ignored in 8 KB CHR mode.
    /// In 4 KB mode it also supplies the SUROM/SXROM outer bits while it was the
    /// most recently written CHR register.
    chr1: u8,
    /// $E000 PRG bank (5 bits): bits 0-3 select the 16 KB bank (bit 0 ignored in
    /// 32 KB mode), bit 4 set disables PRG-RAM at $6000-$7FFF (MMC1B and later).
    prg: u8,
    /// Address ($A000 or $C000) of the CHR register written last. In 4 KB CHR mode
    /// the outer PRG / PRG-RAM bank bits are taken from that register. (On hardware
    /// they follow whichever CHR register PPU A12 currently selects; using the
    /// last-written one is the same approximation Mesen makes.)
    last_chr_reg: u16,
    /// CPU cycle of the previous register write, for the consecutive-write filter.
    /// Initialised far enough away that the first write is never filtered.
    last_write_cycle: u64,
    // Mapper 105 state.
    /// True for the NES-EVENT board (mapper 105) instead of a plain SxROM.
    event: bool,
    /// NES-EVENT power-on lock: 0 = locked to the first 32 KB, 1 = $A000 bit 4 has
    /// been seen low, 2 = it has since gone high again and normal PRG banking is live.
    init_state: u8,
    /// NES-EVENT timer: counts CPU cycles up from 0 while enabled; the IRQ fires when
    /// it reaches `$2000_0000 | (dip << 25)`.
    irq_counter: u32,
    /// NES-EVENT timer running. Follows $A000 bit 4 (clear = running, set = held at 0
    /// and acknowledged); cleared when the timer expires.
    irq_enabled: bool,
    /// NES-EVENT IRQ line, set when the timer expires and cleared by setting $A000 bit 4.
    irq_pending: bool,
    /// NWC timer DIP switches (0 = 5:00.4 minutes, the competition setting is 4).
    pub dip: u32,
}

impl Mmc1 {
    /// Creates the chip in its power-on state (control $0C, all banks 0). For the
    /// NES-EVENT board `event` is set and $A000 bit 4 starts high, so the timer is
    /// held and PRG is locked to the first 32 KB until the game toggles it.
    pub fn new(base: CartBase, event: bool) -> Self {
        let mut m = Self {
            base,
            shift: 0,
            shift_count: 0,
            control: 0x0C,
            chr0: 0,
            chr1: 0,
            prg: 0,
            last_chr_reg: 0xA000,
            last_write_cycle: u64::MAX - 8,
            event,
            init_state: 0,
            irq_counter: 0,
            irq_enabled: false,
            irq_pending: false,
            dip: 4,
        };
        if event {
            m.chr0 |= 0x10;
        }
        m.update();
        m
    }

    /// Recomputes mirroring and all PRG, PRG-RAM and CHR mappings from the registers.
    ///
    /// SxROM: `prg` banks 16/32 KB according to `control`; for 512 KB PRG (SUROM)
    /// bit 4 of the active CHR register picks the 256 KB half, applied to the fixed
    /// bank too. PRG-RAM larger than 8 KB is banked by CHR register bits 2-3 (32 KB,
    /// SXROM) or bit 3 (16 KB, SOROM). Submapper 5 (SEROM and friends) is fixed 32 KB.
    ///
    /// NES-EVENT: `chr0` bit 4 holds/releases the timer and drives `init_state`; once
    /// unlocked, `chr0` bit 3 = 0 maps a 32 KB bank of the first 128 KB chip from
    /// `chr0` bits 1-2, and bit 3 = 1 banks the second 128 KB chip with `prg` bits 0-2
    /// using the normal MMC1 PRG modes. CHR is an unbanked 8 KB RAM.
    fn update(&mut self) {
        let b = &mut self.base;
        b.set_mirroring(match self.control & 3 {
            0 => Mirroring::OneScreenLower,
            1 => Mirroring::OneScreenUpper,
            2 => Mirroring::Vertical,
            _ => Mirroring::Horizontal,
        });
        let prg_mode_16k = self.control & 0x08 != 0;
        let fix_last = self.control & 0x04 != 0;
        let chr_4k = self.control & 0x10 != 0;
        let ram_enabled = self.prg & 0x10 == 0;

        if self.event {
            if self.init_state == 0 && self.chr0 & 0x10 == 0 {
                self.init_state = 1;
            } else if self.init_state == 1 && self.chr0 & 0x10 != 0 {
                self.init_state = 2;
            }
            if self.chr0 & 0x10 != 0 {
                self.irq_enabled = false;
                self.irq_counter = 0;
                self.irq_pending = false;
            } else {
                self.irq_enabled = true;
            }
            b.set_prg_ram_8k(0, 0, ram_enabled);
            if !ram_enabled {
                b.unmap_prg(0);
            }
            if self.init_state == 2 {
                if self.chr0 & 0x08 != 0 {
                    let reg = ((self.prg & 0x07) | 0x08) as i32;
                    if prg_mode_16k {
                        if fix_last {
                            b.set_prg_16k(0, reg);
                            b.set_prg_16k(1, 0x0F);
                        } else {
                            b.set_prg_16k(0, 0x08);
                            b.set_prg_16k(1, reg);
                        }
                    } else {
                        b.set_prg_32k(reg >> 1);
                    }
                } else {
                    b.set_prg_32k(((self.chr0 & 0x06) >> 1) as i32);
                }
            } else {
                b.set_prg_32k(0);
            }
            // CHR is 8 KB of RAM on this board.
            b.set_chr_8k(0);
            return;
        }

        let extra = if self.last_chr_reg == 0xC000 && chr_4k { self.chr1 } else { self.chr0 };
        let outer = if b.prg_rom.len() == 0x80000 { (extra & 0x10) as i32 } else { 0 };

        // PRG RAM: SOROM/SXROM banking through the CHR registers when > 8 KB.
        if b.prg_ram.len() > 0x2000 {
            let bank = if b.prg_ram.len() > 0x4000 { (extra >> 2) & 3 } else { (extra >> 3) & 1 };
            b.set_prg_ram_8k(0, bank as usize, ram_enabled);
        } else {
            b.set_prg_ram_8k(0, 0, ram_enabled);
        }
        if !ram_enabled {
            b.unmap_prg(0);
        }

        if b.submapper == 5 {
            b.set_prg_32k(0);
        } else if prg_mode_16k {
            let reg = (self.prg & 0x0F) as i32;
            if fix_last {
                b.set_prg_16k(0, reg | outer);
                b.set_prg_16k(1, 0x0F | outer);
            } else {
                b.set_prg_16k(0, outer);
                b.set_prg_16k(1, reg | outer);
            }
        } else {
            b.set_prg_32k((((self.prg & 0x0E) as i32) | outer) >> 1);
        }

        if chr_4k {
            b.set_chr_4k(0, self.chr0 as usize);
            b.set_chr_4k(1, self.chr1 as usize);
        } else {
            b.set_chr_8k((self.chr0 >> 1) as usize);
        }
    }
}

impl Mapper for Mmc1 {
    fn base(&self) -> &CartBase { &self.base }
    fn base_mut(&mut self) -> &mut CartBase { &mut self.base }
    fn name(&self) -> &'static str { if self.event { "NES-EVENT (105)" } else { "MMC1 (1)" } }

    fn write_register(&mut self, addr: u16, val: u8, cycle: u64) {
        // Writes on consecutive cycles (RMW dummy write + real write) only count once.
        let consecutive = cycle.wrapping_sub(self.last_write_cycle) < 2;
        self.last_write_cycle = cycle;
        if consecutive && val & 0x80 == 0 {
            return;
        }
        if val & 0x80 != 0 {
            self.shift = 0;
            self.shift_count = 0;
            self.control |= 0x0C;
            self.update();
            return;
        }
        self.shift = (self.shift >> 1) | ((val & 1) << 4);
        self.shift_count += 1;
        if self.shift_count == 5 {
            let data = self.shift & 0x1F;
            match addr & 0xE000 {
                0x8000 => self.control = data,
                0xA000 => {
                    self.last_chr_reg = 0xA000;
                    self.chr0 = data;
                }
                0xC000 => {
                    self.last_chr_reg = 0xC000;
                    self.chr1 = data;
                }
                _ => self.prg = data,
            }
            self.shift = 0;
            self.shift_count = 0;
            self.update();
        }
    }

    fn wants_cpu_clock(&self) -> bool {
        self.event
    }

    fn cpu_clock(&mut self) {
        if self.event && self.irq_enabled {
            self.irq_counter += 1;
            let max = 0x2000_0000u32 | (self.dip << 25);
            if self.irq_counter >= max {
                self.irq_pending = true;
                self.irq_enabled = false;
            }
        }
    }

    fn irq(&self) -> bool {
        self.irq_pending
    }
}
