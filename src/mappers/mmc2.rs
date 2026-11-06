// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones
//
// Portions derived from Mesen (https://github.com/nesdev-org/MesenCE):
// the MMC2/MMC4 latch logic.
// Mesen is Copyright (C) 2014-2026 Sour and contributors, GPL-3.0-or-later.

//! Mapper 9: MMC2 (Punch-Out!!) and mapper 10: MMC4 - CHR latches flipped by the
//! PPU fetching tiles $FD/$FE.

use super::{CartBase, Mapper};
use crate::cartridge::Mirroring;

/// Nintendo MMC2 / MMC4. Each 4 KB pattern table half has two CHR bank registers and
/// a latch that picks between them; the latch is set by the PPU itself when it fetches
/// tile $FD or $FE from that half, letting games switch CHR mid-screen without IRQs.
/// PRG: MMC2 has one switchable 8 KB bank at $8000 and the last three 8 KB banks
/// fixed; MMC4 has a switchable 16 KB bank at $8000 and the last 16 KB fixed.
#[derive(Clone)]
pub struct Mmc2 {
    /// Shared PRG/CHR/nametable state; CHR slots are refreshed by [`Mmc2::update_chr`].
    base: CartBase,
    /// MMC4 (mapper 10) behaviour: 16 KB PRG banking and the wider $0FD8-$0FDF /
    /// $0FE8-$0FEF left-latch trigger ranges.
    mmc4: bool,
    /// Latch for PPU $0000-$0FFF: 0 after a $FD tile fetch, 1 after $FE. Indexes `left`.
    left_latch: usize,
    /// Latch for PPU $1000-$1FFF: 0 after a $FD tile fetch, 1 after $FE. Indexes `right`.
    right_latch: usize,
    /// 4 KB CHR banks for PPU $0000: `[0]` from $B000 (latch $FD), `[1]` from $C000
    /// (latch $FE). 5 bits each.
    left: [u8; 2],
    /// 4 KB CHR banks for PPU $1000: `[0]` from $D000 (latch $FD), `[1]` from $E000
    /// (latch $FE). 5 bits each.
    right: [u8; 2],
    /// A latch changed on the last PPU fetch. The new bank is applied on the next
    /// address notification, because the latch flips only after the triggering
    /// tile's pattern fetch has completed (the trigger tile itself uses the old bank).
    need_update: bool,
}

impl Mmc2 {
    /// Sets the fixed PRG layout for MMC2 or MMC4 and starts both latches in the
    /// $FE state with all CHR bank registers 0.
    pub fn new(mut base: CartBase, mmc4: bool) -> Self {
        if mmc4 {
            base.set_prg_16k(0, 0);
            base.set_prg_16k(1, -1);
        } else {
            base.set_prg_8k(1, 0);
            base.set_prg_8k(2, -3);
            base.set_prg_8k(3, -2);
            base.set_prg_8k(4, -1);
        }
        let mut m = Self { base, mmc4, left_latch: 1, right_latch: 1, left: [0; 2], right: [0; 2], need_update: false };
        m.update_chr();
        m
    }

    /// Maps the CHR banks currently selected by the two latches.
    fn update_chr(&mut self) {
        self.base.set_chr_4k(0, self.left[self.left_latch] as usize);
        self.base.set_chr_4k(1, self.right[self.right_latch] as usize);
    }
}

impl Mapper for Mmc2 {
    fn base(&self) -> &CartBase { &self.base }
    fn base_mut(&mut self) -> &mut CartBase { &mut self.base }
    fn name(&self) -> &'static str { if self.mmc4 { "MMC4 (10)" } else { "MMC2 (9)" } }

    fn write_register(&mut self, addr: u16, val: u8, _cycle: u64) {
        match addr >> 12 {
            0xA => {
                if self.mmc4 {
                    self.base.set_prg_16k(0, (val & 0x0F) as i32);
                } else {
                    self.base.set_prg_8k(1, (val & 0x0F) as i32);
                }
            }
            0xB => self.left[0] = val & 0x1F,
            0xC => self.left[1] = val & 0x1F,
            0xD => self.right[0] = val & 0x1F,
            0xE => self.right[1] = val & 0x1F,
            0xF => self.base.set_mirroring(if val & 1 != 0 { Mirroring::Horizontal } else { Mirroring::Vertical }),
            _ => {}
        }
        self.update_chr();
    }

    fn wants_vram_addr(&self) -> bool {
        true
    }

    fn notify_vram_addr(&mut self, addr: u16, _cpu_cycle: u64) {
        // The latch flips after the triggering fetch completes.
        if self.need_update {
            self.update_chr();
            self.need_update = false;
        }
        let left_fd = if self.mmc4 { (0x0FD8..=0x0FDF).contains(&addr) } else { addr == 0x0FD8 };
        let left_fe = if self.mmc4 { (0x0FE8..=0x0FEF).contains(&addr) } else { addr == 0x0FE8 };
        if left_fd {
            self.left_latch = 0;
            self.need_update = true;
        } else if left_fe {
            self.left_latch = 1;
            self.need_update = true;
        } else if (0x1FD8..=0x1FDF).contains(&addr) {
            self.right_latch = 0;
            self.need_update = true;
        } else if (0x1FE8..=0x1FEF).contains(&addr) {
            self.right_latch = 1;
            self.need_update = true;
        }
    }
}
