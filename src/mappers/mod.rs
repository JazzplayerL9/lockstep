//! Cartridge mapper framework: a shared bank-mapping base (`CartBase`), the `Mapper`
//! trait the CPU/PPU buses talk to, and the iNES mapper-number factory.

mod axrom;
mod cnrom;
mod cprom;
mod fme7;
mod gxrom;
mod mmc1;
mod mmc2;
mod mmc3;
mod mmc5;
mod nrom;
mod uxrom;

use crate::cartridge::{Cartridge, Mirroring};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrgKind {
    None,
    Rom,
    Ram,
}

#[derive(Clone, Copy, Debug)]
pub struct PrgSlot {
    pub kind: PrgKind,
    pub offset: usize,
    pub writable: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChrKind {
    Rom,
    Ram,
}

/// Memory common to every board, plus 8 KB PRG slots ($6000-$FFFF), 1 KB CHR
/// slots ($0000-$1FFF) and four 1 KB nametable slots pointing into CIRAM.
#[derive(Clone)]
pub struct CartBase {
    pub prg_rom: Vec<u8>,
    pub chr_rom: Vec<u8>,
    pub chr_ram: Vec<u8>,
    pub prg_ram: Vec<u8>,
    /// 2 KB console CIRAM + 2 KB for four-screen boards.
    pub ciram: Vec<u8>,
    /// Index 0 = $6000, 1 = $8000, ... 4 = $E000.
    pub prg: [PrgSlot; 5],
    pub chr: [(ChrKind, usize); 8],
    pub nt: [usize; 4],
    pub mirroring: Mirroring,
    pub has_battery: bool,
    pub save_ram_size: usize,
    pub submapper: u8,
    pub mapper_id: u16,
    pub mmc3_rev_a: bool,
    pub crc32: u32,
}

impl CartBase {
    pub fn new(cart: &Cartridge) -> Self {
        let chr_ram_size = if cart.chr_rom.is_empty() {
            cart.chr_ram_size.max(8192)
        } else {
            cart.chr_ram_size
        };
        let mut b = Self {
            prg_rom: cart.prg_rom.clone(),
            chr_rom: cart.chr_rom.clone(),
            chr_ram: vec![0; chr_ram_size],
            prg_ram: cart.prg_ram.clone(),
            ciram: vec![0; 0x1000],
            prg: [PrgSlot { kind: PrgKind::None, offset: 0, writable: false }; 5],
            chr: [(ChrKind::Rom, 0); 8],
            nt: [0; 4],
            mirroring: cart.initial_mirroring,
            has_battery: cart.has_battery,
            save_ram_size: cart.save_ram_size,
            submapper: cart.submapper,
            mapper_id: cart.mapper_id,
            mmc3_rev_a: cart.mmc3_rev_a,
            crc32: cart.crc32,
        };
        b.set_prg_ram_8k(0, 0, true);
        b.set_prg_32k(0);
        b.set_chr_8k(0);
        b.set_mirroring(cart.initial_mirroring);
        b
    }

    #[inline]
    pub fn prg_rom_banks_8k(&self) -> usize {
        (self.prg_rom.len() / 0x2000).max(1)
    }

    /// Maps an 8 KB PRG-ROM bank into `slot` (1..=4 for $8000..$E000). Negative banks count from the end.
    pub fn set_prg_8k(&mut self, slot: usize, bank: i32) {
        let n = self.prg_rom_banks_8k() as i32;
        let b = bank.rem_euclid(n) as usize;
        self.prg[slot] = PrgSlot { kind: PrgKind::Rom, offset: b * 0x2000, writable: false };
    }

    /// Maps a 16 KB bank at $8000 (`slot16` = 0) or $C000 (`slot16` = 1).
    pub fn set_prg_16k(&mut self, slot16: usize, bank: i32) {
        let n16 = ((self.prg_rom.len() / 0x4000).max(1)) as i32;
        let b = bank.rem_euclid(n16);
        self.set_prg_8k(1 + slot16 * 2, b * 2);
        self.set_prg_8k(2 + slot16 * 2, b * 2 + 1);
    }

    pub fn set_prg_32k(&mut self, bank: i32) {
        let n32 = ((self.prg_rom.len() / 0x8000).max(1)) as i32;
        let b = bank.rem_euclid(n32);
        for i in 0..4 {
            self.set_prg_8k(1 + i, b * 4 + i as i32);
        }
        if self.prg_rom.len() < 0x8000 {
            // NROM-128: mirror the 16 KB bank.
            for i in 0..4 {
                self.set_prg_8k(1 + i, i as i32);
            }
        }
    }

    pub fn set_prg_ram_8k(&mut self, slot: usize, bank: usize, writable: bool) {
        if self.prg_ram.is_empty() {
            self.prg[slot] = PrgSlot { kind: PrgKind::None, offset: 0, writable: false };
            return;
        }
        let n = (self.prg_ram.len() / 0x2000).max(1);
        self.prg[slot] = PrgSlot { kind: PrgKind::Ram, offset: (bank % n) * 0x2000, writable };
    }

    pub fn unmap_prg(&mut self, slot: usize) {
        self.prg[slot] = PrgSlot { kind: PrgKind::None, offset: 0, writable: false };
    }

    #[inline]
    fn chr_kind_default(&self) -> ChrKind {
        if self.chr_rom.is_empty() { ChrKind::Ram } else { ChrKind::Rom }
    }

    pub fn set_chr_1k_kind(&mut self, slot: usize, bank: usize, kind: ChrKind) {
        let len = match kind {
            ChrKind::Rom => self.chr_rom.len(),
            ChrKind::Ram => self.chr_ram.len(),
        };
        let n = (len / 0x400).max(1);
        self.chr[slot] = (kind, (bank % n) * 0x400);
    }

    pub fn set_chr_1k(&mut self, slot: usize, bank: usize) {
        let k = self.chr_kind_default();
        self.set_chr_1k_kind(slot, bank, k);
    }

    pub fn set_chr_2k(&mut self, slot2: usize, bank: usize) {
        self.set_chr_1k(slot2 * 2, bank * 2);
        self.set_chr_1k(slot2 * 2 + 1, bank * 2 + 1);
    }

    pub fn set_chr_4k(&mut self, slot4: usize, bank: usize) {
        for i in 0..4 {
            self.set_chr_1k(slot4 * 4 + i, bank * 4 + i);
        }
    }

    pub fn set_chr_8k(&mut self, bank: usize) {
        for i in 0..8 {
            self.set_chr_1k(i, bank * 8 + i);
        }
    }

    pub fn set_mirroring(&mut self, m: Mirroring) {
        self.mirroring = m;
        self.nt = match m {
            Mirroring::Vertical => [0, 0x400, 0, 0x400],
            Mirroring::Horizontal => [0, 0, 0x400, 0x400],
            Mirroring::OneScreenLower => [0; 4],
            Mirroring::OneScreenUpper => [0x400; 4],
            Mirroring::FourScreen => [0, 0x400, 0x800, 0xC00],
        };
    }

    #[inline]
    pub fn set_nametable(&mut self, index: usize, page: usize) {
        self.nt[index] = (page & 3) * 0x400;
    }

    #[inline]
    pub fn read_prg(&self, addr: u16) -> Option<u8> {
        if addr < 0x6000 {
            return None;
        }
        let slot = &self.prg[((addr - 0x6000) >> 13) as usize];
        let off = slot.offset + (addr as usize & 0x1FFF);
        match slot.kind {
            PrgKind::Rom => Some(self.prg_rom[off % self.prg_rom.len()]),
            PrgKind::Ram => Some(self.prg_ram[off % self.prg_ram.len()]),
            PrgKind::None => None,
        }
    }

    #[inline]
    pub fn write_prg_ram(&mut self, addr: u16, val: u8) {
        if addr < 0x6000 {
            return;
        }
        let slot = self.prg[((addr - 0x6000) >> 13) as usize];
        if slot.kind == PrgKind::Ram && slot.writable {
            let len = self.prg_ram.len();
            self.prg_ram[(slot.offset + (addr as usize & 0x1FFF)) % len] = val;
        }
    }

    #[inline]
    pub fn read_chr(&self, addr: u16) -> u8 {
        let (kind, base) = self.chr[(addr >> 10) as usize & 7];
        let off = base + (addr as usize & 0x3FF);
        match kind {
            ChrKind::Rom => self.chr_rom[off % self.chr_rom.len()],
            ChrKind::Ram => self.chr_ram[off % self.chr_ram.len()],
        }
    }

    #[inline]
    pub fn write_chr(&mut self, addr: u16, val: u8) {
        let (kind, base) = self.chr[(addr >> 10) as usize & 7];
        if kind == ChrKind::Ram {
            let len = self.chr_ram.len();
            self.chr_ram[(base + (addr as usize & 0x3FF)) % len] = val;
        }
    }

    #[inline]
    pub fn read_nt(&self, addr: u16) -> u8 {
        self.ciram[self.nt[((addr >> 10) & 3) as usize] + (addr as usize & 0x3FF)]
    }

    #[inline]
    pub fn write_nt(&mut self, addr: u16, val: u8) {
        let i = self.nt[((addr >> 10) & 3) as usize] + (addr as usize & 0x3FF);
        self.ciram[i] = val;
    }

    /// Default PPU bus read for $0000-$3EFF.
    #[inline]
    pub fn ppu_read(&self, addr: u16) -> u8 {
        if addr < 0x2000 { self.read_chr(addr) } else { self.read_nt(addr) }
    }

    #[inline]
    pub fn ppu_write(&mut self, addr: u16, val: u8) {
        if addr < 0x2000 {
            self.write_chr(addr, val)
        } else {
            self.write_nt(addr, val)
        }
    }
}

/// Bus interface every cartridge board implements.
pub trait Mapper: Send + MapperClone {
    fn base(&self) -> &CartBase;
    fn base_mut(&mut self) -> &mut CartBase;
    fn name(&self) -> &'static str;

    /// CPU read of $4020-$FFFF. `None` means open bus.
    fn cpu_read(&mut self, addr: u16) -> Option<u8> {
        self.base().read_prg(addr)
    }

    /// CPU write of $4020-$FFFF.
    fn cpu_write(&mut self, addr: u16, val: u8, cycle: u64) {
        if addr >= 0x8000 {
            self.write_register(addr, val, cycle);
        } else {
            self.base_mut().write_prg_ram(addr, val);
        }
    }

    /// Register write for $8000-$FFFF on the default `cpu_write` path.
    fn write_register(&mut self, _addr: u16, _val: u8, _cycle: u64) {}

    /// PPU bus read for $0000-$3EFF.
    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.base().ppu_read(addr)
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        self.base_mut().ppu_write(addr, val)
    }

    /// Whether `notify_vram_addr` must be called on every PPU address-bus change.
    fn wants_vram_addr(&self) -> bool {
        false
    }

    /// PPU address bus changed (A12 watchers, MMC2 latches, ...). `cpu_cycle` is the CPU cycle counter.
    fn notify_vram_addr(&mut self, _addr: u16, _cpu_cycle: u64) {}

    /// Called once per CPU cycle (cycle-counting IRQs, MMC5 in-frame detection).
    fn cpu_clock(&mut self) {}

    /// CPU write to a PPU register ($2000-$2007), for boards that snoop them (MMC5).
    fn ppu_register_write(&mut self, _addr: u16, _val: u8) {}

    /// Current state of the cartridge IRQ line.
    fn irq(&self) -> bool {
        false
    }

    /// Expansion audio output, roughly in the same units as the 2A03 mix (0.0..1.0).
    fn audio_sample(&self) -> f32 {
        0.0
    }

    /// Battery-backed memory to persist.
    fn battery_ram(&self) -> Vec<u8> {
        let b = self.base();
        let len = if b.save_ram_size > 0 { b.save_ram_size.min(b.prg_ram.len()) } else { b.prg_ram.len() };
        b.prg_ram[..len].to_vec()
    }
}

pub trait MapperClone {
    fn box_clone(&self) -> Box<dyn Mapper>;
}

impl<T: Mapper + Clone + 'static> MapperClone for T {
    fn box_clone(&self) -> Box<dyn Mapper> {
        Box::new(self.clone())
    }
}

impl Clone for Box<dyn Mapper> {
    fn clone(&self) -> Self {
        self.box_clone()
    }
}

/// Mapper numbers this emulator implements.
pub fn is_supported(mapper_id: u16) -> bool {
    matches!(mapper_id, 0 | 1 | 2 | 3 | 4 | 5 | 7 | 9 | 10 | 11 | 13 | 34 | 47 | 66 | 69 | 71 | 79 | 105 | 118 | 119 | 206)
}

pub fn create_mapper(cart: &Cartridge) -> Result<Box<dyn Mapper>, String> {
    let base = CartBase::new(cart);
    let m: Box<dyn Mapper> = match cart.mapper_id {
        0 => Box::new(nrom::Nrom::new(base)),
        1 => Box::new(mmc1::Mmc1::new(base, false)),
        105 => Box::new(mmc1::Mmc1::new(base, true)),
        2 => Box::new(uxrom::UxRom::new(base, true)),
        71 => Box::new(uxrom::UxRom::new_camerica(base)),
        3 => Box::new(cnrom::CnRom::new(base)),
        4 | 47 | 118 | 119 | 206 => Box::new(mmc3::Mmc3::new(base)),
        5 => Box::new(mmc5::Mmc5::new(base)),
        7 => Box::new(axrom::AxRom::new(base)),
        9 => Box::new(mmc2::Mmc2::new(base, false)),
        10 => Box::new(mmc2::Mmc2::new(base, true)),
        11 | 66 | 34 | 79 => Box::new(gxrom::GxRom::new(base)),
        13 => Box::new(cprom::CpRom::new(base)),
        69 => Box::new(fme7::Fme7::new(base)),
        other => return Err(format!("Unsupported mapper {}", other)),
    };
    Ok(m)
}
