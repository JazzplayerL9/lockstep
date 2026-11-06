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

const C: u8 = 0x01;
const Z: u8 = 0x02;
const I: u8 = 0x04;
const D: u8 = 0x08;
const B: u8 = 0x10;
const U: u8 = 0x20;
const V: u8 = 0x40;
const N: u8 = 0x80;

/// Extra idle PPU scanlines inserted before VBlank when the "overclock" option is on.
pub const OVERCLOCK_SCANLINES: u16 = 100;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Imp,
    Acc,
    Imm,
    Rel,
    Zero,
    ZeroX,
    ZeroY,
    Ind,
    IndX,
    IndY,
    IndYW,
    Abs,
    AbsX,
    AbsXW,
    AbsY,
    AbsYW,
    Other,
}

use Mode::*;

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

#[derive(Clone)]
pub struct System {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    pub status: u8,
    pub cycle_count: u64,
    master_clock: u64,
    operand: u16,
    mode: Mode,

    // Interrupt state.
    prev_nmi_line: bool,
    need_nmi: bool,
    prev_need_nmi: bool,
    run_irq: bool,
    prev_run_irq: bool,

    // DMA state.
    need_halt: bool,
    need_dummy_read: bool,
    sprite_dma: bool,
    sprite_dma_page: u8,
    dmc_dma_running: bool,
    abort_dmc_dma: bool,

    /// CPU executed a KIL/JAM opcode.
    pub jammed: bool,
    open_bus: u8,
    apu_skipped_cycles: u64,

    pub ram: [u8; 2048],
    pub ppu: Ppu,
    pub apu: Apu,
    pub mapper: Box<dyn Mapper>,
    pub joypad: Joypad,
    pub overclock_enabled: bool,
    pub title: String,
    cart_for_sram: Cartridge,
}

impl System {
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
            title,
            cart_for_sram,
        };
        sys.power_on();
        Ok(sys)
    }

    /// Back-compat constructor used by older call sites; panics on unsupported mappers.
    pub fn from_cartridge(cart: Cartridge) -> Self {
        Self::new(cart).expect("unsupported cartridge")
    }

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
        self.sprite_dma = false;
        self.dmc_dma_running = false;
        self.jammed = false;
        self.ppu.reset();
        self.apu.reset();
        self.pc = self.peek_u16(0xFFFC);
        for _ in 0..8 {
            self.start_cycle(true);
            self.end_cycle(true);
        }
    }

    pub fn save_sram(&self) {
        self.cart_for_sram.save_battery_sram(&self.mapper.battery_ram());
    }

    pub fn mapper_name(&self) -> &'static str {
        self.mapper.name()
    }

    #[inline]
    pub fn set_overclock(&mut self, on: bool) {
        self.overclock_enabled = on;
        self.ppu.extra_scanlines = if on { OVERCLOCK_SCANLINES } else { 0 };
    }

    // ---------------------------------------------------------------- frame loop

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
    }

    // ---------------------------------------------------------------- clocking

    #[inline]
    fn start_cycle(&mut self, read: bool) {
        self.master_clock += if read { 5 } else { 7 };
        self.ppu.run(self.master_clock - 1, &mut *self.mapper);
        self.cycle_count = self.cycle_count.wrapping_add(1);
        self.ppu.cpu_cycle = self.cycle_count;
        self.mapper.cpu_clock();
        if self.ppu.apu_enabled {
            if self.apu_skipped_cycles & 1 == 1 {
                // Re-sync APU odd/even phase after overclock lines.
                self.apu.clock();
            }
            self.apu_skipped_cycles = 0;
            self.apu.expansion = self.mapper.audio_sample();
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

    fn start_dmc_transfer(&mut self) {
        self.dmc_dma_running = true;
        self.need_dummy_read = true;
        self.need_halt = true;
    }

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
            0x4017 => (self.open_bus & 0xE0) | self.joypad.read_p2(),
            0x4000..=0x401F => self.open_bus,
            _ => self.mapper.cpu_read(addr).unwrap_or(self.open_bus),
        };
        self.open_bus = v;
        v
    }

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

    #[inline]
    fn write(&mut self, addr: u16, val: u8) {
        self.start_cycle(false);
        self.bus_write(addr, val);
        self.end_cycle(false);
    }

    fn dma_read(&mut self, addr: u16) -> u8 {
        // DMA can't see the 2A03's internal registers.
        if (0x4015..=0x401A).contains(&addr) { self.open_bus } else { self.bus_read(addr) }
    }

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

    #[inline]
    fn dummy_pc_read(&mut self) {
        self.read(self.pc);
    }

    #[inline]
    fn read_byte(&mut self) -> u8 {
        let v = self.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }

    #[inline]
    fn read_word(&mut self) -> u16 {
        let lo = self.read_byte() as u16;
        let hi = self.read_byte() as u16;
        (hi << 8) | lo
    }

    #[inline]
    fn read_u16(&mut self, addr: u16) -> u16 {
        let lo = self.read(addr) as u16;
        let hi = self.read(addr.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }

    #[inline]
    fn push(&mut self, v: u8) {
        self.write(0x100 | self.sp as u16, v);
        self.sp = self.sp.wrapping_sub(1);
    }

    #[inline]
    fn push16(&mut self, v: u16) {
        self.push((v >> 8) as u8);
        self.push(v as u8);
    }

    #[inline]
    fn pop(&mut self) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        self.read(0x100 | self.sp as u16)
    }

    #[inline]
    fn pop16(&mut self) -> u16 {
        let lo = self.pop() as u16;
        let hi = self.pop() as u16;
        (hi << 8) | lo
    }

    #[inline]
    fn dummy_stack_read(&mut self) {
        self.read(0x100 | self.sp as u16);
    }

    #[inline]
    fn set_zn(&mut self, v: u8) {
        self.status &= !(Z | N);
        if v == 0 {
            self.status |= Z;
        }
        self.status |= v & N;
    }

    #[inline]
    fn flag(&self, f: u8) -> bool {
        self.status & f != 0
    }

    #[inline]
    fn set_flag(&mut self, f: u8, on: bool) {
        if on {
            self.status |= f;
        } else {
            self.status &= !f;
        }
    }

    #[inline]
    fn page_crossed(a: u16, b: u8) -> bool {
        (a.wrapping_add(b as u16) & 0xFF00) != (a & 0xFF00)
    }

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

    #[inline]
    fn operand_value(&mut self) -> u8 {
        match self.mode {
            Imm | Rel | Acc | Imp | Other => self.operand as u8,
            _ => self.read(self.operand),
        }
    }

    fn add(&mut self, v: u8) {
        let c = (self.status & C) as u16;
        let r = self.a as u16 + v as u16 + c;
        self.set_flag(V, (!(self.a ^ v) & (self.a ^ r as u8) & 0x80) != 0);
        self.set_flag(C, r > 0xFF);
        self.a = r as u8;
        self.set_zn(self.a);
    }

    fn cmp(&mut self, reg: u8, v: u8) {
        self.set_flag(C, reg >= v);
        self.set_zn(reg.wrapping_sub(v));
    }

    fn asl(&mut self, v: u8) -> u8 {
        self.set_flag(C, v & 0x80 != 0);
        let r = v << 1;
        self.set_zn(r);
        r
    }

    fn lsr(&mut self, v: u8) -> u8 {
        self.set_flag(C, v & 1 != 0);
        let r = v >> 1;
        self.set_zn(r);
        r
    }

    fn rol(&mut self, v: u8) -> u8 {
        let c = self.status & C;
        self.set_flag(C, v & 0x80 != 0);
        let r = (v << 1) | c;
        self.set_zn(r);
        r
    }

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

    fn branch(&mut self, cond: bool) {
        let offset = self.operand as u8 as i8;
        if cond {
            // A taken non-crossing branch delays interrupts by one instruction.
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

    pub fn step_instruction(&mut self) {
        let op = self.read_byte();
        self.mode = ADDR_MODE[op as usize];
        self.operand = self.fetch_operand();
        self.execute(op);
        if self.prev_run_irq || self.prev_need_nmi {
            self.interrupt();
        }
    }

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
