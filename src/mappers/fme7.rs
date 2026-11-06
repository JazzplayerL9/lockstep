//! Mapper 69: Sunsoft FME-7 (Batman: Return of the Joker). The 5B audio variant's
//! extra channels are not emulated.

use super::{CartBase, Mapper, PrgKind, PrgSlot};
use crate::cartridge::Mirroring;

#[derive(Clone)]
pub struct Fme7 {
    base: CartBase,
    command: u8,
    wram_reg: u8,
    irq_enabled: bool,
    irq_counter_enabled: bool,
    irq_counter: u16,
    irq_pending: bool,
}

impl Fme7 {
    pub fn new(mut base: CartBase) -> Self {
        if base.prg_ram.len() < 0x8000 {
            let mut ram = base.prg_ram.clone();
            ram.resize(0x8000, 0);
            base.prg_ram = ram;
        }
        base.set_prg_8k(4, -1);
        let mut m = Self { base, command: 0, wram_reg: 0, irq_enabled: false, irq_counter_enabled: false, irq_counter: 0, irq_pending: false };
        m.update_wram();
        m
    }

    fn update_wram(&mut self) {
        let bank = (self.wram_reg & 0x3F) as usize;
        if self.wram_reg & 0x40 != 0 {
            if self.wram_reg & 0x80 != 0 {
                self.base.set_prg_ram_8k(0, bank, true);
            } else {
                self.base.unmap_prg(0);
            }
        } else {
            let n = self.base.prg_rom_banks_8k();
            self.base.prg[0] = PrgSlot { kind: PrgKind::Rom, offset: (bank % n) * 0x2000, writable: false };
        }
    }
}

impl Mapper for Fme7 {
    fn base(&self) -> &CartBase { &self.base }
    fn base_mut(&mut self) -> &mut CartBase { &mut self.base }
    fn name(&self) -> &'static str { "FME-7 (69)" }

    fn write_register(&mut self, addr: u16, val: u8, _cycle: u64) {
        match addr & 0xE000 {
            0x8000 => self.command = val & 0x0F,
            0xA000 => match self.command {
                0..=7 => self.base.set_chr_1k(self.command as usize, val as usize),
                8 => {
                    self.wram_reg = val;
                    self.update_wram();
                }
                9..=11 => self.base.set_prg_8k((self.command - 8) as usize, (val & 0x3F) as i32),
                12 => self.base.set_mirroring(match val & 3 {
                    0 => Mirroring::Vertical,
                    1 => Mirroring::Horizontal,
                    2 => Mirroring::OneScreenLower,
                    _ => Mirroring::OneScreenUpper,
                }),
                13 => {
                    self.irq_enabled = val & 0x01 != 0;
                    self.irq_counter_enabled = val & 0x80 != 0;
                    self.irq_pending = false;
                }
                14 => self.irq_counter = (self.irq_counter & 0xFF00) | val as u16,
                _ => self.irq_counter = (self.irq_counter & 0x00FF) | ((val as u16) << 8),
            },
            _ => {}
        }
    }

    fn cpu_clock(&mut self) {
        if self.irq_counter_enabled {
            self.irq_counter = self.irq_counter.wrapping_sub(1);
            if self.irq_counter == 0xFFFF && self.irq_enabled {
                self.irq_pending = true;
            }
        }
    }

    fn irq(&self) -> bool {
        self.irq_pending
    }
}
