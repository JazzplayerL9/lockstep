// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones
//
// Portions derived from Mesen (https://github.com/nesdev-org/MesenCE):
// the MMC5 scanline detection, ExRAM and split-screen logic.
// Mesen is Copyright (C) 2014-2026 Sour and contributors, GPL-3.0-or-later.

//! Mapper 5: Nintendo MMC5 (ExROM). PRG/CHR banking with separate BG/sprite CHR
//! sets for 8x16 sprites, ExRAM (nametable / extended attributes / RAM modes),
//! fill mode, vertical split, scanline IRQ (detected from PPU fetch patterns, as
//! on hardware) and the 8x8 multiplier. Expansion audio is not emulated.

use super::{CartBase, Mapper, PrgKind, PrgSlot};

/// Nintendo MMC5. The chip has no direct view of the PPU's scanline/dot position;
/// like the real part it infers it from the PPU bus:
///
/// - **Scanline detection**: the PPU reads the same nametable address three times in
///   a row only around each line boundary while rendering (the unused NT fetches at
///   dots 337 and 339, then the dot-1 fetch of the next line, all for the same tile). On the read after such a run the
///   chip either enters the frame (first time) or increments the scanline counter
///   and compares it with $5203.
/// - **In-frame**: set on the first NT fetch after a scanline start is seen; cleared
///   when the PPU bus goes idle for 3 CPU cycles (rendering off / vblank) or when
///   the CPU reads the NMI vector ($FFFA/$FFFB).
/// - **CHR sets**: with 8x16 sprites, CHR set A ($5120-$5127) is used for sprite
///   fetches (NT fetch numbers 32-47 of the line, see `split_tile_number`) and
///   set B ($5128-$512B) for background.
/// - **ExRAM**: 1 KB internal RAM used as an extra nametable, as per-tile extended
///   attributes (mode 1), or as CPU RAM (modes 2/3).
/// - **Vertical split**: replaces BG tiles left/right of a tile column with a second
///   ExRAM-based nametable with its own vertical scroll and 4 KB CHR bank.
#[derive(Clone)]
pub struct Mmc5 {
    /// Shared PRG/CHR state. Nametables are not slot-mapped here; `ppu_read` and
    /// `ppu_write` decode them from `nt_mapping`.
    base: CartBase,
    /// 1 KB internal ExRAM, CPU-visible at $5C00-$5FFF.
    exram: Vec<u8>,
    /// Snooped copy of PPUCTRL ($2000); bit 5 (8x16 sprites) enables the separate
    /// BG/sprite CHR sets.
    ppu_ctrl: u8,
    /// $5100 PRG mode (0-3): 0 = one 32 KB bank, 1 = two 16 KB, 2 = 16 KB + two 8 KB,
    /// 3 = four 8 KB banks. Powers up as 3.
    prg_mode: u8,
    /// $5101 CHR mode (0-3): 8 KB, 4 KB, 2 KB or 1 KB banks.
    chr_mode: u8,
    /// $5102 PRG-RAM protect 1 (bits 0-1); writes allowed only when this is %10.
    prg_ram_protect1: u8,
    /// $5103 PRG-RAM protect 2 (bits 0-1); writes allowed only when this is %01.
    prg_ram_protect2: u8,
    /// $5104 ExRAM mode (0-3): 0 = nametable, 1 = extended attributes, 2 = CPU
    /// read/write RAM, 3 = CPU read-only RAM. In modes 0/1 CPU writes store 0 unless
    /// in-frame, and CPU reads are open bus.
    exram_mode: u8,
    /// $5105 nametable mapping: two bits per quadrant ($2000, $2400, $2800, $2C00 from
    /// bit 0 up): 0 = CIRAM page 0, 1 = CIRAM page 1, 2 = ExRAM, 3 = fill mode.
    nt_mapping: u8,
    /// $5106 fill-mode tile number, returned for every tile byte of a fill nametable.
    fill_tile: u8,
    /// $5107 fill-mode palette (bits 0-1), replicated into all four attribute fields.
    fill_attr: u8,
    /// $5113-$5117 PRG bank registers. `[0]` ($5113) is always an 8 KB RAM bank at
    /// $6000; `[1]`-`[3]` select ROM (bit 7 set, bits 0-6 bank) or RAM (bit 7 clear,
    /// bits 0-2 bank); `[4]` ($5117) is always ROM. Which ones are used, and how many
    /// low bits are ignored, depends on `prg_mode`.
    prg_banks: [u8; 5],
    /// $5120-$512B CHR bank registers (10-bit, upper bits from `chr_upper` at write
    /// time). `[0]`-`[7]` = set A (sprites / everything in 8x8 mode), `[8]`-`[11]` = set B
    /// (background in 8x16 mode).
    chr_banks: [u16; 12],
    /// $5130 upper CHR bank bits (bits 0-1), applied to subsequent $5120-$512B writes
    /// and to extended-attribute CHR banks.
    chr_upper: u8,
    /// Last CHR register address written ($5120-$512B); reset to 0 by `update_chr`
    /// while 8x8 sprites are selected. Outside
    /// the frame it decides which set the CPU sees through $2007: A if <= $5127.
    last_chr_reg: u16,
    /// Whether CHR set A was mapped by the last `update_chr`, so the slots are only
    /// rewritten when the active set changes.
    prev_chr_a: bool,
    /// $5200 bit 7: vertical split enabled.
    split_enabled: bool,
    /// $5200 bit 6: split region is right of the delimiter (else left).
    split_right: bool,
    /// $5200 bits 0-4: tile column (0-31) where the split region starts or ends.
    split_tile_delim: u8,
    /// $5201 vertical scroll of the split region (0-239, wraps at 240).
    split_scroll: u8,
    /// $5202 4 KB CHR bank used for split-region pattern fetches.
    split_bank: u8,
    /// Whether the current BG tile fetch falls inside the split region.
    split_in_region: bool,
    /// ExRAM nametable index (row * 32 + column) of the current split tile, reused
    /// for its attribute fetch.
    split_tile: u32,
    /// Index of the current NT fetch within the line, reset to 0 at the dot-1 fetch
    /// that completes a scanline detection: 0-31 the BG tiles fetched during the line
    /// (screen columns 2-33), 32-47 the sprite-fetch period (two garbage NT reads per
    /// sprite), 48-49 the prefetch of the next line's columns 0-1, 50-51 the unused
    /// fetches at dots 337/339. The split logic uses `(n + 2) % 50` as the column.
    split_tile_number: i32,
    /// $5205 multiplier operand A.
    mul1: u8,
    /// $5206 multiplier operand B; $5205/$5206 reads return the 16-bit product lo/hi.
    mul2: u8,
    /// $5203 IRQ compare scanline; the IRQ is flagged when `scanline_counter` equals it.
    irq_target: u8,
    /// $5204 bit 7: scanline IRQ enabled (gates the IRQ line, not the pending flag).
    irq_enabled: bool,
    /// Scanline IRQ pending flag; read (and cleared) as $5204 bit 7, also cleared on
    /// NMI-vector reads.
    irq_pending: bool,
    /// Detected scanline number within the frame (0 on the first rendered line).
    scanline_counter: u8,
    /// A scanline start was detected while out of frame; `in_frame` is set on the
    /// next NT fetch.
    need_in_frame: bool,
    /// The PPU is rendering visible lines; read back as $5204 bit 6.
    in_frame: bool,
    /// CPU cycles left before the PPU bus is considered idle; reset to 3 on each PPU
    /// read, and on reaching 0 the chip leaves the frame.
    ppu_idle_counter: u8,
    /// Previous PPU read address, for detecting repeated NT reads.
    last_ppu_addr: u16,
    /// Number of consecutive repeats of the same NT address (2 = three identical reads).
    nt_read_counter: u8,
    /// ExRAM index (NT offset) of the last tile fetched in extended-attribute mode.
    ex_last_nt_fetch: u16,
    /// Extended-attribute fetch counter: set to 3 on a BG NT fetch, decremented on
    /// each following fetch. At 2 (attribute fetch) the palette comes from ExRAM bits
    /// 6-7 and the 4 KB CHR bank from bits 0-5; at 1 and 0 (pattern low/high) the
    /// pattern bytes are read from that bank.
    ex_fetch_counter: i8,
    /// 4 KB CHR bank for the current extended-attribute tile: ExRAM bits 0-5 plus
    /// `chr_upper` as bits 6-7.
    ex_chr_bank: u32,
}

impl Mmc5 {
    /// Creates the chip in its power-on state: PRG mode 3 with $5117 = $FF (last
    /// bank at $E000), everything else 0.
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

    /// Decodes PRG bank register `reg` (index into `prg_banks`) as a full 8 KB slot:
    /// RAM or ROM per the rules on `prg_banks`, with RAM writable only when both
    /// protect registers hold their unlock values.
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

    /// Maps CPU `slot` (1-4 = $8000-$E000) from register `reg`, for 16/32 KB modes:
    /// the bank is `(reg & bank_mask) | sub`, where `sub` picks the 8 KB piece of a
    /// larger bank. ROM/RAM selection and writability come from [`Mmc5::bank_slot`].
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

    /// Remaps $6000 (always `prg_banks[0]`) and $8000-$FFFF according to `prg_mode`.
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

    /// Maps CHR set A or B into the eight 1 KB slots according to `chr_mode`. Set A
    /// is used in 8x8 sprite mode, during sprite fetches (tiles 32-47), and outside
    /// the frame when the last-written CHR register was in set A; otherwise set B.
    /// Unless `force`, does nothing when the active set has not changed.
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

    /// Reads CHR memory (ROM if present, else RAM) at absolute byte `pos`, wrapping
    /// to its size. Used for ExRAM-attribute and split fetches, which bypass the slots.
    fn read_chr_raw(&self, pos: usize) -> u8 {
        if !self.base.chr_rom.is_empty() {
            self.base.chr_rom[pos % self.base.chr_rom.len()]
        } else {
            self.base.chr_ram[pos % self.base.chr_ram.len()]
        }
    }

    /// Reads PPU $2000-$2FFF through the $5105 mapping: CIRAM page 0/1, ExRAM (0 if
    /// ExRAM is in a CPU-RAM mode), or the fill tile / fill attribute.
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

    /// Scanline detector, run on every PPU read. Counts consecutive identical NT
    /// address reads; once two repeats (three identical reads) have been seen, the
    /// next read with a different address marks a new scanline: enter the frame
    /// (via `need_in_frame`) or advance `scanline_counter` and flag the IRQ on a
    /// match with $5203. Also resets `split_tile_number` at the line start.
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

    /// Leaves the in-frame state and remaps CHR (the active set may change).
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
        // Only $2000 matters (sprite size). Leaving the frame when rendering is disabled via
        // $2001 is detected the way the real chip does it: the PPU stops reading, and the
        // idle counter in `cpu_clock` expires after 3 CPU cycles.
        if addr & 7 == 0 {
            self.ppu_ctrl = val;
        }
    }

    fn wants_cpu_clock(&self) -> bool {
        true
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
