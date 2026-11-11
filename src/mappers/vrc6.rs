// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones

//! Mappers 24 and 26: Konami VRC6a / VRC6b (*Akumajou Densetsu*, *Madara*, *Esper Dream 2*),
//! including PRG/CHR banking, scanline/cycle IRQ counter, and VRC6 expansion audio
//! (two variable-duty pulse channels plus the 6-bit sawtooth channel).

use super::{CartBase, Mapper};
use crate::cartridge::Mirroring;

/// One Konami VRC6 pulse channel ($9000-$9002 or $A000-$A002).
#[derive(Clone, Copy, Default)]
struct Vrc6Pulse {
    /// Channel enabled ($9002/$A002 bit 7).
    enabled: bool,
    /// 4-bit volume ($9000/$A000 bits 0-3).
    volume: u8,
    /// 3-bit duty threshold ($9000/$A000 bits 4-6; duty step <= duty outputs `volume`).
    duty: u8,
    /// Ignore duty cycle ($9000/$A000 bit 7): always output `volume` while enabled.
    ignore_duty: bool,
    /// 12-bit timer period.
    period: u16,
    /// Timer countdown in CPU cycles.
    timer: u16,
    /// 4-bit duty sequencer (0-15, counting down).
    step: u8,
}

impl Vrc6Pulse {
    /// Handles a write to register 0, 1, or 2 of the pulse channel.
    fn write(&mut self, reg: u16, val: u8) {
        match reg {
            0 => {
                self.volume = val & 0x0F;
                self.duty = (val >> 4) & 0x07;
                self.ignore_duty = val & 0x80 != 0;
            }
            1 => {
                self.period = (self.period & 0x0F00) | val as u16;
            }
            2 => {
                self.period = (self.period & 0x00FF) | (((val & 0x0F) as u16) << 8);
                self.enabled = val & 0x80 != 0;
                if !self.enabled {
                    self.step = 15;
                }
            }
            _ => {}
        }
    }

    /// Clocks the pulse channel by one CPU cycle.
    #[inline]
    fn clock(&mut self, freq_shift: u8) {
        if !self.enabled {
            return;
        }
        if self.timer == 0 {
            self.timer = self.period >> freq_shift;
            self.step = self.step.wrapping_sub(1) & 0x0F;
        } else {
            self.timer -= 1;
        }
    }

    /// Current 4-bit output level (0-15).
    #[inline]
    fn output(&self) -> u8 {
        if !self.enabled {
            0
        } else if self.ignore_duty || self.step <= self.duty {
            self.volume
        } else {
            0
        }
    }
}

/// Konami VRC6 sawtooth channel ($B000-$B002).
#[derive(Clone, Copy, Default)]
struct Vrc6Saw {
    /// Channel enabled ($B002 bit 7).
    enabled: bool,
    /// 6-bit accumulator step rate ($B000 bits 0-5).
    rate: u8,
    /// 8-bit phase accumulator; top 5 bits (`accum >> 3`) form the output.
    accum: u8,
    /// 12-bit timer period.
    period: u16,
    /// Timer countdown in CPU cycles.
    timer: u16,
    /// 14-step sequencer (0-13); `accum` adds `rate` on even steps and resets on wrap to 0.
    step: u8,
}

impl Vrc6Saw {
    /// Handles a write to register 0, 1, or 2 of the sawtooth channel.
    fn write(&mut self, reg: u16, val: u8) {
        match reg {
            0 => self.rate = val & 0x3F,
            1 => self.period = (self.period & 0x0F00) | val as u16,
            2 => {
                self.period = (self.period & 0x00FF) | (((val & 0x0F) as u16) << 8);
                self.enabled = val & 0x80 != 0;
                if !self.enabled {
                    self.accum = 0;
                    self.step = 0;
                }
            }
            _ => {}
        }
    }

    /// Clocks the sawtooth channel by one CPU cycle.
    #[inline]
    fn clock(&mut self, freq_shift: u8) {
        if !self.enabled {
            return;
        }
        if self.timer == 0 {
            self.timer = self.period >> freq_shift;
            self.step = (self.step + 1) % 14;
            if self.step == 0 {
                self.accum = 0;
            } else if self.step & 1 == 0 {
                self.accum = self.accum.wrapping_add(self.rate);
            }
        } else {
            self.timer -= 1;
        }
    }

    /// Current 5-bit output level (0-31).
    #[inline]
    fn output(&self) -> u8 {
        if self.enabled { self.accum >> 3 } else { 0 }
    }
}

/// Konami VRC6 board (Mapper 24 = VRC6a, Mapper 26 = VRC6b with A0/A1 swapped).
#[derive(Clone)]
pub struct Vrc6 {
    /// Shared PRG/CHR/nametable state.
    base: CartBase,
    /// True for Mapper 26 (VRC6b), where address lines A0 and A1 are swapped.
    swap_a0_a1: bool,
    /// Eight 1 KB CHR bank registers ($D000-$D003, $E000-$E003).
    chr_regs: [u8; 8],
    /// Banking/mirroring control register ($B003).
    banking_mode: u8,
    /// IRQ reload value ($F000).
    irq_reload: u8,
    /// Current 8-bit IRQ counter.
    irq_counter: u8,
    /// Prescaler for scanline IRQ mode (counts down from 341 by 3 each CPU cycle).
    irq_prescaler: i16,
    /// IRQ enabled ($F001 bit 1).
    irq_enabled: bool,
    /// IRQ enable-after-acknowledge latch ($F001 bit 0).
    irq_enabled_after_ack: bool,
    /// IRQ cycle mode ($F001 bit 2: `true` = clock every CPU cycle, `false` = prescaled).
    irq_cycle_mode: bool,
    /// IRQ line asserted.
    irq_pending: bool,
    /// Audio frequency control ($9003: bit 0 halts audio, bit 1 = 16x speed, bit 2 = 256x speed).
    audio_freq_ctrl: u8,
    /// VRC6 expansion Pulse 1 ($9000-$9002).
    pulse1: Vrc6Pulse,
    /// VRC6 expansion Pulse 2 ($A000-$A002).
    pulse2: Vrc6Pulse,
    /// VRC6 expansion Sawtooth ($B000-$B002).
    saw: Vrc6Saw,
}

impl Vrc6 {
    /// Creates a VRC6 board with the last 8 KB PRG bank fixed at $E000.
    pub fn new(mut base: CartBase, swap_a0_a1: bool) -> Self {
        base.set_prg_16k(0, 0);
        base.set_prg_8k(3, 0);
        base.set_prg_8k(4, -1);
        Self {
            base,
            swap_a0_a1,
            chr_regs: [0; 8],
            banking_mode: 0,
            irq_reload: 0,
            irq_counter: 0,
            irq_prescaler: 341,
            irq_enabled: false,
            irq_enabled_after_ack: false,
            irq_cycle_mode: false,
            irq_pending: false,
            audio_freq_ctrl: 0,
            pulse1: Vrc6Pulse::default(),
            pulse2: Vrc6Pulse::default(),
            saw: Vrc6Saw::default(),
        }
    }

    /// Updates the eight 1 KB CHR slots and nametable mirroring from `banking_mode` and `chr_regs`.
    fn update_chr_and_mirroring(&mut self) {
        let mode = self.banking_mode & 3;
        let mask = if self.banking_mode & 0x20 != 0 { 0xFE } else { 0xFF };
        let or_lo = if self.banking_mode & 0x20 != 0 { 1 } else { 0 };
        match mode {
            0 => {
                for i in 0..8 {
                    self.base.set_chr_1k(i, self.chr_regs[i] as usize);
                }
            }
            1 => {
                for i in 0..4 {
                    let r = self.chr_regs[i] as usize;
                    self.base.set_chr_1k(i * 2, r & mask as usize);
                    self.base.set_chr_1k(i * 2 + 1, (r & mask as usize) | or_lo);
                }
            }
            _ => {
                for i in 0..4 {
                    self.base.set_chr_1k(i, self.chr_regs[i] as usize);
                }
                for i in 0..2 {
                    let r = self.chr_regs[4 + i] as usize;
                    self.base.set_chr_1k(4 + i * 2, r & mask as usize);
                    self.base.set_chr_1k(4 + i * 2 + 1, (r & mask as usize) | or_lo);
                }
            }
        }
        let m = match (self.banking_mode >> 2) & 3 {
            0 => Mirroring::Vertical,
            1 => Mirroring::Horizontal,
            2 => Mirroring::OneScreenLower,
            _ => Mirroring::OneScreenUpper,
        };
        self.base.set_mirroring(m);
    }

    /// Advances the 8-bit IRQ counter by one tick.
    #[inline]
    fn clock_irq_counter(&mut self) {
        if self.irq_counter == 0xFF {
            self.irq_counter = self.irq_reload;
            self.irq_pending = true;
        } else {
            self.irq_counter = self.irq_counter.wrapping_add(1);
        }
    }
}

impl Mapper for Vrc6 {
    fn base(&self) -> &CartBase {
        &self.base
    }
    fn base_mut(&mut self) -> &mut CartBase {
        &mut self.base
    }
    fn name(&self) -> &'static str {
        if self.swap_a0_a1 { "VRC6b (26)" } else { "VRC6a (24)" }
    }

    fn write_register(&mut self, addr: u16, val: u8, _cycle: u64) {
        let addr = if self.swap_a0_a1 {
            (addr & 0xFFFC) | ((addr & 1) << 1) | ((addr >> 1) & 1)
        } else {
            addr
        };
        match addr & 0xF003 {
            0x8000..=0x8003 => self.base.set_prg_16k(0, (val & 0x0F) as i32),
            0x9000..=0x9002 => self.pulse1.write(addr & 3, val),
            0x9003 => self.audio_freq_ctrl = val,
            0xA000..=0xA002 => self.pulse2.write(addr & 3, val),
            0xB000..=0xB002 => self.saw.write(addr & 3, val),
            0xB003 => {
                self.banking_mode = val;
                let ram_en = val & 0x80 != 0;
                self.base.set_prg_ram_8k(0, 0, ram_en);
                self.update_chr_and_mirroring();
            }
            0xC000..=0xC003 => self.base.set_prg_8k(3, (val & 0x1F) as i32),
            0xD000..=0xD003 => {
                self.chr_regs[(addr & 3) as usize] = val;
                self.update_chr_and_mirroring();
            }
            0xE000..=0xE003 => {
                self.chr_regs[4 + (addr & 3) as usize] = val;
                self.update_chr_and_mirroring();
            }
            0xF000 => self.irq_reload = val,
            0xF001 => {
                self.irq_enabled_after_ack = val & 0x01 != 0;
                self.irq_enabled = val & 0x02 != 0;
                self.irq_cycle_mode = val & 0x04 != 0;
                self.irq_pending = false;
                if self.irq_enabled {
                    self.irq_counter = self.irq_reload;
                    self.irq_prescaler = 341;
                }
            }
            0xF002 => {
                self.irq_pending = false;
                self.irq_enabled = self.irq_enabled_after_ack;
            }
            _ => {}
        }
    }

    fn wants_cpu_clock(&self) -> bool {
        true
    }

    fn cpu_clock(&mut self) {
        if self.irq_enabled {
            if self.irq_cycle_mode {
                self.clock_irq_counter();
            } else {
                self.irq_prescaler -= 3;
                if self.irq_prescaler <= 0 {
                    self.irq_prescaler += 341;
                    self.clock_irq_counter();
                }
            }
        }
        if self.audio_freq_ctrl & 0x01 == 0 {
            let shift = if self.audio_freq_ctrl & 0x04 != 0 {
                8
            } else if self.audio_freq_ctrl & 0x02 != 0 {
                4
            } else {
                0
            };
            self.pulse1.clock(shift);
            self.pulse2.clock(shift);
            self.saw.clock(shift);
        }
    }

    fn has_expansion_audio(&self) -> bool {
        true
    }

    fn audio_sample(&self) -> f32 {
        let sum = self.pulse1.output() + self.pulse2.output() + self.saw.output();
        -(sum as f32) * 0.0065
    }

    fn irq(&self) -> bool {
        self.irq_pending
    }
}
