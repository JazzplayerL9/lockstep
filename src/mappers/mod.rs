// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones

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

use std::sync::Arc;

use crate::cartridge::{Cartridge, Mirroring};

/// What a CPU-side 8 KB slot ($6000-$FFFF) is connected to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrgKind {
    /// Nothing: reads return open bus, writes are ignored.
    None,
    /// PRG-ROM.
    Rom,
    /// Cartridge PRG-RAM (work or battery-backed save RAM).
    Ram,
}

/// One 8 KB window of the CPU address space.
#[derive(Clone, Copy, Debug)]
pub struct PrgSlot {
    /// Memory type mapped here.
    pub kind: PrgKind,
    /// Byte offset of the bank within that memory (always in range: reduced at bank-switch time).
    pub offset: usize,
    /// Whether CPU writes reach the RAM (RAM-protect bits clear this).
    pub writable: bool,
}

/// What a PPU-side 1 KB pattern-table slot ($0000-$1FFF) is connected to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChrKind {
    /// CHR-ROM.
    Rom,
    /// CHR-RAM.
    Ram,
}

/// Memory common to every board, plus 8 KB PRG slots ($6000-$FFFF), 1 KB CHR
/// slots ($0000-$1FFF) and four 1 KB nametable slots pointing into CIRAM.
#[derive(Clone)]
pub struct CartBase {
    /// ROM is immutable and shared between save-state / run-ahead snapshots.
    pub prg_rom: Arc<[u8]>,
    /// Character (pattern) ROM; empty for CHR-RAM-only boards.
    pub chr_rom: Arc<[u8]>,
    /// Character RAM (at least 8 KB, so slot offsets are always in range).
    pub chr_ram: Vec<u8>,
    /// PRG-RAM: battery-backed part first (`save_ram_size` bytes), then work RAM.
    pub prg_ram: Vec<u8>,
    /// 2 KB console CIRAM + 2 KB for four-screen boards.
    pub ciram: Vec<u8>,
    /// Index 0 = $6000, 1 = $8000, ... 4 = $E000.
    pub prg: [PrgSlot; 5],
    /// PPU $0000-$1FFF in eight 1 KB slots: (memory, byte offset).
    pub chr: [(ChrKind, usize); 8],
    /// CIRAM byte offset for each of the four nametables (encodes mirroring).
    pub nt: [usize; 4],
    /// Current mirroring mode (kept in sync with `nt`).
    pub mirroring: Mirroring,
    /// Whether PRG-RAM is battery-backed.
    pub has_battery: bool,
    /// Battery-backed bytes at the start of `prg_ram`.
    pub save_ram_size: usize,
    /// NES 2.0 submapper.
    pub submapper: u8,
    /// iNES mapper number.
    pub mapper_id: u16,
    /// MMC3 revision A IRQ behaviour (from the fixup table).
    pub mmc3_rev_a: bool,
    /// CRC32 of PRG+CHR, for board-specific quirks.
    pub crc32: u32,
}

impl CartBase {
    /// Builds the base from a cartridge: 32 KB PRG mapped at $8000, PRG-RAM at $6000,
    /// the first 8 KB of CHR, and the header's mirroring.
    pub fn new(cart: &Cartridge) -> Self {
        let chr_ram_size = if cart.chr_rom.is_empty() {
            cart.chr_ram_size.max(8192)
        } else {
            cart.chr_ram_size
        };
        let mut b = Self {
            prg_rom: cart.prg_rom.clone().into(),
            chr_rom: cart.chr_rom.clone().into(),
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
        // Offsets are pre-reduced at bank-switch time, so reads need no wrap-around;
        // pad memories smaller than one slot so every slot offset stays in range.
        if !b.prg_ram.is_empty() && b.prg_ram.len() < 0x2000 {
            let mut r = b.prg_ram.clone();
            r.resize(0x2000, 0);
            b.prg_ram = r;
        }
        if b.chr_ram.len() < 0x2000 {
            b.chr_ram.resize(0x2000, 0);
        }
        b.set_prg_ram_8k(0, 0, true);
        b.set_prg_32k(0);
        b.set_chr_8k(0);
        b.set_mirroring(cart.initial_mirroring);
        b
    }

    /// Number of 8 KB PRG-ROM banks.
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

    /// Maps a 32 KB PRG bank at $8000 (16 KB ROMs are mirrored).
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

    /// Maps an 8 KB PRG-RAM bank into `slot` (0 = $6000).
    pub fn set_prg_ram_8k(&mut self, slot: usize, bank: usize, writable: bool) {
        if self.prg_ram.is_empty() {
            self.prg[slot] = PrgSlot { kind: PrgKind::None, offset: 0, writable: false };
            return;
        }
        let n = (self.prg_ram.len() / 0x2000).max(1);
        self.prg[slot] = PrgSlot { kind: PrgKind::Ram, offset: (bank % n) * 0x2000, writable };
    }

    /// Disconnects `slot` (open bus).
    pub fn unmap_prg(&mut self, slot: usize) {
        self.prg[slot] = PrgSlot { kind: PrgKind::None, offset: 0, writable: false };
    }

    /// CHR memory used when a board does not say which: ROM if the cartridge has
    /// CHR-ROM, otherwise CHR-RAM.
    #[inline]
    fn chr_kind_default(&self) -> ChrKind {
        if self.chr_rom.is_empty() { ChrKind::Ram } else { ChrKind::Rom }
    }

    /// Maps 1 KB bank `bank` of the given CHR memory into `slot` (0-7).
    pub fn set_chr_1k_kind(&mut self, slot: usize, bank: usize, kind: ChrKind) {
        let len = match kind {
            ChrKind::Rom => self.chr_rom.len(),
            ChrKind::Ram => self.chr_ram.len(),
        };
        let n = (len / 0x400).max(1);
        self.chr[slot] = (kind, (bank % n) * 0x400);
    }

    /// Maps a 1 KB CHR bank (ROM if present, else RAM).
    pub fn set_chr_1k(&mut self, slot: usize, bank: usize) {
        let k = self.chr_kind_default();
        self.set_chr_1k_kind(slot, bank, k);
    }

    /// Maps a 2 KB CHR bank into 2 KB slot `slot2` (0-3).
    pub fn set_chr_2k(&mut self, slot2: usize, bank: usize) {
        self.set_chr_1k(slot2 * 2, bank * 2);
        self.set_chr_1k(slot2 * 2 + 1, bank * 2 + 1);
    }

    /// Maps a 4 KB CHR bank at $0000 (`slot4` = 0) or $1000 (1).
    pub fn set_chr_4k(&mut self, slot4: usize, bank: usize) {
        for i in 0..4 {
            self.set_chr_1k(slot4 * 4 + i, bank * 4 + i);
        }
    }

    /// Maps an 8 KB CHR bank at $0000.
    pub fn set_chr_8k(&mut self, bank: usize) {
        for i in 0..8 {
            self.set_chr_1k(i, bank * 8 + i);
        }
    }

    /// Sets nametable mirroring.
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

    /// Points nametable `index` (0-3) at CIRAM page `page` (per-nametable control, e.g. TxSROM).
    #[inline]
    pub fn set_nametable(&mut self, index: usize, page: usize) {
        self.nt[index] = (page & 3) * 0x400;
    }

    /// CPU read through the PRG slots; `None` = open bus.
    #[inline]
    pub fn read_prg(&self, addr: u16) -> Option<u8> {
        if addr < 0x6000 {
            return None;
        }
        let slot = &self.prg[((addr - 0x6000) >> 13) as usize];
        let off = slot.offset + (addr as usize & 0x1FFF);
        match slot.kind {
            PrgKind::Rom => Some(self.prg_rom[off]),
            PrgKind::Ram => Some(self.prg_ram[off]),
            PrgKind::None => None,
        }
    }

    /// CPU write to $6000-$FFFF; only lands if that slot is writable RAM.
    #[inline]
    pub fn write_prg_ram(&mut self, addr: u16, val: u8) {
        if addr < 0x6000 {
            return;
        }
        let slot = self.prg[((addr - 0x6000) >> 13) as usize];
        if slot.kind == PrgKind::Ram && slot.writable {
            self.prg_ram[slot.offset + (addr as usize & 0x1FFF)] = val;
        }
    }

    /// PPU read of the pattern tables ($0000-$1FFF).
    #[inline]
    pub fn read_chr(&self, addr: u16) -> u8 {
        let (kind, base) = self.chr[(addr >> 10) as usize & 7];
        let off = base + (addr as usize & 0x3FF);
        match kind {
            ChrKind::Rom => self.chr_rom[off],
            ChrKind::Ram => self.chr_ram[off],
        }
    }

    /// PPU write of the pattern tables (only CHR-RAM is writable).
    #[inline]
    pub fn write_chr(&mut self, addr: u16, val: u8) {
        let (kind, base) = self.chr[(addr >> 10) as usize & 7];
        if kind == ChrKind::Ram {
            self.chr_ram[base + (addr as usize & 0x3FF)] = val;
        }
    }

    /// PPU read of a nametable byte ($2000-$2FFF and mirrors) through the mirroring map.
    #[inline]
    pub fn read_nt(&self, addr: u16) -> u8 {
        self.ciram[self.nt[((addr >> 10) & 3) as usize] + (addr as usize & 0x3FF)]
    }

    /// PPU write of a nametable byte.
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

    /// Default PPU bus write for $0000-$3EFF.
    #[inline]
    pub fn ppu_write(&mut self, addr: u16, val: u8) {
        if addr < 0x2000 {
            self.write_chr(addr, val)
        } else {
            self.write_nt(addr, val)
        }
    }

    /// Appends the mutable cartridge base state (`chr_ram`, `prg_ram`, `ciram`, banking slots, mirroring) to `out`.
    pub fn save_base_state(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.mapper_id.to_le_bytes());
        out.extend_from_slice(&self.crc32.to_le_bytes());
        for buf in [&self.chr_ram, &self.prg_ram, &self.ciram] {
            out.extend_from_slice(&(buf.len() as u32).to_le_bytes());
            out.extend_from_slice(buf);
        }
        let banking = CartBaseBanking {
            prg: self.prg,
            chr: self.chr,
            nt: self.nt,
            mirroring: self.mirroring,
        };
        // SAFETY: `CartBaseBanking` is `Copy` plain-old-data read in-process by the same binary.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &banking as *const CartBaseBanking as *const u8,
                std::mem::size_of::<CartBaseBanking>(),
            )
        };
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
    }

    /// Restores the mutable cartridge base state from `cur`.
    pub fn load_base_state(&mut self, cur: &mut &[u8]) -> Result<(), String> {
        let take = |cur: &mut &[u8], n: usize| -> Result<Vec<u8>, String> {
            if cur.len() < n {
                return Err("Truncated save state (CartBase)".into());
            }
            let (head, tail) = cur.split_at(n);
            *cur = tail;
            Ok(head.to_vec())
        };
        let mapper_id = u16::from_le_bytes(take(cur, 2)?.try_into().unwrap());
        let crc32 = u32::from_le_bytes(take(cur, 4)?.try_into().unwrap());
        if mapper_id != self.mapper_id || crc32 != self.crc32 {
            return Err(format!(
                "Save state ROM mismatch (expected mapper {} crc {:08X}, got mapper {} crc {:08X})",
                self.mapper_id, self.crc32, mapper_id, crc32
            ));
        }
        for buf in [&mut self.chr_ram, &mut self.prg_ram, &mut self.ciram] {
            let len = u32::from_le_bytes(take(cur, 4)?.try_into().unwrap()) as usize;
            if len != buf.len() {
                return Err(format!("Save state RAM size mismatch ({} vs {})", len, buf.len()));
            }
            let data = take(cur, len)?;
            buf.copy_from_slice(&data);
        }
        let b_len = u32::from_le_bytes(take(cur, 4)?.try_into().unwrap()) as usize;
        if b_len != std::mem::size_of::<CartBaseBanking>() {
            return Err("Save state CartBaseBanking size mismatch".into());
        }
        let b_bytes = take(cur, b_len)?;
        // SAFETY: `CartBaseBanking` is `Copy` and size was verified above.
        let banking: CartBaseBanking = unsafe {
            std::ptr::read_unaligned(b_bytes.as_ptr() as *const CartBaseBanking)
        };
        self.prg = banking.prg;
        self.chr = banking.chr;
        self.nt = banking.nt;
        self.mirroring = banking.mirroring;
        Ok(())
    }
}

/// Snapshot of `CartBase`'s bank-mapping tables and mirroring mode.
#[repr(C)]
#[derive(Clone, Copy)]
struct CartBaseBanking {
    /// CPU $6000-$FFFF 8 KB slots.
    prg: [PrgSlot; 5],
    /// PPU $0000-$1FFF 1 KB slots.
    chr: [(ChrKind, usize); 8],
    /// Four nametable offsets into CIRAM.
    nt: [usize; 4],
    /// Current mirroring mode.
    mirroring: Mirroring,
}

/// Bus interface every cartridge board implements.
pub trait Mapper: Send + MapperClone {
    /// Shared memory/banking state.
    fn base(&self) -> &CartBase;
    /// Mutable shared memory/banking state.
    fn base_mut(&mut self) -> &mut CartBase;
    /// Human-readable board name, e.g. `"MMC3 (4)"`.
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

    /// PPU bus write for $0000-$3EFF.
    fn ppu_write(&mut self, addr: u16, val: u8) {
        self.base_mut().ppu_write(addr, val)
    }

    /// Whether `notify_vram_addr` must be called on every PPU address-bus change.
    fn wants_vram_addr(&self) -> bool {
        false
    }

    /// PPU address bus changed (A12 watchers, MMC2 latches, ...). `cpu_cycle` is the CPU cycle counter.
    fn notify_vram_addr(&mut self, _addr: u16, _cpu_cycle: u64) {}

    /// Whether `cpu_clock` does anything (it is skipped otherwise; it's on the hottest path).
    fn wants_cpu_clock(&self) -> bool {
        false
    }

    /// Called once per CPU cycle (cycle-counting IRQs, MMC5 in-frame detection).
    fn cpu_clock(&mut self) {}

    /// CPU write to a PPU register ($2000-$2007), for boards that snoop them (MMC5).
    fn ppu_register_write(&mut self, _addr: u16, _val: u8) {}

    /// Current state of the cartridge IRQ line.
    fn irq(&self) -> bool {
        false
    }

    /// Whether the board produces expansion audio (`audio_sample` is skipped otherwise).
    fn has_expansion_audio(&self) -> bool {
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

/// Object-safe cloning and save-state serialization for `Box<dyn Mapper>` (implemented automatically for every `Clone` mapper).
pub trait MapperClone {
    /// Clones the mapper into a new box (used by save states and run-ahead).
    fn box_clone(&self) -> Box<dyn Mapper>;
    /// Serializes the mapper's mutable state (`CartBase` + board registers) into `out`.
    fn save_mapper_state(&self, out: &mut Vec<u8>);
    /// Deserializes the mapper's mutable state (`CartBase` + board registers) from `cur`.
    fn load_mapper_state(&mut self, cur: &mut &[u8]) -> Result<(), String>;
}

impl<T: Mapper + Clone + 'static> MapperClone for T {
    fn box_clone(&self) -> Box<dyn Mapper> {
        Box::new(self.clone())
    }

    fn save_mapper_state(&self, out: &mut Vec<u8>) {
        self.base().save_base_state(out);
        let self_ptr = self as *const T as *const u8;
        let base_ptr = self.base() as *const CartBase as *const u8;
        let base_off = (base_ptr as usize) - (self_ptr as usize);
        let base_end = base_off + std::mem::size_of::<CartBase>();
        let total = std::mem::size_of::<T>();
        let extra = total - std::mem::size_of::<CartBase>();
        out.extend_from_slice(&(extra as u32).to_le_bytes());
        // SAFETY: All mapper fields outside `base: CartBase` are plain-old-data (`Copy`).
        unsafe {
            if base_off > 0 {
                out.extend_from_slice(std::slice::from_raw_parts(self_ptr, base_off));
            }
            if base_end < total {
                out.extend_from_slice(std::slice::from_raw_parts(
                    self_ptr.add(base_end),
                    total - base_end,
                ));
            }
        }
    }

    fn load_mapper_state(&mut self, cur: &mut &[u8]) -> Result<(), String> {
        self.base_mut().load_base_state(cur)?;
        if cur.len() < 4 {
            return Err("Truncated save state (mapper header)".into());
        }
        let (len_bytes, rest) = cur.split_at(4);
        *cur = rest;
        let saved_extra = u32::from_le_bytes(len_bytes.try_into().unwrap()) as usize;
        let self_ptr = self as *mut T as *mut u8;
        let base_ptr = self.base() as *const CartBase as *const u8;
        let base_off = (base_ptr as usize) - (self_ptr as usize);
        let base_end = base_off + std::mem::size_of::<CartBase>();
        let total = std::mem::size_of::<T>();
        let expected_extra = total - std::mem::size_of::<CartBase>();
        if saved_extra != expected_extra || cur.len() < expected_extra {
            return Err(format!(
                "Mapper state size mismatch (expected {} bytes, got {})",
                expected_extra, saved_extra
            ));
        }
        let (data, tail) = cur.split_at(expected_extra);
        *cur = tail;
        // SAFETY: Only the plain-old-data bytes outside `base: CartBase` are overwritten.
        unsafe {
            if base_off > 0 {
                std::ptr::copy_nonoverlapping(data.as_ptr(), self_ptr, base_off);
            }
            if base_end < total {
                std::ptr::copy_nonoverlapping(
                    data.as_ptr().add(base_off),
                    self_ptr.add(base_end),
                    total - base_end,
                );
            }
        }
        Ok(())
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

/// Instantiates the board emulation for `cart`, or explains why it isn't supported.
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
