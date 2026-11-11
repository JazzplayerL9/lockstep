// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones
//
// Portions derived from Mesen (https://github.com/nesdev-org/MesenCE):
// the FME-7 board logic.
// Mesen is Copyright (C) 2014-2026 Sour and contributors, GPL-3.0-or-later.

//! Mapper 69: Sunsoft FME-7 / 5B (*Batman: Return of the Joker*, *Gimmick!*).
//! Includes the Sunsoft 5B's 3-channel YM2149F PSG expansion audio (`$C000`/`$E000`).

use super::{CartBase, Mapper, PrgKind, PrgSlot};
use crate::cartridge::Mirroring;

/// Logarithmic 4-bit volume table (~3 dB per step) for the Sunsoft 5B (YM2149F) channels.
const SUNSOFT_5B_VOL: [f32; 16] = [
    0.0, 0.00075, 0.00106, 0.00150, 0.00212, 0.00300, 0.00424, 0.00600,
    0.00849, 0.01200, 0.01697, 0.02400, 0.03394, 0.04800, 0.06788, 0.09600,
];

/// Sunsoft FME-7 / 5B. Registers are written indirectly: $8000-$9FFF selects a command
/// (0-15) and $A000-$BFFF writes its parameter. Commands 0-7 select the 1 KB CHR
/// banks, 8 controls $6000-$7FFF, 9-11 select the 8 KB PRG banks at $8000/$A000/$C000
/// ($E000 is fixed to the last bank), 12 sets mirroring and 13-15 drive a 16-bit
/// CPU-cycle IRQ counter. $C000-$DFFF selects a Sunsoft 5B audio register (0-15) and
/// $E000-$FFFF writes its value.
#[derive(Clone)]
pub struct Fme7 {
    /// Shared PRG/CHR/nametable state; bank commands are applied directly to it.
    base: CartBase,
    /// Command number latched by the last $8000-$9FFF write (low 4 bits); selects
    /// what the next $A000-$BFFF parameter write does.
    command: u8,
    /// Command 8 value for $6000-$7FFF: bits 0-5 = bank, bit 6 = RAM (1) / ROM (0),
    /// bit 7 = RAM enable (when RAM is selected but disabled the area is open bus).
    wram_reg: u8,
    /// Command 13 bit 0: whether the counter wrapping asserts an IRQ.
    irq_enabled: bool,
    /// Command 13 bit 7: whether the counter decrements every CPU cycle.
    irq_counter_enabled: bool,
    /// 16-bit down-counter; low byte from command 14, high byte from command 15.
    /// The IRQ fires when it wraps from $0000 to $FFFF.
    irq_counter: u16,
    /// IRQ line state; set on counter wrap (if enabled), cleared by any command 13 write.
    irq_pending: bool,
    /// Selected Sunsoft 5B audio register ($C000-$DFFF, bits 0-3).
    audio_reg: u8,
    /// Sunsoft 5B YM2149F register file (16 bytes).
    audio_regs: [u8; 16],
    /// Sub-divider counting every 16 CPU cycles (2x CPU divider * 8 YM2149F prescaler).
    psg_div: u8,
    /// Down-counters for the three 12-bit tone generators (A, B, C).
    tone_timers: [u16; 3],
    /// Output polarity (0 or 1) of each tone generator.
    tone_step: [u8; 3],
    /// Noise generator down-counter (clocked every 32 CPU cycles).
    noise_timer: u8,
    /// Toggle for the 2x noise prescaler relative to `psg_div`.
    noise_div: bool,
    /// 17-bit noise LFSR (initialized to 1).
    noise_lfsr: u32,
}

impl Fme7 {
    /// Grows PRG-RAM to at least 32 KB (four 8 KB banks for command 8 to select),
    /// fixes the last PRG-ROM bank at $E000 and applies the initial $6000 mapping
    /// (`wram_reg` = 0, i.e. PRG-ROM bank 0).
    pub fn new(mut base: CartBase) -> Self {
        if base.prg_ram.len() < 0x8000 {
            let mut ram = base.prg_ram.clone();
            ram.resize(0x8000, 0);
            base.prg_ram = ram;
        }
        base.set_prg_8k(4, -1);
        let mut audio_regs = [0u8; 16];
        audio_regs[7] = 0x3F; // All tone/noise channels disabled at power-on.
        let mut m = Self {
            base,
            command: 0,
            wram_reg: 0,
            irq_enabled: false,
            irq_counter_enabled: false,
            irq_counter: 0,
            irq_pending: false,
            audio_reg: 0,
            audio_regs,
            psg_div: 0,
            tone_timers: [0; 3],
            tone_step: [0; 3],
            noise_timer: 0,
            noise_div: false,
            noise_lfsr: 1,
        };
        m.update_wram();
        m
    }

    /// Applies `wram_reg` to the $6000-$7FFF slot: an 8 KB PRG-RAM bank (writable),
    /// open bus, or an 8 KB PRG-ROM bank, depending on bits 6-7.
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
            0xC000 => self.audio_reg = val & 0x0F,
            0xE000 => self.audio_regs[self.audio_reg as usize] = val,
            _ => {}
        }
    }

    fn wants_cpu_clock(&self) -> bool {
        true
    }

    fn cpu_clock(&mut self) {
        if self.irq_counter_enabled {
            self.irq_counter = self.irq_counter.wrapping_sub(1);
            if self.irq_counter == 0xFFFF && self.irq_enabled {
                self.irq_pending = true;
            }
        }
        self.psg_div = (self.psg_div + 1) & 15;
        if self.psg_div == 0 {
            for ch in 0..3 {
                if self.tone_timers[ch] == 0 {
                    let period = (self.audio_regs[ch * 2] as u16)
                        | (((self.audio_regs[ch * 2 + 1] & 0x0F) as u16) << 8);
                    self.tone_timers[ch] = period.max(1);
                    self.tone_step[ch] ^= 1;
                } else {
                    self.tone_timers[ch] -= 1;
                }
            }
            self.noise_div = !self.noise_div;
            if self.noise_div {
                if self.noise_timer == 0 {
                    self.noise_timer = (self.audio_regs[6] & 0x1F).max(1);
                    let bit = (self.noise_lfsr ^ (self.noise_lfsr >> 3)) & 1;
                    self.noise_lfsr = (self.noise_lfsr >> 1) | (bit << 16);
                } else {
                    self.noise_timer -= 1;
                }
            }
        }
    }

    fn has_expansion_audio(&self) -> bool {
        true
    }

    fn audio_sample(&self) -> f32 {
        let mix = self.audio_regs[7];
        let noise_out = (self.noise_lfsr & 1) as u8;
        let mut sample = 0.0f32;
        for ch in 0..3 {
            let tone_dis = (mix >> ch) & 1 != 0;
            let noise_dis = (mix >> (ch + 3)) & 1 != 0;
            let gate = (tone_dis || self.tone_step[ch] != 0) && (noise_dis || noise_out != 0);
            if gate {
                let vol = (self.audio_regs[8 + ch] & 0x0F) as usize;
                sample -= SUNSOFT_5B_VOL[vol];
            }
        }
        sample
    }

    fn irq(&self) -> bool {
        self.irq_pending
    }
}
