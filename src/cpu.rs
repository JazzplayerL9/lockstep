// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones
//
// Portions derived from Mesen (https://github.com/nesdev-org/MesenCE):
// the CPU core, interrupt polling and DMA engine.
// Mesen is Copyright (C) 2014-2026 Sour and contributors, GPL-3.0-or-later.

//! Ricoh 2A03 CPU core and system bus. Every bus access is one CPU cycle and advances
//! the PPU through a shared master clock (12 master clocks per CPU cycle, 4 per PPU dot,
//! split around the access so reads/writes land on the correct PPU dot). Dummy reads and
//! writes, interrupt polling on the penultimate cycle, branch/CLI/SEI/PLP interrupt
//! latency, and OAM / DMC DMA cycle stealing are all modelled (following Mesen).

use crate::apu::{Apu, DmcDmaRequest};
use crate::cartridge::Cartridge;
use crate::input::Joypad;
use crate::mappers::{self, Mapper};
use crate::ppu::Ppu;

/// Status flag bit 0: carry.
const C: u8 = 0x01;
/// Status flag bit 1: zero (set when a result is 0).
const Z: u8 = 0x02;
/// Status flag bit 2: IRQ disable. Masks IRQs (not NMI or BRK). Because `end_cycle` samples
/// it when computing `run_irq`, a change made by CLI/SEI/PLP only takes effect on the next
/// instruction's poll, which reproduces the 6502's one-instruction latency for those opcodes.
const I: u8 = 0x04;
/// Status flag bit 3: decimal mode. Settable via SED/CLD/PLP/RTI but ignored by ADC/SBC,
/// since the 2A03 has no BCD unit.
const D: u8 = 0x08;
/// Status bit 4: "break". Not a real register bit; only set in the copy pushed by
/// BRK/PHP (clear when pushed by an IRQ/NMI) and masked off again by PLP/RTI.
const B: u8 = 0x10;
/// Status bit 5: unused. Always reads as 1 in pushed copies; masked off by PLP/RTI.
const U: u8 = 0x20;
/// Status flag bit 6: signed overflow (ADC/SBC, BIT copies operand bit 6).
const V: u8 = 0x40;
/// Status flag bit 7: negative (bit 7 of a result; BIT copies operand bit 7).
const N: u8 = 0x80;

/// Extra idle PPU scanlines inserted before VBlank when the "overclock" option is on.
pub const OVERCLOCK_SCANLINES: u16 = 100;

/// 6502 addressing mode of an opcode, selecting how [`System::fetch_operand`] fetches the
/// operand and which dummy reads it performs. Every access listed is one CPU cycle.
///
/// The `*W` ("write") variants are used by stores and read-modify-write instructions: unlike
/// the plain indexed modes, they always do the dummy read of the un-carried address, because
/// the CPU can't know whether the high byte still needs fixing before a write.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Implied: operand is implicit. One dummy read of the byte at PC (PC not incremented).
    Imp,
    /// Accumulator (ASL A etc.): same bus behaviour as [`Mode::Imp`] (one dummy read at PC).
    Acc,
    /// Immediate: the operand byte at PC is read (and PC incremented); it is the value itself.
    Imm,
    /// Relative (branches): reads the signed 8-bit offset at PC. The taken/page-cross cycles
    /// are added by [`System::branch`].
    Rel,
    /// Zero page: reads an 8-bit address from PC; the effective address is $00xx.
    Zero,
    /// Zero page,X: reads the base address, does a dummy read of the un-indexed zero-page
    /// address, then adds X with wraparound within page zero.
    ZeroX,
    /// Zero page,Y: like [`Mode::ZeroX`] but indexed by Y (LDX/STX/LAX/SAX).
    ZeroY,
    /// Indirect (JMP ($xxxx) only): `fetch_operand` reads just the 16-bit pointer; the target
    /// is read in `execute`, reproducing the page-wrap bug (high byte fetched from $xx00).
    Ind,
    /// (Indirect,X): reads the zero-page pointer, dummy-reads it, adds X (wrapping in page
    /// zero) and reads the 16-bit target address from there (high byte also wraps in page zero).
    IndX,
    /// (Indirect),Y for reads: reads the zero-page pointer and the 16-bit base, adds Y. A
    /// dummy read of the un-carried address (high byte not yet fixed) happens only when
    /// adding Y crosses a page, giving the extra cycle on a page cross.
    IndY,
    /// (Indirect),Y for writes / RMW: like [`Mode::IndY`] but the dummy read at the
    /// (possibly un-carried) address is always performed, page cross or not.
    IndYW,
    /// Absolute: reads a 16-bit address from PC (low byte first).
    Abs,
    /// Absolute,X for reads: reads the 16-bit base and adds X; the dummy read of the
    /// un-carried address (base high byte + low byte of base+X) occurs only on a page cross.
    AbsX,
    /// Absolute,X for writes / RMW: like [`Mode::AbsX`] but always performs the dummy read.
    AbsXW,
    /// Absolute,Y for reads: like [`Mode::AbsX`] indexed by Y (dummy read only on page cross).
    AbsY,
    /// Absolute,Y for writes / RMW: like [`Mode::AbsY`] but always performs the dummy read.
    AbsYW,
    /// No generic operand fetch: `fetch_operand` does nothing and `execute` performs all bus
    /// cycles itself (JSR, KIL, and the SHY/SHX/SHA/TAS stores).
    Other,
}

use Mode::*;

/// Addressing mode for each of the 256 opcodes (official and unofficial), indexed by
/// opcode byte; row = high nibble, column = low nibble. Used by `step_instruction` to choose
/// the operand fetch. Stores and RMW opcodes use the `*W` modes so their dummy read always happens.
#[rustfmt::skip]
static ADDR_MODE: [Mode; 256] = [
//  0      1      2      3      4      5      6      7      8      9      A      B      C      D      E      F
    Imp,   IndX,  Other, IndX,  Zero,  Zero,  Zero,  Zero,  Imp,   Imm,   Acc,   Imm,   Abs,   Abs,   Abs,   Abs,   // 0
    Rel,   IndY,  Other, IndYW, ZeroX, ZeroX, ZeroX, ZeroX, Imp,   AbsY,  Imp,   AbsYW, AbsX,  AbsX,  AbsXW, AbsXW, // 1
    Other, IndX,  Other, IndX,  Zero,  Zero,  Zero,  Zero,  Imp,   Imm,   Acc,   Imm,   Abs,   Abs,   Abs,   Abs,   // 2
    Rel,   IndY,  Other, IndYW, ZeroX, ZeroX, ZeroX, ZeroX, Imp,   AbsY,  Imp,   AbsYW, AbsX,  AbsX,  AbsXW, AbsXW, // 3
    Imp,   IndX,  Other, IndX,  Zero,  Zero,  Zero,  Zero,  Imp,   Imm,   Acc,   Imm,   Abs,   Abs,   Abs,   Abs,   // 4
    Rel,   IndY,  Other, IndYW, ZeroX, ZeroX, ZeroX, ZeroX, Imp,   AbsY,  Imp,   AbsYW, AbsX,  AbsX,  AbsXW, AbsXW, // 5
    Imp,   IndX,  Other, IndX,  Zero,  Zero,  Zero,  Zero,  Imp,   Imm,   Acc,   Imm,   Ind,   Abs,   Abs,   Abs,   // 6
    Rel,   IndY,  Other, IndYW, ZeroX, ZeroX, ZeroX, ZeroX, Imp,   AbsY,  Imp,   AbsYW, AbsX,  AbsX,  AbsXW, AbsXW, // 7
    Imm,   IndX,  Imm,   IndX,  Zero,  Zero,  Zero,  Zero,  Imp,   Imm,   Imp,   Imm,   Abs,   Abs,   Abs,   Abs,   // 8
    Rel,   IndYW, Other, Other, ZeroX, ZeroX, ZeroY, ZeroY, Imp,   AbsYW, Imp,   Other, Other, AbsXW, Other, Other, // 9
    Imm,   IndX,  Imm,   IndX,  Zero,  Zero,  Zero,  Zero,  Imp,   Imm,   Imp,   Imm,   Abs,   Abs,   Abs,   Abs,   // A
    Rel,   IndY,  Other, IndY,  ZeroX, ZeroX, ZeroY, ZeroY, Imp,   AbsY,  Imp,   AbsY,  AbsX,  AbsX,  AbsY,  AbsY,  // B
    Imm,   IndX,  Imm,   IndX,  Zero,  Zero,  Zero,  Zero,  Imp,   Imm,   Imp,   Imm,   Abs,   Abs,   Abs,   Abs,   // C
    Rel,   IndY,  Other, IndYW, ZeroX, ZeroX, ZeroX, ZeroX, Imp,   AbsY,  Imp,   AbsYW, AbsX,  AbsX,  AbsXW, AbsXW, // D
    Imm,   IndX,  Imm,   IndX,  Zero,  Zero,  Zero,  Zero,  Imp,   Imm,   Imp,   Imm,   Abs,   Abs,   Abs,   Abs,   // E
    Rel,   IndY,  Other, IndYW, ZeroX, ZeroX, ZeroX, ZeroX, Imp,   AbsY,  Imp,   AbsYW, AbsX,  AbsX,  AbsXW, AbsXW, // F
];

/// A complete NES: the 2A03 CPU (registers and DMA engine), 2 KB of work RAM, the PPU,
/// APU, controller ports and the cartridge mapper, all driven from one master clock.
///
/// The CPU is the scheduler: every bus access it makes is exactly one CPU cycle, during which
/// the PPU advances three dots and the APU one cycle. See `docs/ARCHITECTURE.md`.
///
/// `System` is `Clone`; a clone is a complete save state (ROM data is shared, not copied).
#[repr(C)]
#[derive(Clone)]
pub struct System {
    /// Accumulator.
    pub a: u8,
    /// X index register.
    pub x: u8,
    /// Y index register.
    pub y: u8,
    /// Stack pointer (the stack lives at $0100-$01FF).
    pub sp: u8,
    /// Program counter.
    pub pc: u16,
    /// Processor status flags (NV-BDIZC). Bits 4 and 5 only exist when pushed.
    pub status: u8,
    /// CPU cycles executed since power-on (wraps; starts at `u64::MAX` so the first cycle is 0).
    pub cycle_count: u64,
    /// Master clock counter (21.477 MHz NTSC ticks), 12 per CPU cycle. Each cycle is split
    /// in two halves to place the bus access on the right PPU dot: reads add 5 in `start_cycle`
    /// and 7 in `end_cycle`, writes add 7 then 5 (Mesen's start/end clock counts of 6 +/- 1).
    /// The PPU is run up to `master_clock - 1` after each half.
    master_clock: u64,
    /// Operand of the current instruction as resolved by `fetch_operand`: the effective
    /// address for memory modes, or the immediate/relative byte for `Imm`/`Rel` (0 for `Imp`/`Acc`/`Other`).
    operand: u16,
    /// Addressing mode of the instruction currently executing (from [`ADDR_MODE`]).
    mode: Mode,

    // Interrupt state.
    /// Level of the PPU's NMI output as sampled at the end of the previous CPU cycle; used by
    /// the edge detector in `end_cycle` (a low-to-high transition of `ppu.nmi_line` sets `need_nmi`).
    prev_nmi_line: bool,
    /// NMI edge latched: set by `end_cycle` on a rising edge of the PPU NMI line and held until
    /// an NMI is serviced (`interrupt`, or BRK hijacked into the NMI vector), or cleared by reset.
    need_nmi: bool,
    /// `need_nmi` as it was one cycle earlier. Interrupts are polled on an instruction's
    /// penultimate cycle: after the last cycle, `step_instruction` checks this delayed copy, so an
    /// NMI detected during the final cycle waits for the following instruction.
    prev_need_nmi: bool,
    /// IRQ wanted at the end of the current cycle: (mapper IRQ or APU IRQ line asserted) and
    /// the I flag clear. Recomputed by `end_cycle` every cycle (level-sensitive, not latched).
    run_irq: bool,
    /// `run_irq` from one cycle earlier, i.e. the value polled on the instruction's
    /// penultimate cycle. `step_instruction` runs `interrupt` if this (or `prev_need_nmi`) is set.
    /// `branch` manipulates it to model taken-branch IRQ delay.
    prev_run_irq: bool,

    // DMA state.
    /// A DMA (OAM via a $4014 write, or a DMC sample fetch) has been requested and the CPU
    /// must halt on its next read cycle. Checked by `read`; writes never halt, so DMA waits
    /// until the CPU's next read. Cleared when the halt cycle is consumed.
    need_halt: bool,
    /// DMC DMA still needs its dummy (alignment) cycle after the halt cycle before it may do
    /// its sample read. Set by `start_dmc_transfer`, consumed by `dma_cycle_prologue`.
    need_dummy_read: bool,
    /// OAM DMA ($4014) in progress: 256 read/write pairs copying page `sprite_dma_page` to $2004.
    sprite_dma: bool,
    /// High byte of the source address for OAM DMA (the value written to $4014).
    sprite_dma_page: u8,
    /// A DMC sample-byte DMA is pending/in progress (requested by the APU via
    /// `DmcDmaRequest::Start`). Cleared after the DMC read completes or when aborted.
    dmc_dma_running: bool,
    /// The APU cancelled a DMC DMA (`DmcDmaRequest::Stop`) after its halt cycle had already
    /// happened; the DMA engine drops the transfer on its next cycle instead of reading.
    abort_dmc_dma: bool,

    /// CPU executed a KIL/JAM opcode.
    pub jammed: bool,
    /// Last value driven on the CPU data bus (by any read or write, except $4015 reads which
    /// only use the internal bus). Returned for unmapped addresses and used for the undriven
    /// upper bits of $4016/$4017 and bit 5 of $4015.
    open_bus: u8,
    /// CPU cycles during which the APU was not clocked because the PPU had `apu_enabled`
    /// clear (the overclock's extra scanlines). If odd, `start_cycle` clocks the APU once more
    /// when it resumes to keep its odd/even (put/get) cycle phase consistent.
    apu_skipped_cycles: u64,

    /// The console's 2 KB of internal work RAM ($0000-$07FF, mirrored to $1FFF).
    pub ram: [u8; 2048],
    /// Picture processing unit.
    pub ppu: Ppu,
    /// Audio processing unit.
    pub apu: Apu,
    /// The cartridge: ROM/RAM and its bank-switching / IRQ hardware.
    pub mapper: Box<dyn Mapper>,
    /// Controller ports ($4016/$4017).
    pub joypad: Joypad,
    /// Whether idle "overclock" scanlines are inserted before VBlank (see [`System::set_overclock`]).
    pub overclock_enabled: bool,
    /// Cached `mapper.wants_cpu_clock()`: whether `start_cycle` must call `mapper.cpu_clock()`
    /// every CPU cycle (cached to avoid a virtual call per cycle for boards that don't need it).
    mapper_clock: bool,
    /// Cached `mapper.has_expansion_audio()`: whether `start_cycle` must fetch the mapper's
    /// expansion audio sample into `apu.expansion` every cycle.
    mapper_audio: bool,
    /// Game title (from the ROM file name).
    pub title: String,
    /// Copy of the cartridge header/metadata with PRG/CHR ROM stripped, kept only so
    /// `save_sram` knows whether there is a battery and where the `.sav` file goes.
    cart_for_sram: Cartridge,
}

impl System {
    /// Powers on a console with `cart` inserted. Fails if the cartridge's board (mapper) isn't emulated.
    pub fn new(cart: Cartridge) -> Result<Self, String> {
        let mapper = mappers::create_mapper(&cart)?;
        let mut cart_for_sram = cart;
        // Keep only what save_sram needs; ROM data lives in the mapper.
        cart_for_sram.prg_rom = Vec::new();
        cart_for_sram.chr_rom = Vec::new();
        let title = cart_for_sram.title.clone();
        let mut sys = Self {
            a: 0,
            x: 0,
            y: 0,
            sp: 0xFD,
            pc: 0,
            status: I,
            cycle_count: u64::MAX,
            master_clock: 0,
            operand: 0,
            mode: Imp,
            prev_nmi_line: false,
            need_nmi: false,
            prev_need_nmi: false,
            run_irq: false,
            prev_run_irq: false,
            need_halt: false,
            need_dummy_read: false,
            sprite_dma: false,
            sprite_dma_page: 0,
            dmc_dma_running: false,
            abort_dmc_dma: false,
            jammed: false,
            open_bus: 0,
            apu_skipped_cycles: 0,
            ram: [0; 2048],
            ppu: Ppu::new(),
            apu: Apu::new(),
            mapper,
            joypad: Joypad::new(),
            overclock_enabled: false,
            mapper_clock: false,
            mapper_audio: false,
            title,
            cart_for_sram,
        };
        sys.mapper_clock = sys.mapper.wants_cpu_clock();
        sys.mapper_audio = sys.mapper.has_expansion_audio();
        sys.ppu.vram_hook = sys.mapper.wants_vram_addr();
        let lower_title = sys.title.to_ascii_lowercase();
        if [
            "duck hunt",
            "wild gunman",
            "hogan",
            "gumshoe",
            "barker bill",
            "freedom force",
            "gotcha",
            "mechanized attack",
            "operation wolf",
            "shooting range",
            "to the earth",
            "baby boomer",
            "chiller",
            "lone ranger",
        ]
        .iter()
        .any(|kw| lower_title.contains(kw))
        {
            sys.joypad.zapper_connected = true;
        }
        sys.power_on();
        Ok(sys)
    }

    /// Back-compat constructor used by older call sites; panics on unsupported mappers.
    pub fn from_cartridge(cart: Cartridge) -> Self {
        Self::new(cart).expect("unsupported cartridge")
    }

    /// Power-on: loads PC from the reset vector ($FFFC) with a side-effect-free peek, resets
    /// the cycle counters (master clock starts at 12) and runs the 8 cycles of the reset sequence.
    fn power_on(&mut self) {
        self.pc = self.peek_u16(0xFFFC);
        self.cycle_count = u64::MAX;
        self.master_clock = 12;
        // The CPU spends 8 cycles in its reset sequence before fetching the first opcode.
        for _ in 0..8 {
            self.start_cycle(true);
            self.end_cycle(true);
        }
    }

    /// Soft reset (console reset button).
    pub fn reset(&mut self) {
        self.status |= I;
        self.sp = self.sp.wrapping_sub(3);
        self.need_nmi = false;
        self.prev_need_nmi = false;
        self.run_irq = false;
        self.prev_run_irq = false;
        self.need_halt = false;
        self.need_dummy_read = false;
        self.sprite_dma = false;
        self.dmc_dma_running = false;
        self.abort_dmc_dma = false;
        self.jammed = false;
        self.ppu.reset();
        self.apu.reset();
        self.pc = self.peek_u16(0xFFFC);
        for _ in 0..8 {
            self.start_cycle(true);
            self.end_cycle(true);
        }
    }

    /// Writes battery-backed cartridge RAM to the `.sav` file next to the ROM (no-op without a battery).
    pub fn save_sram(&self) {
        self.cart_for_sram.save_battery_sram(&self.mapper.battery_ram());
    }

    /// Serializes the entire console state (CPU, 2 KB RAM, PPU, APU, mapper, and controller ports)
    /// into a gzip-compressed byte buffer suitable for writing to a `.s1`..`.s9` save-state file.
    pub fn to_state_bytes(&self) -> Vec<u8> {
        use std::io::Write;
        let mut raw = Vec::with_capacity(160 * 1024);
        raw.extend_from_slice(b"LKST\x01\x00\x00\x00");
        let pod_len = std::mem::offset_of!(Self, ppu);
        raw.extend_from_slice(&(pod_len as u32).to_le_bytes());
        // SAFETY: All fields of `System` before `ppu` are plain-old-data (`Copy`).
        let cpu_pod = unsafe { std::slice::from_raw_parts(self as *const Self as *const u8, pod_len) };
        raw.extend_from_slice(cpu_pod);
        self.ppu.save_state(&mut raw);
        self.apu.save_state(&mut raw);
        self.mapper.save_mapper_state(&mut raw);
        self.joypad.save_state(&mut raw);
        raw.push(self.overclock_enabled as u8);

        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let _ = enc.write_all(&raw);
        enc.finish().unwrap_or(raw)
    }

    /// Restores the entire console state from a buffer produced by [`System::to_state_bytes`].
    pub fn load_state_bytes(&mut self, data: &[u8]) -> Result<(), String> {
        use std::io::Read;
        let mut raw = Vec::new();
        if data.starts_with(&[0x1F, 0x8B]) {
            let mut dec = flate2::read::GzDecoder::new(data);
            dec.read_to_end(&mut raw)
                .map_err(|e| format!("Failed to decompress save state: {}", e))?;
        } else {
            raw.extend_from_slice(data);
        }
        let mut cur: &[u8] = &raw;
        if cur.len() < 12 || &cur[0..8] != b"LKST\x01\x00\x00\x00" {
            return Err("Invalid save state header".into());
        }
        let pod_len = std::mem::offset_of!(Self, ppu);
        let saved_pod = u32::from_le_bytes(cur[8..12].try_into().unwrap()) as usize;
        cur = &cur[12..];
        if saved_pod != pod_len || cur.len() < pod_len {
            return Err("CPU state size mismatch".into());
        }
        let (cpu_pod, rest) = cur.split_at(pod_len);
        cur = rest;
        // Clone self first so a partial failure leaves `self` untouched.
        let mut tmp = self.clone();
        // SAFETY: All fields of `System` before `ppu` are plain-old-data (`Copy`).
        unsafe {
            std::ptr::copy_nonoverlapping(cpu_pod.as_ptr(), &mut tmp as *mut Self as *mut u8, pod_len);
        }
        tmp.ppu.load_state(&mut cur)?;
        tmp.apu.load_state(&mut cur)?;
        tmp.mapper.load_mapper_state(&mut cur)?;
        tmp.joypad.load_state(&mut cur)?;
        if cur.is_empty() {
            return Err("Truncated save state (tail)".into());
        }
        tmp.overclock_enabled = cur[0] != 0;
        tmp.mapper_clock = tmp.mapper.wants_cpu_clock();
        tmp.mapper_audio = tmp.mapper.has_expansion_audio();
        tmp.ppu.vram_hook = tmp.mapper.wants_vram_addr();
        *self = tmp;
        Ok(())
    }

    /// Writes a compressed save state to `<rom>.s<slot>` (slots 1–9) on disk.
    pub fn save_state_file(&self, slot: u8) -> Result<std::path::PathBuf, String> {
        let path = self
            .cart_for_sram
            .state_path(slot)
            .ok_or_else(|| "Cartridge has no ROM file path for disk save state".to_string())?;
        let bytes = self.to_state_bytes();
        std::fs::write(&path, bytes).map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;
        Ok(path)
    }

    /// Loads a compressed save state from `<rom>.s<slot>` (slots 1–9) on disk.
    pub fn load_state_file(&mut self, slot: u8) -> Result<std::path::PathBuf, String> {
        let path = self
            .cart_for_sram
            .state_path(slot)
            .ok_or_else(|| "Cartridge has no ROM file path for disk save state".to_string())?;
        let bytes =
            std::fs::read(&path).map_err(|e| format!("Slot {} empty ({})", slot.clamp(1, 9), e))?;
        self.load_state_bytes(&bytes)?;
        Ok(path)
    }

    /// Returns the disk path for save-state `slot` (1–9), if the cartridge was loaded from a file.
    pub fn state_path(&self, slot: u8) -> Option<std::path::PathBuf> {
        self.cart_for_sram.state_path(slot)
    }

    /// Human-readable board name, e.g. `"MMC3 (4)"`.
    pub fn mapper_name(&self) -> &'static str {
        self.mapper.name()
    }

    /// Enables/disables the overclock: [`OVERCLOCK_SCANLINES`] extra idle scanlines before VBlank,
    /// giving games more CPU time per frame (reduces slowdown) without changing the video or
    /// audio rate. Takes effect at the next frame boundary.
    #[inline]
    pub fn set_overclock(&mut self, on: bool) {
        self.overclock_enabled = on;
        self.ppu.extra_scanlines = if on { OVERCLOCK_SCANLINES } else { 0 };
    }

    // ---------------------------------------------------------------- frame loop

    /// Emulates until the PPU has finished drawing a frame (scanline 240).
    /// The finished image is in `ppu.framebuffer`, and audio in `apu.audio_samples`.
    pub fn run_frame(&mut self) {
        self.ppu.extra_scanlines = if self.overclock_enabled { OVERCLOCK_SCANLINES } else { 0 };
        self.ppu.frame_complete = false;
        let start = self.cycle_count;
        while !self.ppu.frame_complete {
            self.step_instruction();
            // Safety valve: a frame is ~30k cycles; never spin forever if the PPU stalls.
            if self.cycle_count.wrapping_sub(start) > 400_000 {
                break;
            }
        }
        self.joypad.step_zapper_frame();
    }

    /// Runs one real frame, then (if `ahead` > 0) speculatively runs `ahead` more frames
    /// with the same input, copies that future frame into `out`, and rolls back. The
    /// real frame's audio is left in `apu.audio_samples`. This hides the game's own
    /// 1-2 frames of input lag ("run-ahead").
    pub fn run_frame_ahead(&mut self, ahead: u8, out: &mut [u8]) {
        self.run_frame();
        if ahead == 0 {
            out.copy_from_slice(&self.ppu.framebuffer);
            return;
        }
        let snapshot = self.clone();
        for _ in 0..ahead {
            self.run_frame();
        }
        out.copy_from_slice(&self.ppu.framebuffer);
        *self = snapshot;
    }

    // ---------------------------------------------------------------- clocking

    /// First half of a CPU cycle: advances the master clock (5 for a read, 7 for a write), runs
    /// the PPU up to it, increments `cycle_count`, clocks the mapper (if it wants CPU clocks) and
    /// the APU, and handles any DMC DMA start/stop the APU requested this cycle. While the PPU is
    /// in overclock lines (`apu_enabled` false) the APU is not clocked and the skipped cycles counted.
    #[inline]
    fn start_cycle(&mut self, read: bool) {
        self.master_clock += if read { 5 } else { 7 };
        self.ppu.run(self.master_clock - 1, &mut *self.mapper);
        self.cycle_count = self.cycle_count.wrapping_add(1);
        self.ppu.cpu_cycle = self.cycle_count;
        if self.mapper_clock {
            self.mapper.cpu_clock();
        }
        if self.ppu.apu_enabled {
            if self.apu_skipped_cycles & 1 == 1 {
                // Re-sync APU odd/even phase after overclock lines.
                self.apu.clock();
            }
            self.apu_skipped_cycles = 0;
            if self.mapper_audio {
                self.apu.expansion = self.mapper.audio_sample();
            }
            self.apu.clock();
            match std::mem::take(&mut self.apu.dma_request) {
                DmcDmaRequest::Start => self.start_dmc_transfer(),
                DmcDmaRequest::Stop => self.stop_dmc_transfer(),
                DmcDmaRequest::None => {}
            }
        } else {
            self.apu_skipped_cycles += 1;
        }
    }

    /// Second half of a CPU cycle: advances the master clock (7 for a read, 5 for a write),
    /// runs the PPU, then polls the interrupt lines. Shifts `need_nmi`/`run_irq` into their `prev_*`
    /// copies, edge-detects the PPU NMI line, and recomputes `run_irq` from the mapper/APU IRQ
    /// lines and the I flag. The `prev_*` values are what `step_instruction` acts on, giving
    /// polling on the penultimate cycle of each instruction.
    #[inline]
    fn end_cycle(&mut self, read: bool) {
        self.master_clock += if read { 7 } else { 5 };
        self.ppu.run(self.master_clock - 1, &mut *self.mapper);

        self.prev_need_nmi = self.need_nmi;
        let nmi = self.ppu.nmi_line;
        if !self.prev_nmi_line && nmi {
            self.need_nmi = true;
        }
        self.prev_nmi_line = nmi;

        self.prev_run_irq = self.run_irq;
        self.run_irq = (self.mapper.irq() || self.apu.irq_line()) && self.status & I == 0;
    }

    /// Requests a DMC sample DMA: the CPU will halt on its next read, spend a dummy cycle, then
    /// fetch the byte (see `process_pending_dma`).
    fn start_dmc_transfer(&mut self) {
        self.dmc_dma_running = true;
        self.need_dummy_read = true;
        self.need_halt = true;
    }

    /// Cancels a pending DMC DMA (the APU disabled the channel). If the halt hasn't happened
    /// yet the request is simply dropped; otherwise `abort_dmc_dma` is set so the in-progress DMA
    /// engine abandons it.
    fn stop_dmc_transfer(&mut self) {
        if self.dmc_dma_running {
            if self.need_halt {
                self.dmc_dma_running = false;
                self.need_dummy_read = false;
                self.need_halt = false;
            } else {
                self.abort_dmc_dma = true;
            }
        }
    }

    // ---------------------------------------------------------------- bus

    /// Side-effect-free read used for vectors at power-on.
    fn peek_u16(&mut self, addr: u16) -> u16 {
        let lo = self.mapper.cpu_read(addr).unwrap_or(0) as u16;
        let hi = self.mapper.cpu_read(addr + 1).unwrap_or(0) as u16;
        (hi << 8) | lo
    }

    /// Performs a CPU-bus read with side effects (PPU/APU/joypad registers, mapper), without
    /// advancing time. Updates `open_bus` with the result, except for $4015 which doesn't drive
    /// the external bus. $4000-$401F registers that aren't readable return open bus; unmapped
    /// cartridge space returns open bus.
    fn bus_read(&mut self, addr: u16) -> u8 {
        let v = match addr {
            0x0000..=0x1FFF => self.ram[(addr & 0x7FF) as usize],
            0x2000..=0x3FFF => self.ppu.read_register(addr, &mut *self.mapper),
            0x4015 => {
                // $4015 reads don't drive the external data bus.
                let v = self.apu.read_status() | (self.open_bus & 0x20);
                return v;
            }
            0x4016 => (self.open_bus & 0xE0) | self.joypad.read_p1(),
            0x4017 => {
                let mut b = (self.open_bus & 0xE0) | self.joypad.read_p2();
                if self.joypad.zapper_connected {
                    if !self.ppu.zapper_sees_light(self.joypad.zapper_x, self.joypad.zapper_y) {
                        b |= 0x08;
                    }
                    if self.joypad.zapper_trigger_frames > 0 {
                        b |= 0x10;
                    }
                }
                b
            }
            0x4000..=0x401F => self.open_bus,
            _ => self.mapper.cpu_read(addr).unwrap_or(self.open_bus),
        };
        self.open_bus = v;
        v
    }

    /// Performs a CPU-bus write without advancing time and sets `open_bus` to `val`. $2000-$3FFF
    /// also notifies the mapper (for boards that snoop PPU register writes); $4014 schedules OAM
    /// DMA (which starts at the next read); $4018-$401F are ignored; $4020+ goes to the mapper.
    fn bus_write(&mut self, addr: u16, val: u8) {
        self.open_bus = val;
        match addr {
            0x0000..=0x1FFF => self.ram[(addr & 0x7FF) as usize] = val,
            0x2000..=0x3FFF => {
                self.ppu.write_register(addr, val, &mut *self.mapper);
                self.mapper.ppu_register_write(addr, val);
            }
            0x4014 => {
                self.sprite_dma = true;
                self.sprite_dma_page = val;
                self.need_halt = true;
            }
            0x4016 => self.joypad.write_strobe(val),
            0x4000..=0x4013 | 0x4015 | 0x4017 => self.apu.write_register(addr, val, self.cycle_count),
            0x4018..=0x401F => {}
            _ => self.mapper.cpu_write(addr, val, self.cycle_count),
        }
    }

    /// One full CPU read cycle at `addr`. If a DMA is pending, the CPU is halted here first
    /// and `process_pending_dma` runs the stolen cycles before this read happens.
    #[inline]
    fn read(&mut self, addr: u16) -> u8 {
        if self.need_halt {
            self.process_pending_dma(addr);
        }
        self.start_cycle(true);
        let v = self.bus_read(addr);
        self.end_cycle(true);
        v
    }

    /// One full CPU write cycle. DMA never halts the CPU on a write cycle.
    #[inline]
    fn write(&mut self, addr: u16, val: u8) {
        self.start_cycle(false);
        self.bus_write(addr, val);
        self.end_cycle(false);
    }

    /// Read performed by the DMA unit (OAM source byte or DMC sample). Reads of $4015-$401A
    /// return open bus because DMA can't see the 2A03's internal registers. (Mesen's extra glitch
    /// where a halted read in $4000-$401F lets the CPU read internal registers is not modelled.)
    fn dma_read(&mut self, addr: u16) -> u8 {
        // DMA can't see the 2A03's internal registers.
        if (0x4015..=0x401A).contains(&addr) { self.open_bus } else { self.bus_read(addr) }
    }

    /// Runs the DMA engine, ported from Mesen's `ProcessPendingDma`. Called by `read` when
    /// `need_halt` is set, with the address the CPU was about to read.
    ///
    /// The first cycle is the halt cycle, repeating the CPU's read of `read_addr`. Then cycles
    /// alternate get (even `cycle_count`) / put (odd): on get cycles a ready DMC DMA (halt and
    /// dummy consumed) reads its byte and has priority, otherwise OAM DMA reads its next byte,
    /// otherwise a dummy re-read of `read_addr` aligns the transfer. On put cycles OAM DMA writes
    /// the byte it just read to $2004, or a dummy read is done. OAM DMA thus takes 513-514 cycles
    /// (plus any DMC steals). Returns when neither DMA is active; the CPU's read then proceeds.
    fn process_pending_dma(&mut self, read_addr: u16) {
        self.need_halt = false;
        // Halt cycle (repeats the CPU's read).
        self.start_cycle(true);
        self.bus_read(read_addr);
        self.end_cycle(true);

        if self.abort_dmc_dma {
            self.dmc_dma_running = false;
            self.abort_dmc_dma = false;
            if !self.sprite_dma {
                self.need_dummy_read = false;
                return;
            }
        }

        let mut sprite_counter: u16 = 0;
        let mut sprite_addr: u8 = 0;
        let mut value: u8 = 0;

        while self.dmc_dma_running || self.sprite_dma {
            let get_cycle = self.cycle_count & 1 == 0;
            // `process_cycle` bookkeeping from Mesen: halt/dummy states are consumed here.
            if get_cycle {
                if self.dmc_dma_running && !self.need_halt && !self.need_dummy_read {
                    self.dma_cycle_prologue();
                    self.start_cycle(true);
                    let a = self.apu.dmc_read_address();
                    let v = self.dma_read(a);
                    self.end_cycle(true);
                    self.dmc_dma_running = false;
                    self.abort_dmc_dma = false;
                    self.apu.set_dmc_read_buffer(v);
                } else if self.sprite_dma {
                    self.dma_cycle_prologue();
                    self.start_cycle(true);
                    let a = (self.sprite_dma_page as u16) << 8 | sprite_addr as u16;
                    value = self.dma_read(a);
                    self.end_cycle(true);
                    sprite_addr = sprite_addr.wrapping_add(1);
                    sprite_counter += 1;
                } else {
                    self.dma_cycle_prologue();
                    self.start_cycle(true);
                    self.bus_read(read_addr);
                    self.end_cycle(true);
                }
            } else if self.sprite_dma && sprite_counter & 1 == 1 {
                self.dma_cycle_prologue();
                self.start_cycle(true);
                self.ppu.write_register(0x2004, value, &mut *self.mapper);
                self.end_cycle(true);
                sprite_counter += 1;
                if sprite_counter == 0x200 {
                    self.sprite_dma = false;
                }
            } else {
                self.dma_cycle_prologue();
                self.start_cycle(true);
                self.bus_read(read_addr);
                self.end_cycle(true);
            }
        }
    }

    /// Bookkeeping at the start of each DMA cycle (Mesen's `processCycle`): consumes a pending
    /// DMC abort, or else one outstanding DMC halt or dummy cycle. OAM DMA cycles therefore also
    /// count as the DMC's halt/dummy cycles when the two overlap.
    #[inline]
    fn dma_cycle_prologue(&mut self) {
        if self.abort_dmc_dma {
            self.dmc_dma_running = false;
            self.abort_dmc_dma = false;
            self.need_dummy_read = false;
            self.need_halt = false;
        } else if self.need_halt {
            self.need_halt = false;
        } else if self.need_dummy_read {
            self.need_dummy_read = false;
        }
    }

    // ---------------------------------------------------------------- helpers

    /// Dummy read of the byte at PC without incrementing PC (one cycle).
    #[inline]
    fn dummy_pc_read(&mut self) {
        self.read(self.pc);
    }

    /// Reads the byte at PC and increments PC (one cycle).
    #[inline]
    fn read_byte(&mut self) -> u8 {
        let v = self.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }

    /// Reads a little-endian 16-bit word at PC, advancing PC by 2 (two cycles).
    #[inline]
    fn read_word(&mut self) -> u16 {
        let lo = self.read_byte() as u16;
        let hi = self.read_byte() as u16;
        (hi << 8) | lo
    }

    /// Reads a little-endian 16-bit word at `addr`/`addr+1` (two cycles). Used for vectors.
    #[inline]
    fn read_u16(&mut self, addr: u16) -> u16 {
        let lo = self.read(addr) as u16;
        let hi = self.read(addr.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }

    /// Pushes a byte to the stack at $0100+SP and decrements SP (one write cycle).
    #[inline]
    fn push(&mut self, v: u8) {
        self.write(0x100 | self.sp as u16, v);
        self.sp = self.sp.wrapping_sub(1);
    }

    /// Pushes a 16-bit value, high byte first (two write cycles).
    #[inline]
    fn push16(&mut self, v: u16) {
        self.push((v >> 8) as u8);
        self.push(v as u8);
    }

    /// Increments SP and reads the byte at $0100+SP (one cycle).
    #[inline]
    fn pop(&mut self) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        self.read(0x100 | self.sp as u16)
    }

    /// Pops a 16-bit value, low byte first (two cycles).
    #[inline]
    fn pop16(&mut self) -> u16 {
        let lo = self.pop() as u16;
        let hi = self.pop() as u16;
        (hi << 8) | lo
    }

    /// Dummy read of $0100+SP (the cycle in which the 6502 increments SP for PLA/PLP/RTS/RTI,
    /// or JSR's internal cycle). One cycle.
    #[inline]
    fn dummy_stack_read(&mut self) {
        self.read(0x100 | self.sp as u16);
    }

    /// Sets Z and N from `v`.
    #[inline]
    fn set_zn(&mut self, v: u8) {
        self.status &= !(Z | N);
        if v == 0 {
            self.status |= Z;
        }
        self.status |= v & N;
    }

    /// Whether status flag `f` (one of the flag bit constants) is set.
    #[inline]
    fn flag(&self, f: u8) -> bool {
        self.status & f != 0
    }

    /// Sets or clears status flag `f`.
    #[inline]
    fn set_flag(&mut self, f: u8, on: bool) {
        if on {
            self.status |= f;
        } else {
            self.status &= !f;
        }
    }

    /// Whether adding the unsigned index `b` to address `a` changes the high byte.
    #[inline]
    fn page_crossed(a: u16, b: u8) -> bool {
        (a.wrapping_add(b as u16) & 0xFF00) != (a & 0xFF00)
    }

    /// Performs the addressing-mode cycles for the current `mode` (after the opcode fetch) and
    /// returns the operand: the effective address for memory modes, the byte itself for
    /// `Imm`/`Rel`, or 0 for `Imp`/`Acc` (after their dummy PC read) and `Other` (no cycles).
    /// Includes the dummy reads described on [`Mode`]; on page crosses the dummy read targets the
    /// address with the un-carried high byte, as on real hardware.
    fn fetch_operand(&mut self) -> u16 {
        match self.mode {
            Acc | Imp => {
                self.dummy_pc_read();
                0
            }
            Imm | Rel => self.read_byte() as u16,
            Zero => self.read_byte() as u16,
            ZeroX => {
                let z = self.read_byte();
                self.read(z as u16);
                z.wrapping_add(self.x) as u16
            }
            ZeroY => {
                let z = self.read_byte();
                self.read(z as u16);
                z.wrapping_add(self.y) as u16
            }
            Ind => self.read_word(),
            IndX => {
                let z = self.read_byte();
                self.read(z as u16);
                let z = z.wrapping_add(self.x);
                let lo = self.read(z as u16) as u16;
                let hi = self.read(z.wrapping_add(1) as u16) as u16;
                (hi << 8) | lo
            }
            IndY | IndYW => {
                let z = self.read_byte();
                let lo = self.read(z as u16) as u16;
                let hi = self.read(z.wrapping_add(1) as u16) as u16;
                let base = (hi << 8) | lo;
                let crossed = Self::page_crossed(base, self.y);
                if crossed || self.mode == IndYW {
                    let dummy = base.wrapping_add(self.y as u16).wrapping_sub(if crossed { 0x100 } else { 0 });
                    self.read(dummy);
                }
                base.wrapping_add(self.y as u16)
            }
            Abs => self.read_word(),
            AbsX | AbsXW => {
                let base = self.read_word();
                let crossed = Self::page_crossed(base, self.x);
                if crossed || self.mode == AbsXW {
                    let dummy = base.wrapping_add(self.x as u16).wrapping_sub(if crossed { 0x100 } else { 0 });
                    self.read(dummy);
                }
                base.wrapping_add(self.x as u16)
            }
            AbsY | AbsYW => {
                let base = self.read_word();
                let crossed = Self::page_crossed(base, self.y);
                if crossed || self.mode == AbsYW {
                    let dummy = base.wrapping_add(self.y as u16).wrapping_sub(if crossed { 0x100 } else { 0 });
                    self.read(dummy);
                }
                base.wrapping_add(self.y as u16)
            }
            Other => 0,
        }
    }

    /// Value for a read instruction: the immediate byte for `Imm`/`Rel`/`Imp`/`Acc`/`Other`
    /// (no bus access), otherwise one read cycle at the effective address.
    #[inline]
    fn operand_value(&mut self) -> u8 {
        match self.mode {
            Imm | Rel | Acc | Imp | Other => self.operand as u8,
            _ => self.read(self.operand),
        }
    }

    /// Binary ADC: A = A + v + C, setting C, V, Z and N. SBC is `add(v ^ 0xFF)`. No decimal mode.
    fn add(&mut self, v: u8) {
        let c = (self.status & C) as u16;
        let r = self.a as u16 + v as u16 + c;
        self.set_flag(V, (!(self.a ^ v) & (self.a ^ r as u8) & 0x80) != 0);
        self.set_flag(C, r > 0xFF);
        self.a = r as u8;
        self.set_zn(self.a);
    }

    /// Compare (CMP/CPX/CPY/DCP): C = `reg >= v`, Z/N from `reg - v`.
    fn cmp(&mut self, reg: u8, v: u8) {
        self.set_flag(C, reg >= v);
        self.set_zn(reg.wrapping_sub(v));
    }

    /// Arithmetic shift left; C = old bit 7. Returns the result and sets Z/N.
    fn asl(&mut self, v: u8) -> u8 {
        self.set_flag(C, v & 0x80 != 0);
        let r = v << 1;
        self.set_zn(r);
        r
    }

    /// Logical shift right; C = old bit 0. Returns the result and sets Z/N.
    fn lsr(&mut self, v: u8) -> u8 {
        self.set_flag(C, v & 1 != 0);
        let r = v >> 1;
        self.set_zn(r);
        r
    }

    /// Rotate left through carry; C = old bit 7. Returns the result and sets Z/N.
    fn rol(&mut self, v: u8) -> u8 {
        let c = self.status & C;
        self.set_flag(C, v & 0x80 != 0);
        let r = (v << 1) | c;
        self.set_zn(r);
        r
    }

    /// Rotate right through carry; C = old bit 0. Returns the result and sets Z/N.
    fn ror(&mut self, v: u8) -> u8 {
        let c = (self.status & C) << 7;
        self.set_flag(C, v & 1 != 0);
        let r = (v >> 1) | c;
        self.set_zn(r);
        r
    }

    /// Read-modify-write on memory with the 6502's dummy write of the original value.
    #[inline]
    fn rmw(&mut self, f: fn(&mut Self, u8) -> u8) -> u8 {
        let addr = self.operand;
        let v = self.read(addr);
        self.write(addr, v);
        let r = f(self, v);
        self.write(addr, r);
        r
    }

    /// Conditional branch using the relative offset in `operand`. Not taken: no extra cycles.
    /// Taken: one dummy read at PC, plus a dummy read at the un-carried target if a page is crossed.
    ///
    /// Interrupt timing (as in Mesen): branches poll on their 2nd and 4th cycles. Restoring
    /// `run_irq` to `prev_run_irq` before the extra cycle discards an IRQ that first appears in
    /// the taken branch's last cycle, so one more instruction runs before the IRQ. On a page cross
    /// the two polls are ORed so an IRQ seen at either poll is taken.
    fn branch(&mut self, cond: bool) {
        let offset = self.operand as u8 as i8;
        if cond {
            // Taken branches poll interrupts on their 2nd cycle (restore that sample here)
            // and, if a page is crossed, again on their 4th; a non-crossing taken branch
            // therefore delays a newly arrived interrupt by one instruction.
            self.run_irq = self.prev_run_irq;
            self.dummy_pc_read();
            let target = self.pc.wrapping_add(offset as i16 as u16);
            if (target & 0xFF00) != (self.pc & 0xFF00) {
                self.run_irq |= self.prev_run_irq;
                self.read((self.pc & 0xFF00) | (target & 0x00FF));
            }
            self.pc = target;
        }
    }

    /// Store for the unstable SHY/SHX/SHA/TAS opcodes (Mesen's `SyaSxaAxa`): dummy read at the
    /// un-carried address, then write `value & (base_high + 1)`. On a page cross the written
    /// address's high byte is also ANDed with `value`. If DMA interrupted the dummy read (it took
    /// more than one cycle) the AND with the high byte is skipped and `value` is written unmodified.
    fn sh_store(&mut self, base: u16, index: u8, value: u8) {
        let crossed = Self::page_crossed(base, index);
        let before = self.cycle_count;
        let dummy = base.wrapping_add(index as u16).wrapping_sub(if crossed { 0x100 } else { 0 });
        self.read(dummy);
        let had_dma = self.cycle_count.wrapping_sub(before) > 1;
        let operand = base.wrapping_add(index as u16);
        let mut hi = (operand >> 8) as u8;
        let lo = operand as u8;
        if crossed {
            hi &= value;
        }
        let v = if had_dma { value } else { value & ((base >> 8) as u8).wrapping_add(1) };
        self.write(((hi as u16) << 8) | lo as u16, v);
    }

    // ---------------------------------------------------------------- interrupts

    /// Hardware interrupt sequence (7 cycles): two dummy reads at PC, push PC and status (with
    /// B clear, U set), set I, load PC from the vector. The vector is chosen when the status is
    /// pushed: if an NMI is pending by then it uses $FFFA (NMI hijacks an IRQ) and clears
    /// `need_nmi`, otherwise $FFFE.
    fn interrupt(&mut self) {
        self.dummy_pc_read();
        self.dummy_pc_read();
        self.push16(self.pc);
        if self.need_nmi {
            self.need_nmi = false;
            self.push(self.status | U);
            self.status |= I;
            self.pc = self.read_u16(0xFFFA);
        } else {
            self.push(self.status | U);
            self.status |= I;
            self.pc = self.read_u16(0xFFFE);
        }
    }

    // ---------------------------------------------------------------- execute

    /// Executes one CPU instruction (and any interrupt that becomes pending), advancing the
    /// PPU/APU in step with every bus cycle.
    pub fn step_instruction(&mut self) {
        let op = self.read_byte();
        self.mode = ADDR_MODE[op as usize];
        self.operand = self.fetch_operand();
        self.execute(op);
        if self.prev_run_irq || self.prev_need_nmi {
            self.interrupt();
        }
    }

    /// Executes opcode `op` after `fetch_operand` has set `operand`. Covers all 256 opcodes
    /// (official, unofficial NOP/LAX/SAX/SLO/RLA/SRE/RRA/DCP/ISC/ANC/ALR/ARR/LXA/ANE/AXS/LAS, the
    /// SHx/TAS stores, and KIL, which rewinds PC and sets `jammed`). BRK checks `need_nmi` like
    /// `interrupt` (NMI can hijack it) and clears `prev_need_nmi` so the handler's first
    /// instruction runs before another NMI is taken.
    fn execute(&mut self, op: u8) {
        match op {
            // Loads
            0xA9 | 0xA5 | 0xB5 | 0xAD | 0xBD | 0xB9 | 0xA1 | 0xB1 => {
                let v = self.operand_value();
                self.a = v;
                self.set_zn(v);
            }
            0xA2 | 0xA6 | 0xB6 | 0xAE | 0xBE => {
                let v = self.operand_value();
                self.x = v;
                self.set_zn(v);
            }
            0xA0 | 0xA4 | 0xB4 | 0xAC | 0xBC => {
                let v = self.operand_value();
                self.y = v;
                self.set_zn(v);
            }
            // Stores
            0x85 | 0x95 | 0x8D | 0x9D | 0x99 | 0x81 | 0x91 => self.write(self.operand, self.a),
            0x86 | 0x96 | 0x8E => self.write(self.operand, self.x),
            0x84 | 0x94 | 0x8C => self.write(self.operand, self.y),
            // Transfers
            0xAA => {
                self.x = self.a;
                self.set_zn(self.x);
            }
            0xA8 => {
                self.y = self.a;
                self.set_zn(self.y);
            }
            0xBA => {
                self.x = self.sp;
                self.set_zn(self.x);
            }
            0x8A => {
                self.a = self.x;
                self.set_zn(self.a);
            }
            0x9A => self.sp = self.x,
            0x98 => {
                self.a = self.y;
                self.set_zn(self.a);
            }
            // Stack
            0x48 => self.push(self.a),
            0x08 => self.push(self.status | B | U),
            0x68 => {
                self.dummy_stack_read();
                let v = self.pop();
                self.a = v;
                self.set_zn(v);
            }
            0x28 => {
                self.dummy_stack_read();
                let v = self.pop();
                self.status = v & 0xCF;
            }
            // Logic / arithmetic
            0x29 | 0x25 | 0x35 | 0x2D | 0x3D | 0x39 | 0x21 | 0x31 => {
                let v = self.operand_value();
                self.a &= v;
                self.set_zn(self.a);
            }
            0x09 | 0x05 | 0x15 | 0x0D | 0x1D | 0x19 | 0x01 | 0x11 => {
                let v = self.operand_value();
                self.a |= v;
                self.set_zn(self.a);
            }
            0x49 | 0x45 | 0x55 | 0x4D | 0x5D | 0x59 | 0x41 | 0x51 => {
                let v = self.operand_value();
                self.a ^= v;
                self.set_zn(self.a);
            }
            0x24 | 0x2C => {
                let v = self.operand_value();
                self.set_flag(Z, self.a & v == 0);
                self.set_flag(V, v & 0x40 != 0);
                self.set_flag(N, v & 0x80 != 0);
            }
            0x69 | 0x65 | 0x75 | 0x6D | 0x7D | 0x79 | 0x61 | 0x71 => {
                let v = self.operand_value();
                self.add(v);
            }
            0xE9 | 0xEB | 0xE5 | 0xF5 | 0xED | 0xFD | 0xF9 | 0xE1 | 0xF1 => {
                let v = self.operand_value();
                self.add(v ^ 0xFF);
            }
            0xC9 | 0xC5 | 0xD5 | 0xCD | 0xDD | 0xD9 | 0xC1 | 0xD1 => {
                let v = self.operand_value();
                self.cmp(self.a, v);
            }
            0xE0 | 0xE4 | 0xEC => {
                let v = self.operand_value();
                self.cmp(self.x, v);
            }
            0xC0 | 0xC4 | 0xCC => {
                let v = self.operand_value();
                self.cmp(self.y, v);
            }
            // Inc / dec
            0xE6 | 0xF6 | 0xEE | 0xFE => {
                self.rmw(|s, v| {
                    let r = v.wrapping_add(1);
                    s.set_zn(r);
                    r
                });
            }
            0xC6 | 0xD6 | 0xCE | 0xDE => {
                self.rmw(|s, v| {
                    let r = v.wrapping_sub(1);
                    s.set_zn(r);
                    r
                });
            }
            0xE8 => {
                self.x = self.x.wrapping_add(1);
                self.set_zn(self.x);
            }
            0xCA => {
                self.x = self.x.wrapping_sub(1);
                self.set_zn(self.x);
            }
            0xC8 => {
                self.y = self.y.wrapping_add(1);
                self.set_zn(self.y);
            }
            0x88 => {
                self.y = self.y.wrapping_sub(1);
                self.set_zn(self.y);
            }
            // Shifts
            0x0A => self.a = self.asl(self.a),
            0x4A => self.a = self.lsr(self.a),
            0x2A => self.a = self.rol(self.a),
            0x6A => self.a = self.ror(self.a),
            0x06 | 0x16 | 0x0E | 0x1E => {
                self.rmw(Self::asl);
            }
            0x46 | 0x56 | 0x4E | 0x5E => {
                self.rmw(Self::lsr);
            }
            0x26 | 0x36 | 0x2E | 0x3E => {
                self.rmw(Self::rol);
            }
            0x66 | 0x76 | 0x6E | 0x7E => {
                self.rmw(Self::ror);
            }
            // Jumps
            0x4C => self.pc = self.operand,
            0x6C => {
                let a = self.operand;
                let lo = self.read(a) as u16;
                let hi = self.read((a & 0xFF00) | (a.wrapping_add(1) & 0x00FF)) as u16;
                self.pc = (hi << 8) | lo;
            }
            0x20 => {
                let lo = self.read_byte() as u16;
                self.dummy_stack_read();
                self.push16(self.pc);
                let hi = self.read_byte() as u16;
                self.pc = (hi << 8) | lo;
            }
            0x60 => {
                self.dummy_stack_read();
                let a = self.pop16();
                self.pc = a;
                self.dummy_pc_read();
                self.pc = a.wrapping_add(1);
            }
            0x40 => {
                self.dummy_stack_read();
                let p = self.pop();
                self.status = p & 0xCF;
                self.pc = self.pop16();
            }
            0x00 => {
                self.push16(self.pc.wrapping_add(1));
                let flags = self.status | B | U;
                if self.need_nmi {
                    self.need_nmi = false;
                    self.push(flags);
                    self.status |= I;
                    self.pc = self.read_u16(0xFFFA);
                } else {
                    self.push(flags);
                    self.status |= I;
                    self.pc = self.read_u16(0xFFFE);
                }
                self.prev_need_nmi = false;
            }
            // Branches
            0x10 => self.branch(!self.flag(N)),
            0x30 => self.branch(self.flag(N)),
            0x50 => self.branch(!self.flag(V)),
            0x70 => self.branch(self.flag(V)),
            0x90 => self.branch(!self.flag(C)),
            0xB0 => self.branch(self.flag(C)),
            0xD0 => self.branch(!self.flag(Z)),
            0xF0 => self.branch(self.flag(Z)),
            // Flags
            0x18 => self.status &= !C,
            0x38 => self.status |= C,
            0x58 => self.status &= !I,
            0x78 => self.status |= I,
            0xB8 => self.status &= !V,
            0xD8 => self.status &= !D,
            0xF8 => self.status |= D,
            // NOPs (including unofficial multi-byte NOPs, which still perform their reads)
            0xEA | 0x1A | 0x3A | 0x5A | 0x7A | 0xDA | 0xFA => {}
            0x80 | 0x82 | 0x89 | 0xC2 | 0xE2 | 0x04 | 0x44 | 0x64 | 0x14 | 0x34 | 0x54 | 0x74 | 0xD4 | 0xF4 | 0x0C | 0x1C
            | 0x3C | 0x5C | 0x7C | 0xDC | 0xFC => {
                self.operand_value();
            }
            // Unofficial
            0xA7 | 0xB7 | 0xAF | 0xBF | 0xA3 | 0xB3 => {
                let v = self.operand_value();
                self.a = v;
                self.x = v;
                self.set_zn(v);
            }
            0x87 | 0x97 | 0x8F | 0x83 => self.write(self.operand, self.a & self.x),
            0x07 | 0x17 | 0x0F | 0x1F | 0x1B | 0x03 | 0x13 => {
                let r = self.rmw(Self::asl);
                self.a |= r;
                self.set_zn(self.a);
            }
            0x27 | 0x37 | 0x2F | 0x3F | 0x3B | 0x23 | 0x33 => {
                let r = self.rmw(Self::rol);
                self.a &= r;
                self.set_zn(self.a);
            }
            0x47 | 0x57 | 0x4F | 0x5F | 0x5B | 0x43 | 0x53 => {
                let r = self.rmw(Self::lsr);
                self.a ^= r;
                self.set_zn(self.a);
            }
            0x67 | 0x77 | 0x6F | 0x7F | 0x7B | 0x63 | 0x73 => {
                let r = self.rmw(Self::ror);
                self.add(r);
            }
            0xC7 | 0xD7 | 0xCF | 0xDF | 0xDB | 0xC3 | 0xD3 => {
                let r = self.rmw(|_, v| v.wrapping_sub(1));
                self.cmp(self.a, r);
            }
            0xE7 | 0xF7 | 0xEF | 0xFF | 0xFB | 0xE3 | 0xF3 => {
                let r = self.rmw(|_, v| v.wrapping_add(1));
                self.add(r ^ 0xFF);
            }
            0x0B | 0x2B => {
                let v = self.operand_value();
                self.a &= v;
                self.set_zn(self.a);
                self.set_flag(C, self.a & 0x80 != 0);
            }
            0x4B => {
                let v = self.operand_value();
                self.a &= v;
                self.set_flag(C, self.a & 1 != 0);
                self.a >>= 1;
                self.set_zn(self.a);
            }
            0x6B => {
                let v = self.operand_value();
                self.a = ((self.a & v) >> 1) | ((self.status & C) << 7);
                self.set_zn(self.a);
                self.set_flag(C, self.a & 0x40 != 0);
                self.set_flag(V, ((self.a >> 6) ^ (self.a >> 5)) & 1 != 0);
            }
            0xAB => {
                let v = self.operand_value();
                self.a = v;
                self.x = v;
                self.set_zn(v);
            }
            0x8B => {
                let v = self.operand_value();
                self.a = (self.a | 0xEE) & self.x & v;
                self.set_zn(self.a);
            }
            0xCB => {
                let v = self.operand_value();
                let ax = self.a & self.x;
                self.set_flag(C, ax >= v);
                self.x = ax.wrapping_sub(v);
                self.set_zn(self.x);
            }
            0xBB => {
                let v = self.operand_value() & self.sp;
                self.a = v;
                self.x = v;
                self.sp = v;
                self.set_zn(v);
            }
            0x9C => {
                let base = self.read_word();
                self.sh_store(base, self.x, self.y);
            }
            0x9E => {
                let base = self.read_word();
                self.sh_store(base, self.y, self.x);
            }
            0x9F => {
                let base = self.read_word();
                self.sh_store(base, self.y, self.x & self.a);
            }
            0x93 => {
                let z = self.read_byte();
                let lo = self.read(z as u16) as u16;
                let hi = self.read(z.wrapping_add(1) as u16) as u16;
                self.sh_store((hi << 8) | lo, self.y, self.x & self.a);
            }
            0x9B => {
                let base = self.read_word();
                self.sh_store(base, self.y, self.x & self.a);
                self.sp = self.x & self.a;
            }
            // KIL / JAM
            0x02 | 0x12 | 0x22 | 0x32 | 0x42 | 0x52 | 0x62 | 0x72 | 0x92 | 0xB2 | 0xD2 | 0xF2 => {
                self.pc = self.pc.wrapping_sub(1);
                self.prev_run_irq = false;
                self.prev_need_nmi = false;
                self.jammed = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal NROM-128 iNES image whose reset handler at $8000 runs `code`.
    fn make_test_rom(code: &[u8]) -> Vec<u8> {
        let mut rom = vec![0u8; 16 + 0x4000];
        rom[0..4].copy_from_slice(b"NES\x1A");
        rom[4] = 1; // 16 KB PRG
        rom[5] = 0; // CHR-RAM
        rom[16..16 + code.len()].copy_from_slice(code);
        // Reset vector ($FFFC-$FFFD in $C000 mirror) -> $8000
        rom[16 + 0x3FFC] = 0x00;
        rom[16 + 0x3FFD] = 0x80;
        rom
    }

    #[test]
    fn save_state_bytes_and_disk_slot_roundtrip_is_bit_exact() {
        // Loop at $8000: INC $10; INX; BNE $8000; INC $11; JMP $8000
        let code = [0xE6, 0x10, 0xE8, 0xD0, 0xFB, 0xE6, 0x11, 0x4C, 0x00, 0x80];
        let rom_bytes = make_test_rom(&code);
        let tmp_rom = std::env::temp_dir().join("lockstep_test_savestate.nes");
        std::fs::write(&tmp_rom, &rom_bytes).unwrap();

        let cart1 = Cartridge::from_file(&tmp_rom).unwrap();
        let cart2 = Cartridge::from_file(&tmp_rom).unwrap();
        let mut sys1 = System::new(cart1).unwrap();
        let mut sys2 = System::new(cart2).unwrap();

        for _ in 0..5 {
            sys1.run_frame();
        }
        let state_path = sys1.save_state_file(3).unwrap();
        assert!(state_path.exists());

        sys2.load_state_file(3).unwrap();

        for _ in 0..5 {
            sys1.run_frame();
            sys2.run_frame();
        }

        assert_eq!(sys1.pc, sys2.pc);
        assert_eq!(sys1.a, sys2.a);
        assert_eq!(sys1.x, sys2.x);
        assert_eq!(sys1.y, sys2.y);
        assert_eq!(sys1.cycle_count, sys2.cycle_count);
        assert_eq!(sys1.ram, sys2.ram);
        assert_eq!(&sys1.ppu.framebuffer[..], &sys2.ppu.framebuffer[..]);

        let _ = std::fs::remove_file(&tmp_rom);
        let _ = std::fs::remove_file(&state_path);
    }

    #[test]
    fn zapper_light_sense_and_trigger_pulse_on_4017() {
        // Simple loop at $8000: JMP $8000
        let rom_bytes = make_test_rom(&[0x4C, 0x00, 0x80]);
        let cart = Cartridge::from_ines_bytes(&rom_bytes, "Duck Hunt Test".into()).unwrap();
        let mut sys = System::new(cart).unwrap();
        // Title "Duck Hunt Test" auto-connects the Zapper.
        assert!(sys.joypad.zapper_connected);

        // Set backdrop palette $3F00 to bright white ($30) and run one frame so `pixels` is filled with $30.
        sys.ppu.palette[0] = 0x30;
        sys.joypad.set_zapper(128, 100, 2);

        // Aimed off-screen: bit 3 (0x08 = dark) and bit 4 (0x10 = trigger) are both set.
        sys.joypad.set_zapper(-1, -1, 2);
        let v_off = sys.bus_read(0x4017);
        assert_eq!(v_off & 0x18, 0x18);

        // Run a frame so `pixels` are rendered with palette[0] = $30 (white), and trigger decrements from 2 -> 1.
        sys.run_frame();
        assert_eq!(sys.joypad.zapper_trigger_frames, 1);

        // Position PPU beam at scanline 102, cycle 140 (just past (128, 100)).
        sys.ppu.scanline = 102;
        sys.ppu.cycle = 140;
        sys.joypad.set_zapper(128, 100, 0);
        assert!(sys.ppu.zapper_sees_light(128, 100));
        let v_on = sys.bus_read(0x4017);
        // Bit 3 (0x08) must be 0 when light is detected; bit 4 (0x10) is still 1 for 1 more frame.
        assert_eq!(v_on & 0x08, 0x00);
        assert_eq!(v_on & 0x10, 0x10);
    }
}
