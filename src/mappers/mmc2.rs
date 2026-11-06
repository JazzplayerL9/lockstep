//! Mapper 9: MMC2 (Punch-Out!!) and mapper 10: MMC4 - CHR latches flipped by the
//! PPU fetching tiles $FD/$FE.

use super::{CartBase, Mapper};
use crate::cartridge::Mirroring;

#[derive(Clone)]
pub struct Mmc2 {
    base: CartBase,
    mmc4: bool,
    left_latch: usize,
    right_latch: usize,
    left: [u8; 2],
    right: [u8; 2],
    need_update: bool,
}

impl Mmc2 {
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
