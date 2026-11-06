//! Mapper 1: Nintendo MMC1 (SxROM, including SUROM 512 KB PRG and fixed-32K SEROM),
//! and mapper 105: NES-EVENT (Nintendo World Championships 1990) with its IRQ timer.

use super::{CartBase, Mapper};
use crate::cartridge::Mirroring;

#[derive(Clone)]
pub struct Mmc1 {
    base: CartBase,
    shift: u8,
    shift_count: u8,
    control: u8,
    chr0: u8,
    chr1: u8,
    prg: u8,
    last_chr_reg: u16,
    last_write_cycle: u64,
    // Mapper 105 state.
    event: bool,
    init_state: u8,
    irq_counter: u32,
    irq_enabled: bool,
    irq_pending: bool,
    /// NWC timer DIP switches (0 = 5:00.4 minutes, the competition setting is 4).
    pub dip: u32,
}

impl Mmc1 {
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
