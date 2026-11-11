// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones

//! Mapper 19: Namco 163 / 129 (*Megami Tensei II*, *Rolling Thunder*, *Splatterhouse*,
//! *Erika to Satoru no Yume Bouken*, *Mappy Kids*, *Final Lap*).
//! Implements PRG/CHR banking, ROM/CIRAM nametables, the 15-bit CPU-cycle IRQ counter,
//! and the 1-to-8 channel wavetable expansion synthesizer.

use super::{CartBase, ChrKind, Mapper};

/// Namco 163 / 129 board (Mapper 19).
#[derive(Clone)]
pub struct Namco163 {
    /// Shared PRG/CHR/nametable state.
    base: CartBase,
    /// 128 bytes of internal wavetable / audio register RAM ($4800 / $F800).
    internal_ram: [u8; 128],
    /// Current 7-bit address in `internal_ram` ($F800 bits 0-6).
    ram_addr: u8,
    /// Auto-increment `ram_addr` on $4800 reads/writes ($F800 bit 7).
    ram_auto_inc: bool,
    /// 15-bit up-counting IRQ timer ($5000/$5800).
    irq_counter: u16,
    /// IRQ enabled ($5800 bit 7).
    irq_enabled: bool,
    /// IRQ asserted (set when `irq_counter` reaches $7FFF while enabled).
    irq_pending: bool,
    /// $E800 bit 6: disable CIRAM mapping for CHR $0000-$0FFF (`true` = always CHR-ROM).
    chr_nt_disable_low: bool,
    /// $E800 bit 7: disable CIRAM mapping for CHR $1000-$1FFF (`true` = always CHR-ROM).
    chr_nt_disable_high: bool,
    /// Raw 8-bit CHR bank registers for $0000-$1FFF ($8000-$BFFF).
    chr_regs: [u8; 8],
    /// Raw 8-bit nametable bank registers for $2000-$2FFF ($C000-$DFFF).
    nt_regs: [u8; 4],
    /// Audio channel clock divider (0-14; updates one channel every 15 CPU cycles).
    audio_div: u8,
    /// Currently active channel index (7 down to `8 - num_channels`).
    active_ch: u8,
    /// Latest output of each of the 8 channels, averaged for click-free mixing.
    ch_out: [f32; 8],
    /// Expansion audio disabled ($E000 bit 6).
    audio_disabled: bool,
}

impl Namco163 {
    /// Creates a Namco 163 board with the last 8 KB PRG-ROM bank fixed at $E000.
    pub fn new(mut base: CartBase) -> Self {
        base.set_prg_8k(4, -1);
        Self {
            base,
            internal_ram: [0; 128],
            ram_addr: 0,
            ram_auto_inc: false,
            irq_counter: 0,
            irq_enabled: false,
            irq_pending: false,
            chr_nt_disable_low: false,
            chr_nt_disable_high: false,
            chr_regs: [0; 8],
            nt_regs: [0xE0; 4],
            audio_div: 0,
            active_ch: 7,
            ch_out: [0.0; 8],
            audio_disabled: false,
        }
    }

    /// Steps the current wavetable channel `ch` (0-7, where 7 is the primary channel at $78-$7F).
    fn step_audio_channel(&mut self, ch: usize) {
        let base = 0x40 + ch * 8;
        let freq = (self.internal_ram[base] as u32)
            | ((self.internal_ram[base + 2] as u32) << 8)
            | (((self.internal_ram[base + 4] & 0x03) as u32) << 16);
        let phase = (self.internal_ram[base + 1] as u32)
            | ((self.internal_ram[base + 3] as u32) << 8)
            | ((self.internal_ram[base + 5] as u32) << 16);
        let len = 256 - ((self.internal_ram[base + 4] & 0xFC) as u32);
        let wave_addr = self.internal_ram[base + 6] as u32;
        let vol = (self.internal_ram[base + 7] & 0x0F) as f32;

        let new_phase = (phase + freq) % (len << 16);
        self.internal_ram[base + 1] = (new_phase & 0xFF) as u8;
        self.internal_ram[base + 3] = ((new_phase >> 8) & 0xFF) as u8;
        self.internal_ram[base + 5] = ((new_phase >> 16) & 0xFF) as u8;

        let sample_pos = ((new_phase >> 16) + wave_addr) & 0xFF;
        let byte = self.internal_ram[(sample_pos >> 1) as usize];
        let nibble = if sample_pos & 1 != 0 { byte >> 4 } else { byte & 0x0F };
        let centered = (nibble as f32) - 8.0;
        self.ch_out[ch] = centered * vol * 0.0012;
    }
}

impl Mapper for Namco163 {
    fn base(&self) -> &CartBase {
        &self.base
    }
    fn base_mut(&mut self) -> &mut CartBase {
        &mut self.base
    }
    fn name(&self) -> &'static str {
        "Namco 163 (19)"
    }

    fn cpu_read(&mut self, addr: u16) -> Option<u8> {
        match addr {
            0x4800..=0x4FFF => {
                let v = self.internal_ram[(self.ram_addr & 0x7F) as usize];
                if self.ram_auto_inc {
                    self.ram_addr = (self.ram_addr + 1) & 0x7F;
                }
                Some(v)
            }
            0x5000..=0x57FF => Some((self.irq_counter & 0xFF) as u8),
            0x5800..=0x5FFF => {
                Some(((self.irq_counter >> 8) as u8 & 0x7F) | if self.irq_enabled { 0x80 } else { 0 })
            }
            0x6000..=0xFFFF => self.base.read_prg(addr),
            _ => None,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8, _cycle: u64) {
        match addr {
            0x4800..=0x4FFF => {
                self.internal_ram[(self.ram_addr & 0x7F) as usize] = val;
                if self.ram_auto_inc {
                    self.ram_addr = (self.ram_addr + 1) & 0x7F;
                }
            }
            0x5000..=0x57FF => {
                self.irq_counter = (self.irq_counter & 0x7F00) | val as u16;
                self.irq_pending = false;
            }
            0x5800..=0x5FFF => {
                self.irq_counter = (self.irq_counter & 0x00FF) | (((val & 0x7F) as u16) << 8);
                self.irq_enabled = val & 0x80 != 0;
                self.irq_pending = false;
            }
            0x6000..=0x7FFF => self.base.write_prg_ram(addr, val),
            0x8000..=0xBFFF => {
                let slot = ((addr - 0x8000) >> 11) as usize;
                self.chr_regs[slot] = val;
                self.base.set_chr_1k_kind(slot, val as usize, ChrKind::Rom);
            }
            0xC000..=0xDFFF => {
                let nt = ((addr - 0xC000) >> 11) as usize;
                self.nt_regs[nt] = val;
                if val >= 0xE0 {
                    self.base.set_nametable(nt, (val & 1) as usize);
                }
            }
            0xE000..=0xE7FF => {
                self.audio_disabled = val & 0x40 != 0;
                self.base.set_prg_8k(1, (val & 0x3F) as i32);
            }
            0xE800..=0xEFFF => {
                self.chr_nt_disable_low = val & 0x40 != 0;
                self.chr_nt_disable_high = val & 0x80 != 0;
                self.base.set_prg_8k(2, (val & 0x3F) as i32);
            }
            0xF000..=0xF7FF => {
                self.base.set_prg_8k(3, (val & 0x3F) as i32);
            }
            0xF800..=0xFFFF => {
                self.ram_addr = val & 0x7F;
                self.ram_auto_inc = val & 0x80 != 0;
            }
            _ => {}
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        if addr < 0x2000 {
            let slot = (addr >> 10) as usize;
            let reg = self.chr_regs[slot];
            let nt_dis = if slot < 4 { self.chr_nt_disable_low } else { self.chr_nt_disable_high };
            if !nt_dis && reg >= 0xE0 {
                return self.base.ciram[((reg as usize & 1) << 10) | (addr as usize & 0x3FF)];
            }
            return self.base.read_chr(addr);
        }
        let nt = ((addr - 0x2000) >> 10) as usize & 3;
        let reg = self.nt_regs[nt];
        if reg >= 0xE0 {
            self.base.ciram[((reg as usize & 1) << 10) | (addr as usize & 0x3FF)]
        } else if !self.base.chr_rom.is_empty() {
            let pos = ((reg as usize) << 10) | (addr as usize & 0x3FF);
            self.base.chr_rom[pos % self.base.chr_rom.len()]
        } else {
            self.base.read_nt(addr)
        }
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if addr < 0x2000 {
            let slot = (addr >> 10) as usize;
            let reg = self.chr_regs[slot];
            let nt_dis = if slot < 4 { self.chr_nt_disable_low } else { self.chr_nt_disable_high };
            if !nt_dis && reg >= 0xE0 {
                self.base.ciram[((reg as usize & 1) << 10) | (addr as usize & 0x3FF)] = val;
            } else {
                self.base.write_chr(addr, val);
            }
            return;
        }
        let nt = ((addr - 0x2000) >> 10) as usize & 3;
        let reg = self.nt_regs[nt];
        if reg >= 0xE0 {
            self.base.ciram[((reg as usize & 1) << 10) | (addr as usize & 0x3FF)] = val;
        }
    }

    fn wants_cpu_clock(&self) -> bool {
        true
    }

    fn cpu_clock(&mut self) {
        if self.irq_enabled && self.irq_counter < 0x7FFF {
            self.irq_counter += 1;
            if self.irq_counter == 0x7FFF {
                self.irq_pending = true;
            }
        }
        if !self.audio_disabled {
            self.audio_div += 1;
            if self.audio_div >= 15 {
                self.audio_div = 0;
                let num_ch = ((self.internal_ram[0x7F] >> 4) & 7) + 1;
                let min_ch = 8 - num_ch;
                self.step_audio_channel(self.active_ch as usize);
                if self.active_ch <= min_ch {
                    self.active_ch = 7;
                } else {
                    self.active_ch -= 1;
                }
            }
        }
    }

    fn has_expansion_audio(&self) -> bool {
        true
    }

    fn audio_sample(&self) -> f32 {
        if self.audio_disabled {
            return 0.0;
        }
        let num_ch = (((self.internal_ram[0x7F] >> 4) & 7) + 1) as usize;
        let mut sum = 0.0f32;
        for ch in (8 - num_ch)..8 {
            sum += self.ch_out[ch];
        }
        sum / (num_ch as f32)
    }

    fn irq(&self) -> bool {
        self.irq_pending
    }
}
