//! Multi-mapper hardware abstraction supporting NROM (0), MMC1 (1), UxROM (2),
//! CNROM (3), MMC3 (4, 118, 119), AxROM (7), MMC2 (9 - Punch-Out!!), BNROM (34),
//! GxROM (66), and Sunsoft FME-7 (69).

use std::cell::Cell;

use crate::cartridge::{Cartridge, Mirroring};

#[derive(Clone, Debug)]
pub enum MapperVariant {
    /// Mapper 0: NROM (Super Mario Bros., Donkey Kong, Excitebike, Ice Climber)
    Nrom,
    /// Mapper 1: Nintendo MMC1 / SxROM (Abadox, Zelda, Metroid, Mega Man 2, Tetris)
    Mmc1 {
        shift_reg: u8,
        shift_count: u8,
        control: u8,
        chr_bank0: u8,
        chr_bank1: u8,
        prg_bank: u8,
        last_write_cycle: u64,
    },
    /// Mapper 2: UxROM (Contra, Castlevania, Mega Man, DuckTales, Ghosts 'n Goblins)
    UxRom { prg_bank: u8 },
    /// Mapper 3: CNROM (Gradius, Arkanoid, Paperboy, Solomon's Key)
    CnRom { chr_bank: u8 },
    /// Mapper 4 / 118 / 119: Nintendo MMC3 / TxROM (Super Mario Bros. 2 & 3, Mega Man 3-6, Kirby)
    Mmc3 {
        bank_select: u8,
        regs: [u8; 8],
        irq_latch: u8,
        irq_counter: u8,
        irq_reload: bool,
        irq_enabled: bool,
        irq_pending: bool,
    },
    /// Mapper 7: AxROM (Battletoads, Marble Madness, Wizards & Warriors, RC Pro-Am)
    AxRom { prg_bank: u8 },
    /// Mapper 9: Nintendo MMC2 / PxROM (Mike Tyson's Punch-Out!!)
    Mmc2 {
        prg_bank: u8,
        chr_fd0: u8,
        chr_fe0: u8,
        chr_fd1: u8,
        chr_fe1: u8,
        latch0_fe: Cell<bool>,
        latch1_fe: Cell<bool>,
    },
    /// Mapper 34 / 66: BNROM / GxROM (Deadly Towers, Super Mario Bros. + Duck Hunt, Gumshoe)
    GxRom { prg_bank: u8, chr_bank: u8 },
    /// Mapper 69: Sunsoft FME-7 / 5B (Batman: Return of the Joker)
    Fme7 {
        command: u8,
        chr_banks: [u8; 8],
        prg_banks: [u8; 4],
        irq_enabled: bool,
        irq_counter_enabled: bool,
        irq_counter: u16,
        irq_pending: bool,
    },
}

#[derive(Clone, Debug)]
pub struct Mapper {
    pub cart: Cartridge,
    pub mirroring: Mirroring,
    pub variant: MapperVariant,
}

impl Mapper {
    pub fn from_cartridge(cart: Cartridge) -> Self {
        let mirroring = cart.initial_mirroring;
        let variant = match cart.mapper_id {
            0 => MapperVariant::Nrom,
            1 | 105 => MapperVariant::Mmc1 {
                shift_reg: 0x10,
                shift_count: 0,
                control: 0x0C,
                chr_bank0: 0,
                chr_bank1: 0,
                prg_bank: 0,
                last_write_cycle: u64::MAX,
            },
            2 => MapperVariant::UxRom { prg_bank: 0 },
            3 => MapperVariant::CnRom { chr_bank: 0 },
            4 | 47 | 118 | 119 => MapperVariant::Mmc3 {
                bank_select: 0,
                regs: [0, 2, 4, 5, 6, 7, 0, 1],
                irq_latch: 0,
                irq_counter: 0,
                irq_reload: false,
                irq_enabled: false,
                irq_pending: false,
            },
            7 => MapperVariant::AxRom { prg_bank: 0 },
            9 => MapperVariant::Mmc2 {
                prg_bank: 0,
                chr_fd0: 0,
                chr_fe0: 0,
                chr_fd1: 0,
                chr_fe1: 0,
                latch0_fe: Cell::new(false),
                latch1_fe: Cell::new(false),
            },
            34 | 66 => MapperVariant::GxRom {
                prg_bank: 0,
                chr_bank: 0,
            },
            69 => MapperVariant::Fme7 {
                command: 0,
                chr_banks: [0; 8],
                prg_banks: [0; 4],
                irq_enabled: false,
                irq_counter_enabled: false,
                irq_counter: 0,
                irq_pending: false,
            },
            _ => MapperVariant::Nrom,
        };
        Self {
            cart,
            mirroring,
            variant,
        }
    }

    pub fn mapper_name(&self) -> &'static str {
        match self.cart.mapper_id {
            0 => "NROM (0)",
            1 | 105 => "MMC1 (1)",
            2 => "UxROM (2)",
            3 => "CNROM (3)",
            4 | 47 | 118 | 119 => "MMC3 (4)",
            7 => "AxROM (7)",
            9 => "MMC2 (9)",
            34 => "BNROM (34)",
            66 => "GxROM (66)",
            69 => "FME-7 (69)",
            _ => "Generic",
        }
    }

    #[inline]
    pub fn irq_pending(&self) -> bool {
        match &self.variant {
            MapperVariant::Mmc3 { irq_pending, .. } | MapperVariant::Fme7 { irq_pending, .. } => {
                *irq_pending
            }
            _ => false,
        }
    }

    #[inline]
    pub fn clock_scanline_irq(&mut self) {
        if let MapperVariant::Mmc3 {
            irq_latch,
            irq_counter,
            irq_reload,
            irq_enabled,
            irq_pending,
            ..
        } = &mut self.variant
        {
            if *irq_counter == 0 || *irq_reload {
                *irq_counter = *irq_latch;
                *irq_reload = false;
            } else {
                *irq_counter -= 1;
            }
            if *irq_counter == 0 && *irq_enabled {
                *irq_pending = true;
            }
        }
    }

    #[inline]
    pub fn read_prg(&self, addr: u16) -> u8 {
        if (0x6000..=0x7FFF).contains(&addr) {
            if let MapperVariant::Fme7 { prg_banks, .. } = &self.variant
                && (prg_banks[0] & 0x40) == 0 {
                    let prg_len = self.cart.prg_rom.len();
                    let num_8k = (prg_len / 8192).max(1);
                    let b = ((prg_banks[0] & 0x3F) as usize) % num_8k;
                    return self.cart.prg_rom[b * 8192 + ((addr - 0x6000) as usize)];
                }
            return self.cart.prg_ram[(addr - 0x6000) as usize];
        }
        if addr < 0x8000 {
            return 0;
        }

        let prg_len = self.cart.prg_rom.len();
        let num_32k = (prg_len / 32768).max(1);
        let num_16k = (prg_len / 16384).max(1);
        let num_8k = (prg_len / 8192).max(1);

        let rom_offset = match &self.variant {
            MapperVariant::Nrom | MapperVariant::CnRom { .. } => {
                ((addr - 0x8000) as usize) % prg_len
            }
            MapperVariant::Mmc1 {
                control, prg_bank, ..
            } => {
                let prg_mode = (*control >> 2) & 0x03;
                let bank = (*prg_bank as usize) & 0x0F;
                match prg_mode {
                    0 | 1 => {
                        let b32 = (bank & !1) % num_16k;
                        b32 * 16384 + ((addr - 0x8000) as usize)
                    }
                    2 => {
                        if addr < 0xC000 {
                            (addr - 0x8000) as usize
                        } else {
                            (bank % num_16k) * 16384 + ((addr - 0xC000) as usize)
                        }
                    }
                    _ => {
                        if addr < 0xC000 {
                            (bank % num_16k) * 16384 + ((addr - 0x8000) as usize)
                        } else {
                            (num_16k - 1) * 16384 + ((addr - 0xC000) as usize)
                        }
                    }
                }
            }
            MapperVariant::UxRom { prg_bank } => {
                if addr < 0xC000 {
                    ((*prg_bank as usize) % num_16k) * 16384 + ((addr - 0x8000) as usize)
                } else {
                    (num_16k - 1) * 16384 + ((addr - 0xC000) as usize)
                }
            }
            MapperVariant::Mmc3 {
                bank_select, regs, ..
            } => {
                let prg_invert = (*bank_select & 0x40) != 0;
                let slot = ((addr - 0x8000) / 0x2000) as usize;
                let bank_8k = match (slot, prg_invert) {
                    (0, false) => (regs[6] as usize) % num_8k,
                    (0, true) => num_8k.saturating_sub(2),
                    (1, _) => (regs[7] as usize) % num_8k,
                    (2, false) => num_8k.saturating_sub(2),
                    (2, true) => (regs[6] as usize) % num_8k,
                    _ => num_8k - 1,
                };
                bank_8k * 8192 + ((addr as usize) & 0x1FFF)
            }
            MapperVariant::AxRom { prg_bank } => {
                let b32 = ((*prg_bank & 0x07) as usize) % num_32k;
                b32 * 32768 + ((addr - 0x8000) as usize)
            }
            MapperVariant::Mmc2 { prg_bank, .. } => {
                let slot = ((addr - 0x8000) / 0x2000) as usize;
                let bank_8k = match slot {
                    0 => (*prg_bank as usize) % num_8k,
                    1 => num_8k.saturating_sub(3),
                    2 => num_8k.saturating_sub(2),
                    _ => num_8k - 1,
                };
                bank_8k * 8192 + ((addr as usize) & 0x1FFF)
            }
            MapperVariant::GxRom { prg_bank, .. } => {
                let b32 = (*prg_bank as usize) % num_32k;
                b32 * 32768 + ((addr - 0x8000) as usize)
            }
            MapperVariant::Fme7 { prg_banks, .. } => {
                let slot = ((addr - 0x8000) / 0x2000) as usize;
                let bank_8k = match slot {
                    0 => ((prg_banks[1] & 0x3F) as usize) % num_8k,
                    1 => ((prg_banks[2] & 0x3F) as usize) % num_8k,
                    2 => ((prg_banks[3] & 0x3F) as usize) % num_8k,
                    _ => num_8k - 1,
                };
                bank_8k * 8192 + ((addr as usize) & 0x1FFF)
            }
        };

        self.cart.prg_rom[rom_offset % prg_len]
    }

    pub fn write_prg(&mut self, addr: u16, val: u8, cpu_cycle: u64) {
        if (0x6000..=0x7FFF).contains(&addr) {
            self.cart.prg_ram[(addr - 0x6000) as usize] = val;
            return;
        }
        if addr < 0x8000 {
            return;
        }

        let mapper_id = self.cart.mapper_id;

        match &mut self.variant {
            MapperVariant::Nrom => {}
            MapperVariant::Mmc1 {
                shift_reg,
                shift_count,
                control,
                chr_bank0,
                chr_bank1,
                prg_bank,
                last_write_cycle,
            } => {
                if cpu_cycle == last_write_cycle.wrapping_add(1) {
                    *last_write_cycle = cpu_cycle;
                    return;
                }
                *last_write_cycle = cpu_cycle;

                if (val & 0x80) != 0 {
                    *shift_reg = 0x10;
                    *shift_count = 0;
                    *control |= 0x0C;
                    Self::update_mmc1_mirroring(*control, &mut self.mirroring);
                    return;
                }

                *shift_reg = (*shift_reg >> 1) | ((val & 1) << 4);
                *shift_count += 1;

                if *shift_count == 5 {
                    let data = *shift_reg & 0x1F;
                    match (addr >> 13) & 0x03 {
                        0 => {
                            *control = data;
                            Self::update_mmc1_mirroring(*control, &mut self.mirroring);
                        }
                        1 => *chr_bank0 = data,
                        2 => *chr_bank1 = data,
                        3 => *prg_bank = data & 0x0F,
                        _ => unreachable!(),
                    }
                    *shift_reg = 0x10;
                    *shift_count = 0;
                }
            }
            MapperVariant::UxRom { prg_bank } => {
                *prg_bank = val;
            }
            MapperVariant::CnRom { chr_bank } => {
                *chr_bank = val & 0x03;
            }
            MapperVariant::Mmc3 {
                bank_select,
                regs,
                irq_latch,
                irq_reload,
                irq_enabled,
                irq_pending,
                ..
            } => match addr & 0xE001 {
                0x8000 => *bank_select = val,
                0x8001 => {
                    let reg_idx = (*bank_select & 0x07) as usize;
                    regs[reg_idx] = if reg_idx < 2 { val & 0xFE } else { val };
                }
                0xA000 => {
                    self.mirroring = if (val & 0x01) != 0 {
                        Mirroring::Horizontal
                    } else {
                        Mirroring::Vertical
                    };
                }
                0xA001 => {}
                0xC000 => *irq_latch = val,
                0xC001 => *irq_reload = true,
                0xE000 => {
                    *irq_enabled = false;
                    *irq_pending = false;
                }
                0xE001 => *irq_enabled = true,
                _ => {}
            },
            MapperVariant::AxRom { prg_bank } => {
                *prg_bank = val & 0x07;
                self.mirroring = if (val & 0x10) != 0 {
                    Mirroring::OneScreenUpper
                } else {
                    Mirroring::OneScreenLower
                };
            }
            MapperVariant::Mmc2 {
                prg_bank,
                chr_fd0,
                chr_fe0,
                chr_fd1,
                chr_fe1,
                ..
            } => match addr & 0xF000 {
                0xA000 => *prg_bank = val & 0x0F,
                0xB000 => *chr_fd0 = val & 0x1F,
                0xC000 => *chr_fe0 = val & 0x1F,
                0xD000 => *chr_fd1 = val & 0x1F,
                0xE000 => *chr_fe1 = val & 0x1F,
                0xF000 => {
                    self.mirroring = if (val & 0x01) != 0 {
                        Mirroring::Horizontal
                    } else {
                        Mirroring::Vertical
                    };
                }
                _ => {}
            },
            MapperVariant::GxRom { prg_bank, chr_bank } => {
                if mapper_id == 34 {
                    *prg_bank = val;
                } else {
                    *prg_bank = (val >> 4) & 0x03;
                    *chr_bank = val & 0x03;
                }
            }
            MapperVariant::Fme7 {
                command,
                chr_banks,
                prg_banks,
                irq_enabled,
                irq_counter_enabled,
                irq_counter,
                irq_pending,
            } => match addr & 0xE000 {
                0x8000 => *command = val & 0x0F,
                0xA000 => match *command {
                    0..=7 => chr_banks[*command as usize] = val,
                    8..=11 => prg_banks[(*command - 8) as usize] = val,
                    12 => {
                        self.mirroring = match val & 0x03 {
                            0 => Mirroring::Vertical,
                            1 => Mirroring::Horizontal,
                            2 => Mirroring::OneScreenLower,
                            _ => Mirroring::OneScreenUpper,
                        };
                    }
                    13 => {
                        *irq_enabled = (val & 0x01) != 0;
                        *irq_counter_enabled = (val & 0x80) != 0;
                        *irq_pending = false;
                    }
                    14 => *irq_counter = (*irq_counter & 0xFF00) | (val as u16),
                    15 => *irq_counter = (*irq_counter & 0x00FF) | ((val as u16) << 8),
                    _ => {}
                },
                _ => {}
            },
        }
    }

    #[inline]
    fn update_mmc1_mirroring(control: u8, mirroring: &mut Mirroring) {
        *mirroring = match control & 0x03 {
            0 => Mirroring::OneScreenLower,
            1 => Mirroring::OneScreenUpper,
            2 => Mirroring::Vertical,
            _ => Mirroring::Horizontal,
        };
    }

    #[inline]
    fn resolve_chr_offset(&self, addr: u16) -> usize {
        let chr_len = self.cart.chr_mem.len();
        if self.cart.chr_is_ram || chr_len <= 8192 {
            return (addr as usize) & (chr_len - 1);
        }

        let num_8k = (chr_len / 8192).max(1);
        let num_4k = (chr_len / 4096).max(1);
        let num_1k = (chr_len / 1024).max(1);

        let offset = match &self.variant {
            MapperVariant::Nrom | MapperVariant::UxRom { .. } | MapperVariant::AxRom { .. } => {
                (addr as usize) & 0x1FFF
            }
            MapperVariant::Mmc1 {
                control,
                chr_bank0,
                chr_bank1,
                ..
            } => {
                let chr_mode = (*control >> 4) & 1;
                if chr_mode == 0 {
                    let bank8k = ((*chr_bank0 & 0x1E) as usize) % num_4k;
                    bank8k * 4096 + (addr as usize)
                } else if addr < 0x1000 {
                    let b0 = ((*chr_bank0 & 0x1F) as usize) % num_4k;
                    b0 * 4096 + (addr as usize)
                } else {
                    let b1 = ((*chr_bank1 & 0x1F) as usize) % num_4k;
                    b1 * 4096 + ((addr - 0x1000) as usize)
                }
            }
            MapperVariant::CnRom { chr_bank } | MapperVariant::GxRom { chr_bank, .. } => {
                let b8 = (*chr_bank as usize) % num_8k;
                b8 * 8192 + ((addr as usize) & 0x1FFF)
            }
            MapperVariant::Mmc3 {
                bank_select, regs, ..
            } => {
                let chr_invert = (*bank_select & 0x80) != 0;
                let effective_addr = if chr_invert { addr ^ 0x1000 } else { addr };
                let bank_1k = match effective_addr >> 10 {
                    0 => (regs[0] & 0xFE) as usize,
                    1 => ((regs[0] & 0xFE) | 1) as usize,
                    2 => (regs[1] & 0xFE) as usize,
                    3 => ((regs[1] & 0xFE) | 1) as usize,
                    4 => regs[2] as usize,
                    5 => regs[3] as usize,
                    6 => regs[4] as usize,
                    _ => regs[5] as usize,
                };
                (bank_1k % num_1k) * 1024 + ((addr as usize) & 0x03FF)
            }
            MapperVariant::Mmc2 {
                chr_fd0,
                chr_fe0,
                chr_fd1,
                chr_fe1,
                latch0_fe,
                latch1_fe,
                ..
            } => {
                let bank_4k = if addr < 0x1000 {
                    if latch0_fe.get() {
                        *chr_fe0 as usize
                    } else {
                        *chr_fd0 as usize
                    }
                } else if latch1_fe.get() {
                    *chr_fe1 as usize
                } else {
                    *chr_fd1 as usize
                };
                let off = (bank_4k % num_4k) * 4096 + ((addr as usize) & 0x0FFF);
                // MMC2 hardware updates latches AFTER fetching the trigger tile byte
                match addr {
                    0x0FD8 => latch0_fe.set(false),
                    0x0FE8 => latch0_fe.set(true),
                    0x1FD8..=0x1FDF => latch1_fe.set(false),
                    0x1FE8..=0x1FEF => latch1_fe.set(true),
                    _ => {}
                }
                off
            }
            MapperVariant::Fme7 { chr_banks, .. } => {
                let slot = (addr >> 10) as usize;
                let b1k = (chr_banks[slot & 7] as usize) % num_1k;
                b1k * 1024 + ((addr as usize) & 0x03FF)
            }
        };

        offset % chr_len
    }

    #[inline]
    pub fn read_chr(&self, addr: u16) -> u8 {
        let off = self.resolve_chr_offset(addr & 0x1FFF);
        self.cart.chr_mem[off]
    }

    #[inline]
    pub fn write_chr(&mut self, addr: u16, val: u8) {
        if self.cart.chr_is_ram {
            let off = self.resolve_chr_offset(addr & 0x1FFF);
            self.cart.chr_mem[off] = val;
        }
    }
}
