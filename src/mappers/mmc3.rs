//! Mapper 4: Nintendo MMC3 / MMC6 (submapper 1) / Acclaim MC-ACC (submapper 3),
//! plus the MMC3-derived boards 47 (NES-QJ), 118 (TxSROM), 119 (TQROM) and
//! 206 (Namco 108 / DxROM).

use super::{CartBase, ChrKind, Mapper};
use crate::cartridge::Mirroring;

#[derive(Clone)]
pub struct Mmc3 {
    base: CartBase,
    id: u16,
    mmc6: bool,
    mc_acc: bool,
    reg8000: u8,
    rega001: u8,
    regs: [u8; 8],
    irq_latch: u8,
    irq_counter: u8,
    irq_reload: bool,
    irq_enabled: bool,
    irq_pending: bool,
    /// CPU cycle (+1) at which A12 went low; 0 while A12 is high.
    a12_low_since: u64,
    // MC-ACC falling-edge prescaler.
    acc_counter: u8,
    prev_addr: u16,
    // Mapper 47 outer bank.
    block: u8,
    four_screen: bool,
}

impl Mmc3 {
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

    fn prg_page(&self, page: i32) -> i32 {
        if self.id == 47 {
            (page.rem_euclid(16)) | ((self.block as i32) << 4)
        } else {
            page
        }
    }

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

    fn clock_irq_counter(&mut self) {
        let count = self.irq_counter;
        if self.irq_counter == 0 || self.irq_reload {
            self.irq_counter = self.irq_latch;
        } else {
            self.irq_counter -= 1;
        }
        let fire = if self.base.mmc3_rev_a {
            (count > 0 || self.irq_reload) && self.irq_counter == 0
        } else {
            self.irq_counter == 0
        };
        if fire && self.irq_enabled {
            self.irq_pending = true;
        }
        self.irq_reload = false;
    }

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
