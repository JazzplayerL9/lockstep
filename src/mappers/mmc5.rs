//! Mapper 5: Nintendo MMC5 (ExROM). PRG/CHR banking with separate BG/sprite CHR
//! sets for 8x16 sprites, ExRAM (nametable / extended attributes / RAM modes),
//! fill mode, vertical split, scanline IRQ (detected from PPU fetch patterns, as
//! on hardware) and the 8x8 multiplier. Expansion audio is not emulated.

use super::{CartBase, Mapper, PrgKind, PrgSlot};

#[derive(Clone)]
pub struct Mmc5 {
    base: CartBase,
    exram: Vec<u8>,
    ppu_ctrl: u8,
    prg_mode: u8,
    chr_mode: u8,
    prg_ram_protect1: u8,
    prg_ram_protect2: u8,
    exram_mode: u8,
    nt_mapping: u8,
    fill_tile: u8,
    fill_attr: u8,
    prg_banks: [u8; 5],
    chr_banks: [u16; 12],
    chr_upper: u8,
    last_chr_reg: u16,
    prev_chr_a: bool,
    split_enabled: bool,
    split_right: bool,
    split_tile_delim: u8,
    split_scroll: u8,
    split_bank: u8,
    split_in_region: bool,
    split_tile: u32,
    split_tile_number: i32,
    mul1: u8,
    mul2: u8,
    irq_target: u8,
    irq_enabled: bool,
    irq_pending: bool,
    scanline_counter: u8,
    need_in_frame: bool,
    in_frame: bool,
    ppu_idle_counter: u8,
    last_ppu_addr: u16,
    nt_read_counter: u8,
    ex_last_nt_fetch: u16,
    ex_fetch_counter: i8,
    ex_chr_bank: u32,
}

impl Mmc5 {
    pub fn new(base: CartBase) -> Self {
        let mut m = Self {
            base,
            exram: vec![0; 0x400],
            ppu_ctrl: 0,
            prg_mode: 3,
            chr_mode: 0,
            prg_ram_protect1: 0,
            prg_ram_protect2: 0,
            exram_mode: 0,
            nt_mapping: 0,
            fill_tile: 0,
            fill_attr: 0,
            prg_banks: [0, 0, 0, 0, 0xFF],
            chr_banks: [0; 12],
            chr_upper: 0,
            last_chr_reg: 0,
            prev_chr_a: false,
            split_enabled: false,
            split_right: false,
            split_tile_delim: 0,
            split_scroll: 0,
            split_bank: 0,
            split_in_region: false,
            split_tile: 0,
            split_tile_number: 0,
            mul1: 0,
            mul2: 0,
            irq_target: 0,
            irq_enabled: false,
            irq_pending: false,
            scanline_counter: 0,
            need_in_frame: false,
            in_frame: false,
            ppu_idle_counter: 0,
            last_ppu_addr: 0,
            nt_read_counter: 0,
            ex_last_nt_fetch: 0,
            ex_fetch_counter: 0,
            ex_chr_bank: 0,
        };
        m.update_prg();
        m.update_chr(true);
        m
    }

    fn bank_slot(&self, reg: usize) -> PrgSlot {
        let raw = self.prg_banks[reg];
        let is_ram = reg == 0 || (raw & 0x80 == 0 && reg != 4);
        if is_ram {
            if self.base.prg_ram.is_empty() {
                return PrgSlot { kind: PrgKind::None, offset: 0, writable: false };
            }
            let n = (self.base.prg_ram.len() / 0x2000).max(1);
            let writable = self.prg_ram_protect1 == 2 && self.prg_ram_protect2 == 1;
            PrgSlot { kind: PrgKind::Ram, offset: ((raw & 7) as usize % n) * 0x2000, writable }
        } else {
            let n = self.base.prg_rom_banks_8k();
            PrgSlot { kind: PrgKind::Rom, offset: ((raw & 0x7F) as usize % n) * 0x2000, writable: false }
        }
    }

    fn set_slot(&mut self, slot: usize, reg: usize, bank_mask: u8, sub: u8) {
        let mut s = self.bank_slot(reg);
        let raw = self.prg_banks[reg];
        let bank = ((raw & bank_mask) | sub) as usize;
        match s.kind {
            PrgKind::Rom => s.offset = ((bank & 0x7F) % self.base.prg_rom_banks_8k()) * 0x2000,
            PrgKind::Ram => s.offset = ((bank & 7) % (self.base.prg_ram.len() / 0x2000).max(1)) * 0x2000,
            PrgKind::None => {}
        }
        self.base.prg[slot] = s;
    }

    fn update_prg(&mut self) {
        self.base.prg[0] = self.bank_slot(0);
        match self.prg_mode {
            0 => {
                for i in 0..4 {
                    self.set_slot(1 + i, 4, 0x7C, i as u8);
                }
            }
            1 => {
                self.set_slot(1, 2, 0xFE, 0);
                self.set_slot(2, 2, 0xFE, 1);
                self.set_slot(3, 4, 0x7E, 0);
                self.set_slot(4, 4, 0x7E, 1);
            }
            2 => {
                self.set_slot(1, 2, 0xFE, 0);
                self.set_slot(2, 2, 0xFE, 1);
                self.set_slot(3, 3, 0xFF, 0);
                self.set_slot(4, 4, 0x7F, 0);
            }
            _ => {
                self.set_slot(1, 1, 0xFF, 0);
                self.set_slot(2, 2, 0xFF, 0);
                self.set_slot(3, 3, 0xFF, 0);
                self.set_slot(4, 4, 0x7F, 0);
            }
        }
    }

    fn update_chr(&mut self, force: bool) {
        let large = self.ppu_ctrl & 0x20 != 0;
        if !large {
            self.last_chr_reg = 0;
        }
        let chr_a = !large
            || (self.split_tile_number >= 32 && self.split_tile_number < 48)
            || (!self.in_frame && self.last_chr_reg <= 0x5127);
        if !force && chr_a == self.prev_chr_a {
            return;
        }
        self.prev_chr_a = chr_a;
        let c = self.chr_banks;
        let sel = |a: usize, b: usize| if chr_a { c[a] as usize } else { c[b] as usize };
        let base = &mut self.base;
        match self.chr_mode {
            0 => {
                let bank = sel(7, 11) << 3;
                for i in 0..8 {
                    base.set_chr_1k(i, bank + i);
                }
            }
            1 => {
                let b0 = sel(3, 11) << 2;
                let b1 = sel(7, 11) << 2;
                for i in 0..4 {
                    base.set_chr_1k(i, b0 + i);
                    base.set_chr_1k(4 + i, b1 + i);
                }
            }
            2 => {
                let banks = [sel(1, 9), sel(3, 11), sel(5, 9), sel(7, 11)];
                for (i, b) in banks.iter().enumerate() {
                    base.set_chr_1k(i * 2, b << 1);
                    base.set_chr_1k(i * 2 + 1, (b << 1) + 1);
                }
            }
            _ => {
                let banks = [sel(0, 8), sel(1, 9), sel(2, 10), sel(3, 11), sel(4, 8), sel(5, 9), sel(6, 10), sel(7, 11)];
                for (i, b) in banks.iter().enumerate() {
                    base.set_chr_1k(i, *b);
                }
            }
        }
    }

    fn read_chr_raw(&self, pos: usize) -> u8 {
        if !self.base.chr_rom.is_empty() {
            self.base.chr_rom[pos % self.base.chr_rom.len()]
        } else {
            self.base.chr_ram[pos % self.base.chr_ram.len()]
        }
    }

    fn read_nametable(&self, addr: u16) -> u8 {
        let quadrant = ((addr >> 10) & 3) as usize;
        let off = (addr & 0x3FF) as usize;
        match (self.nt_mapping >> (quadrant * 2)) & 3 {
            0 => self.base.ciram[off],
            1 => self.base.ciram[0x400 + off],
            2 => {
                if self.exram_mode <= 1 {
                    self.exram[off]
                } else {
                    0
                }
            }
            _ => {
                if off < 0x3C0 {
                    self.fill_tile
                } else {
                    self.fill_attr * 0x55
                }
            }
        }
    }

    fn detect_scanline_start(&mut self, addr: u16) {
        if self.nt_read_counter >= 2 {
            if !self.in_frame && !self.need_in_frame {
                self.need_in_frame = true;
                self.scanline_counter = 0;
            } else {
                self.scanline_counter = self.scanline_counter.wrapping_add(1);
                if self.irq_target == self.scanline_counter {
                    self.irq_pending = true;
                }
            }
        } else if (0x2000..=0x2FFF).contains(&addr) && self.last_ppu_addr == addr {
            self.nt_read_counter += 1;
            if self.nt_read_counter >= 2 {
                self.split_tile_number = 0;
            }
        }
        if self.last_ppu_addr != addr {
            self.nt_read_counter = 0;
        }
    }

    fn leave_frame(&mut self) {
        self.in_frame = false;
        self.update_chr(true);
    }
}

impl Mapper for Mmc5 {
    fn base(&self) -> &CartBase { &self.base }
    fn base_mut(&mut self) -> &mut CartBase { &mut self.base }
    fn name(&self) -> &'static str { "MMC5 (5)" }

    fn cpu_read(&mut self, addr: u16) -> Option<u8> {
        match addr {
            0x5204 => {
                let v = (if self.in_frame { 0x40 } else { 0 }) | (if self.irq_pending { 0x80 } else { 0 });
                self.irq_pending = false;
                Some(v)
            }
            0x5205 => Some(((self.mul1 as u16 * self.mul2 as u16) & 0xFF) as u8),
            0x5206 => Some(((self.mul1 as u16 * self.mul2 as u16) >> 8) as u8),
            0x5015 => Some(0),
            0x5C00..=0x5FFF => {
                if self.exram_mode >= 2 {
                    Some(self.exram[(addr - 0x5C00) as usize])
                } else {
                    None
                }
            }
            0xFFFA | 0xFFFB => {
                self.leave_frame();
                self.last_ppu_addr = 0;
                self.scanline_counter = 0;
                self.irq_pending = false;
                self.base.read_prg(addr)
            }
            0x6000..=0xFFFF => self.base.read_prg(addr),
            _ => None,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8, _cycle: u64) {
        match addr {
            0x5100 => {
                self.prg_mode = val & 3;
                self.update_prg();
            }
            0x5101 => {
                self.chr_mode = val & 3;
                self.update_chr(true);
            }
            0x5102 => {
                self.prg_ram_protect1 = val & 3;
                self.update_prg();
            }
            0x5103 => {
                self.prg_ram_protect2 = val & 3;
                self.update_prg();
            }
            0x5104 => self.exram_mode = val & 3,
            0x5105 => self.nt_mapping = val,
            0x5106 => self.fill_tile = val,
            0x5107 => self.fill_attr = val & 3,
            0x5113..=0x5117 => {
                self.prg_banks[(addr - 0x5113) as usize] = val;
                self.update_prg();
            }
            0x5120..=0x512B => {
                let nv = val as u16 | ((self.chr_upper as u16) << 8);
                let i = (addr - 0x5120) as usize;
                if nv != self.chr_banks[i] || self.last_chr_reg != addr {
                    self.chr_banks[i] = nv;
                    self.last_chr_reg = addr;
                    self.update_chr(true);
                }
            }
            0x5130 => self.chr_upper = val & 3,
            0x5200 => {
                self.split_enabled = val & 0x80 != 0;
                self.split_right = val & 0x40 != 0;
                self.split_tile_delim = val & 0x1F;
            }
            0x5201 => self.split_scroll = val,
            0x5202 => self.split_bank = val,
            0x5203 => self.irq_target = val,
            0x5204 => self.irq_enabled = val & 0x80 != 0,
            0x5205 => self.mul1 = val,
            0x5206 => self.mul2 = val,
            0x5C00..=0x5FFF => {
                let i = (addr - 0x5C00) as usize;
                match self.exram_mode {
                    0 | 1 => self.exram[i] = if self.in_frame { val } else { 0 },
                    2 => self.exram[i] = val,
                    _ => {}
                }
            }
            0x6000..=0xFFFF => self.base.write_prg_ram(addr, val),
            _ => {}
        }
    }

    fn ppu_register_write(&mut self, addr: u16, val: u8) {
        if addr & 7 == 0 {
            self.ppu_ctrl = val;
        } else if addr & 7 == 1 && val & 0x18 == 0 {
            // Rendering disabled: the PPU stops fetching, so in-frame drops.
        }
    }

    fn cpu_clock(&mut self) {
        if self.ppu_idle_counter > 0 {
            self.ppu_idle_counter -= 1;
            if self.ppu_idle_counter == 0 {
                self.leave_frame();
            }
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let is_nt_fetch = (0x2000..=0x2FFF).contains(&addr) && (addr & 0x3FF) < 0x3C0;
        if is_nt_fetch {
            self.split_tile_number += 1;
            if self.in_frame {
                self.update_chr(false);
            } else if self.need_in_frame {
                self.need_in_frame = false;
                self.in_frame = true;
                self.update_chr(false);
            }
        }
        self.detect_scanline_start(addr);
        self.ppu_idle_counter = 3;
        self.last_ppu_addr = addr;

        if self.exram_mode <= 1 && self.in_frame {
            if self.split_enabled {
                let scanline = if self.split_tile_number >= 49 { self.scanline_counter.wrapping_add(1) } else { self.scanline_counter };
                let vscroll = ((scanline as u32 + self.split_scroll as u32) % 240) as u32;
                let column = ((self.split_tile_number + 2) % 50) as u8;
                if addr >= 0x2000 {
                    if is_nt_fetch {
                        if column == 0 {
                            self.split_in_region = !self.split_right;
                        }
                        if column == self.split_tile_delim && self.split_tile_number < 50 {
                            self.split_in_region = !self.split_in_region;
                        } else if column > 32 {
                            self.split_in_region = false;
                        }
                        if self.split_in_region {
                            self.split_tile = ((vscroll & 0xF8) << 2) | column as u32;
                            return self.exram[(self.split_tile & 0x3FF) as usize];
                        }
                    } else if self.split_in_region {
                        let t = self.split_tile;
                        let shift = ((t >> 4) & 0x04) | (t & 0x02);
                        let at = 0x3C0 | ((t & 0x380) >> 4) | ((t & 0x1F) >> 2);
                        let pal = (self.exram[at as usize] >> shift) & 3;
                        return pal * 0x55;
                    }
                } else if self.split_in_region {
                    let pos = ((self.split_bank as usize) << 12) + ((((addr as usize) & !7) | (vscroll as usize & 7)) & 0xFFF);
                    return self.read_chr_raw(pos);
                }
            }
            if self.exram_mode == 1 && (self.split_tile_number < 32 || self.split_tile_number >= 48) {
                if is_nt_fetch {
                    self.ex_last_nt_fetch = addr & 0x3FF;
                    self.ex_fetch_counter = 3;
                } else if self.ex_fetch_counter > 0 {
                    self.ex_fetch_counter -= 1;
                    match self.ex_fetch_counter {
                        2 => {
                            let v = self.exram[self.ex_last_nt_fetch as usize];
                            self.ex_chr_bank = ((v & 0x3F) as u32) | ((self.chr_upper as u32) << 6);
                            return ((v & 0xC0) >> 6) * 0x55;
                        }
                        _ => {
                            let pos = ((self.ex_chr_bank as usize) << 12) + (addr as usize & 0xFFF);
                            return self.read_chr_raw(pos);
                        }
                    }
                }
            }
        }

        if addr >= 0x2000 { self.read_nametable(addr) } else { self.base.read_chr(addr) }
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if addr < 0x2000 {
            self.base.write_chr(addr, val);
            return;
        }
        let quadrant = ((addr >> 10) & 3) as usize;
        let off = (addr & 0x3FF) as usize;
        match (self.nt_mapping >> (quadrant * 2)) & 3 {
            0 => self.base.ciram[off] = val,
            1 => self.base.ciram[0x400 + off] = val,
            2 => {
                if self.exram_mode <= 1 {
                    self.exram[off] = val;
                }
            }
            _ => {}
        }
    }

    fn irq(&self) -> bool {
        self.irq_pending && self.irq_enabled
    }
}
