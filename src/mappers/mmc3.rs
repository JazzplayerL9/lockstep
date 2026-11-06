// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones
//
// Portions derived from Mesen (https://github.com/nesdev-org/MesenCE):
// the MMC3 IRQ counter and MMC3-variant board logic.
// Mesen is Copyright (C) 2014-2026 Sour and contributors, GPL-3.0-or-later.

//! Mapper 4: Nintendo MMC3 / MMC6 (submapper 1) / Acclaim MC-ACC (submapper 3),
//! plus the MMC3-derived boards 47 (NES-QJ), 118 (TxSROM), 119 (TQROM) and
//! 206 (Namco 108 / DxROM).

use super::{CartBase, ChrKind, Mapper};
use crate::cartridge::Mirroring;

/// Nintendo MMC3 and its derivatives. Registers are decoded by A0 and A13-A14:
/// $8000/$8001 bank select/data, $A000/$A001 mirroring/PRG-RAM protect,
/// $C000/$C001 IRQ latch/reload, $E000/$E001 IRQ disable/enable. Eight bank registers
/// (R0-R7) give two 2 KB + four 1 KB CHR banks and two switchable 8 KB PRG banks; the
/// other two PRG slots hold the second-to-last and last banks.
///
/// The scanline IRQ counter is clocked by rising edges of PPU A12. The chip filters
/// out edges that follow only a brief low period (e.g. the alternating BG/sprite
/// fetches within a scanline), so normally it clocks once per scanline when BG uses
/// $0000 and sprites $1000. MMC3 revisions differ on a counter that reloads to 0:
/// rev B (the default here) fires the IRQ whenever the counter is 0 after a clock, rev A
/// only when it got there by decrementing or by an explicit $C001 reload
/// (selected per cartridge via `CartBase::mmc3_rev_a`).
#[derive(Clone)]
pub struct Mmc3 {
    /// Shared PRG/CHR/nametable state; registers are applied to it by [`Mmc3::update`].
    base: CartBase,
    /// iNES mapper number (4, 47, 118, 119 or 206) selecting board-specific wiring.
    id: u16,
    /// MMC6 (mapper 4 submapper 1): 1 KB internal PRG-RAM at $7000-$7FFF in two
    /// 512-byte halves with separate read/write enables, gated by $8000 bit 5.
    mmc6: bool,
    /// Acclaim MC-ACC (mapper 4 submapper 3): IRQ counter clocked on A12 falling
    /// edges through a divide-by-8 prescaler instead of the filtered rising edge.
    mc_acc: bool,
    /// $8000 bank select: bits 0-2 = which R register $8001 writes, bit 6 = PRG mode
    /// (0: R6 at $8000, second-last fixed at $C000; 1: swapped), bit 7 = CHR A12
    /// inversion (0: 2 KB banks at $0000; 1: 2 KB banks at $1000). On MMC6 bit 5
    /// enables the internal PRG-RAM. On Namco 108 (206) only bits 0-2 exist.
    reg8000: u8,
    /// $A001 PRG-RAM protect. MMC3: bit 7 = enable $6000-$7FFF, bit 6 = write protect.
    /// MMC6: bit 7/6 = read/write enable for $7200-$73FF, bit 5/4 = read/write enable
    /// for $7000-$71FF (writes ignored while $8000 bit 5 is clear). On mapper 47 the
    /// same enable/protect bits gate writes to the outer-bank register.
    rega001: u8,
    /// Bank registers R0-R7 written through $8001: R0-R1 = 2 KB CHR (bit 0 ignored),
    /// R2-R5 = 1 KB CHR, R6-R7 = 8 KB PRG. On TxSROM bit 7 of the CHR registers also
    /// selects the CIRAM page, on TQROM bit 6 selects CHR-RAM. Power-on values map
    /// CHR banks 0-7 in order and PRG banks 0/1.
    regs: [u8; 8],
    /// $C000 IRQ latch: value the counter is reloaded with.
    irq_latch: u8,
    /// Scanline down-counter, reloaded from `irq_latch` when it is 0 or a reload is pending.
    irq_counter: u8,
    /// Set by a $C001 write (which also zeroes the counter); forces a reload on the
    /// next counter clock.
    irq_reload: bool,
    /// IRQ enable: set by $E001, cleared by $E000.
    irq_enabled: bool,
    /// IRQ line state; set when the counter fires while enabled, acknowledged by $E000.
    irq_pending: bool,
    /// CPU cycle (+1) at which A12 went low; 0 while A12 is high.
    /// A rising edge only clocks the counter if A12 stayed low for at least 3 CPU
    /// cycles, which models the M2-based low-time filter.
    a12_low_since: u64,
    // MC-ACC falling-edge prescaler.
    /// MC-ACC prescaler: counts A12 falling edges 1..8; the IRQ counter is clocked on
    /// the first edge of each group of 8. Reset by $C001.
    acc_counter: u8,
    /// Previous PPU address seen by the MC-ACC edge detector, to find A12 high-to-low
    /// transitions.
    prev_addr: u16,
    // Mapper 47 outer bank.
    /// Mapper 47 (NES-QJ) outer bank, bit 0 of a $6000-$7FFF write: selects which
    /// 128 KB PRG / 128 KB CHR half the MMC3 banks address.
    block: u8,
    /// Header requests four-screen VRAM; $A000 mirroring writes are then ignored.
    four_screen: bool,
}

impl Mmc3 {
    /// Creates the chip in its power-on state for the board given by `base.mapper_id`
    /// and submapper. TQROM gets at least 8 KB CHR-RAM, MMC6 at least 1 KB PRG-RAM.
    pub fn new(mut base: CartBase) -> Self {
        let id = base.mapper_id;
        if id == 119 && base.chr_ram.len() < 0x2000 {
            base.chr_ram = vec![0; 0x2000];
        }
        let four_screen = base.mirroring == Mirroring::FourScreen;
        let mut m = Self {
            mmc6: id == 4 && base.submapper == 1,
            mc_acc: id == 4 && base.submapper == 3,
            base,
            id,
            reg8000: 0,
            rega001: 0,
            regs: [0, 2, 4, 5, 6, 7, 0, 1],
            irq_latch: 0,
            irq_counter: 0,
            irq_reload: false,
            irq_enabled: false,
            irq_pending: false,
            a12_low_since: 0,
            acc_counter: 0,
            prev_addr: 0,
            block: 0,
            four_screen,
        };
        if m.mmc6 && m.base.prg_ram.len() < 0x400 {
            m.base.prg_ram = vec![0; 0x400];
        }
        m.update();
        m
    }

    /// Translates a raw 1 KB CHR bank number into (bank, memory) for this board:
    /// mapper 47 adds the outer 128 KB block, TQROM sends banks $40-$7F to CHR-RAM.
    fn chr_page(&self, page: u8) -> (usize, ChrKind) {
        let mut page = page as usize;
        if self.id == 47 {
            page = (page & 0x7F) | ((self.block as usize) << 7);
        }
        if self.id == 119 && (0x40..=0x7F).contains(&page) {
            return (page - 0x40, ChrKind::Ram);
        }
        let kind = if self.base.chr_rom.is_empty() { ChrKind::Ram } else { ChrKind::Rom };
        (page, kind)
    }

    /// Translates an 8 KB PRG bank number for this board. On mapper 47 the bank is
    /// taken modulo 16 within the selected 128 KB block, so negative (from-the-end)
    /// numbers refer to the end of the block; other boards pass it through unchanged.
    fn prg_page(&self, page: i32) -> i32 {
        if self.id == 47 {
            (page.rem_euclid(16)) | ((self.block as i32) << 4)
        } else {
            page
        }
    }

    /// Recomputes PRG, CHR, TxSROM nametable and PRG-RAM mappings from the registers.
    /// MMC6 PRG-RAM is not slot-mapped; it is handled in `cpu_read` / `cpu_write`.
    fn update(&mut self) {
        let prg_mode = if self.id == 206 { 0 } else { (self.reg8000 >> 6) & 1 };
        let chr_mode = if self.id == 206 { 0 } else { (self.reg8000 >> 7) & 1 };
        let r = self.regs;

        let (p6, p7) = (r[6] as i32, r[7] as i32);
        let second_last = self.prg_page(-2);
        let last = self.prg_page(-1);
        let (s0, s2) = if prg_mode == 0 { (self.prg_page(p6), second_last) } else { (second_last, self.prg_page(p6)) };
        let s1 = self.prg_page(p7);
        self.base.set_prg_8k(1, s0);
        self.base.set_prg_8k(2, s1);
        self.base.set_prg_8k(3, s2);
        self.base.set_prg_8k(4, last);

        let pages: [u8; 8] = if chr_mode == 0 {
            [r[0] & 0xFE, r[0] | 1, r[1] & 0xFE, r[1] | 1, r[2], r[3], r[4], r[5]]
        } else {
            [r[2], r[3], r[4], r[5], r[0] & 0xFE, r[0] | 1, r[1] & 0xFE, r[1] | 1]
        };
        for (slot, &p) in pages.iter().enumerate() {
            let (page, kind) = self.chr_page(p);
            self.base.set_chr_1k_kind(slot, page, kind);
        }

        if self.id == 118 {
            // TxSROM: CHR register bit 7 selects the CIRAM page for each nametable.
            if chr_mode == 0 {
                let a = (r[0] >> 7) as usize;
                let b = (r[1] >> 7) as usize;
                self.base.nt = [a * 0x400, a * 0x400, b * 0x400, b * 0x400];
            } else {
                for i in 0..4 {
                    self.base.nt[i] = ((r[2 + i] >> 7) as usize) * 0x400;
                }
            }
        }

        if !self.mmc6 {
            if self.base.prg_ram.is_empty() {
                self.base.unmap_prg(0);
            } else if self.id == 206 {
                self.base.set_prg_ram_8k(0, 0, true);
            } else if self.rega001 & 0x80 != 0 {
                self.base.set_prg_ram_8k(0, 0, self.rega001 & 0x40 == 0);
            } else {
                self.base.unmap_prg(0);
            }
        }
    }

    /// Clocks the scanline counter once: reload from the latch if it is 0 or a reload
    /// is pending, else decrement, then raise the IRQ (if enabled) using the rev A or
    /// rev B zero condition described on [`Mmc3`].
    fn clock_irq_counter(&mut self) {
        let count = self.irq_counter;
        if self.irq_counter == 0 || self.irq_reload {
            self.irq_counter = self.irq_latch;
        } else {
            self.irq_counter -= 1;
        }
        // MC-ACC always uses the rev B condition (as Mesen does).
        let fire = if self.base.mmc3_rev_a && !self.mc_acc {
            (count > 0 || self.irq_reload) && self.irq_counter == 0
        } else {
            self.irq_counter == 0
        };
        if fire && self.irq_enabled {
            self.irq_pending = true;
        }
        self.irq_reload = false;
    }

    /// MMC6 `(readable, writable)` for the 512-byte half containing `addr` (chosen by
    /// A9), from the $A001 enable bits. Both false while $8000 bit 5 is clear.
    fn mmc6_ram_access(&self, addr: u16) -> (bool, bool) {
        if self.reg8000 & 0x20 == 0 {
            return (false, false);
        }
        if addr & 0x200 == 0 {
            (self.rega001 & 0x20 != 0, self.rega001 & 0x10 != 0)
        } else {
            (self.rega001 & 0x80 != 0, self.rega001 & 0x40 != 0)
        }
    }
}

impl Mapper for Mmc3 {
    fn base(&self) -> &CartBase { &self.base }
    fn base_mut(&mut self) -> &mut CartBase { &mut self.base }
    fn name(&self) -> &'static str {
        match self.id {
            47 => "MMC3 NES-QJ (47)",
            118 => "TxSROM (118)",
            119 => "TQROM (119)",
            206 => "Namco 108 (206)",
            _ if self.mmc6 => "MMC6 (4.1)",
            _ if self.mc_acc => "MC-ACC (4.3)",
            _ => "MMC3 (4)",
        }
    }

    fn cpu_read(&mut self, addr: u16) -> Option<u8> {
        if self.mmc6 && (0x6000..0x8000).contains(&addr) {
            if addr < 0x7000 {
                return None;
            }
            let (r0, _) = self.mmc6_ram_access(addr & !0x200);
            let (r1, _) = self.mmc6_ram_access(addr | 0x200);
            if !r0 && !r1 {
                return None;
            }
            let (readable, _) = self.mmc6_ram_access(addr);
            return Some(if readable { self.base.prg_ram[(addr & 0x3FF) as usize] } else { 0 });
        }
        self.base.read_prg(addr)
    }

    fn cpu_write(&mut self, addr: u16, val: u8, cycle: u64) {
        if addr < 0x8000 {
            if self.mmc6 {
                if addr >= 0x7000 && self.mmc6_ram_access(addr).1 {
                    self.base.prg_ram[(addr & 0x3FF) as usize] = val;
                }
                return;
            }
            if self.id == 47 && (0x6000..0x8000).contains(&addr) {
                if self.rega001 & 0x80 != 0 && self.rega001 & 0x40 == 0 {
                    self.block = val & 1;
                    self.update();
                }
                return;
            }
            self.base.write_prg_ram(addr, val);
            return;
        }
        self.write_register(addr, val, cycle);
    }

    fn write_register(&mut self, addr: u16, val: u8, _cycle: u64) {
        if self.id == 206 {
            if addr < 0xA000 {
                if addr & 1 == 0 {
                    self.reg8000 = val & 0x07;
                } else {
                    let idx = (self.reg8000 & 7) as usize;
                    self.regs[idx] = match idx {
                        0 | 1 => val & 0x3E,
                        2..=5 => val & 0x3F,
                        _ => val & 0x0F,
                    };
                    self.update();
                }
            }
            return;
        }
        match addr & 0xE001 {
            0x8000 => {
                self.reg8000 = val;
                self.update();
            }
            0x8001 => {
                let idx = (self.reg8000 & 7) as usize;
                self.regs[idx] = if idx <= 1 { val & !1 } else { val };
                self.update();
            }
            0xA000 => {
                if !self.four_screen && self.id != 118 {
                    self.base.set_mirroring(if val & 1 != 0 { Mirroring::Horizontal } else { Mirroring::Vertical });
                }
            }
            0xA001 => {
                if self.mmc6 && self.reg8000 & 0x20 == 0 {
                    return;
                }
                self.rega001 = val;
                self.update();
            }
            0xC000 => self.irq_latch = val,
            0xC001 => {
                if self.mc_acc {
                    self.acc_counter = 0;
                }
                self.irq_counter = 0;
                self.irq_reload = true;
            }
            0xE000 => {
                self.irq_enabled = false;
                self.irq_pending = false;
            }
            _ => self.irq_enabled = true,
        }
    }

    fn wants_vram_addr(&self) -> bool {
        self.id != 206
    }

    fn notify_vram_addr(&mut self, addr: u16, cpu_cycle: u64) {
        if self.mc_acc {
            // MC-ACC counts falling edges of A12, once per 8 edges.
            if addr & 0x1000 == 0 && self.prev_addr & 0x1000 != 0 {
                self.acc_counter += 1;
                if self.acc_counter == 1 {
                    self.clock_irq_counter();
                } else if self.acc_counter == 8 {
                    self.acc_counter = 0;
                }
            }
            self.prev_addr = addr;
            return;
        }
        if addr & 0x1000 != 0 {
            let rising = self.a12_low_since > 0 && (cpu_cycle + 1).saturating_sub(self.a12_low_since) >= 3;
            self.a12_low_since = 0;
            if rising {
                self.clock_irq_counter();
            }
        } else if self.a12_low_since == 0 {
            self.a12_low_since = cpu_cycle + 1;
        }
    }

    fn irq(&self) -> bool {
        self.irq_pending
    }

    fn battery_ram(&self) -> Vec<u8> {
        if self.mmc6 {
            return self.base.prg_ram[..0x400].to_vec();
        }
        let b = &self.base;
        let len = if b.save_ram_size > 0 { b.save_ram_size.min(b.prg_ram.len()) } else { b.prg_ram.len() };
        b.prg_ram[..len].to_vec()
    }
}
