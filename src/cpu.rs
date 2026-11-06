//! Ricoh 2A03 / NMOS 6502 CPU core, system bus interconnect, hardware NMI/IRQ handling,
//! save-state snapshot support, and universal Post-Render Scanline 240 CPU Overclocking.

use crate::apu::Apu;
use crate::cartridge::Cartridge;
use crate::input::Joypad;
use crate::mapper::Mapper;
use crate::ppu::Ppu;

const FLAG_C: u8 = 0x01;
const FLAG_Z: u8 = 0x02;
const FLAG_I: u8 = 0x04;
const FLAG_D: u8 = 0x08;
const FLAG_B: u8 = 0x10;
const FLAG_U: u8 = 0x20;
const FLAG_V: u8 = 0x40;
const FLAG_N: u8 = 0x80;

#[derive(Clone)]
pub struct System {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    pub status: u8,
    pub ram: [u8; 2048],
    pub mapper: Mapper,
    pub ppu: Ppu,
    pub apu: Apu,
    pub joypad: Joypad,
    pub total_cycles: u64,
    pub overclock_enabled: bool,
    pub overclock_cycles_remaining: u32,
    in_nmi_handler: bool,
    last_branch_target: u16,
    spin_loop_hits: u8,
    /// CPU cycle (0-based) within the current instruction on which its PPU bus access lands.
    access_cycle_offset: u32,
    /// CPU cycles already ticked mid-instruction to align a PPU register access.
    cycles_pre_ticked: u32,
    /// Whether the current opcode's bus access shifts one cycle later on a page cross.
    access_adds_page: bool,
    /// Page-cross penalty recorded by the indexed addressing helpers.
    access_page_extra: u32,
    /// NMI/IRQ line state sampled on the second-to-last cycle of the previous instruction.
    nmi_latched: bool,
    irq_latched: bool,
}

impl System {
    pub fn from_cartridge(cart: Cartridge) -> Self {
        let mapper = Mapper::from_cartridge(cart);
        let mut sys = Self {
            a: 0,
            x: 0,
            y: 0,
            sp: 0xFD,
            pc: 0,
            status: FLAG_I | FLAG_U,
            ram: [0; 2048],
            mapper,
            ppu: Ppu::new(),
            apu: Apu::new(),
            joypad: Joypad::new(),
            total_cycles: 0,
            overclock_enabled: true,
            overclock_cycles_remaining: 0,
            in_nmi_handler: false,
            last_branch_target: 0,
            spin_loop_hits: 0,
            access_cycle_offset: 0,
            cycles_pre_ticked: 0,
            access_adds_page: false,
            access_page_extra: 0,
            nmi_latched: false,
            irq_latched: false,
        };
        sys.reset();
        sys
    }

    pub fn reset(&mut self) {
        self.sp = 0xFD;
        self.status = FLAG_I | FLAG_U;
        self.in_nmi_handler = false;
        self.spin_loop_hits = 0;
        self.pc = self.read_u16(0xFFFC);
    }

    pub fn save_sram(&self) {
        self.mapper.cart.save_battery_sram(&self.mapper.cart.prg_ram);
    }

    #[inline]
    pub fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x1FFF => self.ram[(addr & 0x07FF) as usize],
            0x2000..=0x3FFF => {
                self.catch_up_to_bus_cycle();
                self.ppu.cpu_read(addr, &self.mapper)
            }
            0x4015 => self.apu.read_status(),
            0x4016 => self.joypad.read_p1(),
            0x4017 => self.joypad.read_p2(),
            0x4000..=0x401F => 0,
            0x6000..=0xFFFF => self.mapper.read_prg(addr),
            _ => 0,
        }
    }

    #[inline]
    pub fn write(&mut self, addr: u16, val: u8) {
        match addr {
            0x0000..=0x1FFF => {
                self.ram[(addr & 0x07FF) as usize] = val;
            }
            0x2000..=0x3FFF => {
                self.catch_up_to_bus_cycle();
                self.ppu.cpu_write(addr, val, &mut self.mapper);
            }
            0x4014 => {
                let base = (val as u16) << 8;
                for i in 0..256u16 {
                    let byte = self.read(base.wrapping_add(i));
                    let oam_idx = self.ppu.oam_addr.wrapping_add(i as u8) as usize;
                    self.ppu.oam[oam_idx] = byte;
                }
                // 513 cycles, +1 alignment cycle when DMA starts on an odd CPU cycle.
                let odd = (self.total_cycles.wrapping_add(3) & 1) == 1;
                self.tick_hardware(if odd { 514 } else { 513 });
            }
            0x4016 => {
                self.joypad.write_strobe(val);
            }
            0x4000..=0x4017 => {
                self.apu.write_reg(addr, val);
            }
            0x6000..=0xFFFF => {
                self.mapper.write_prg(addr, val, self.total_cycles);
            }
            _ => {}
        }
    }

    /// The 6502 performs the data read/write of load/store instructions on the instruction's
    /// final bus cycle. Advance the PPU/APU up to that cycle before touching a PPU register so
    /// mid-scanline writes ($2001/$2005/$2006) land on the correct dot instead of at opcode fetch.
    #[inline]
    fn catch_up_to_bus_cycle(&mut self) {
        if self.cycles_pre_ticked == 0 && self.access_cycle_offset > 0 {
            let n = self.access_cycle_offset
                + if self.access_adds_page { self.access_page_extra } else { 0 };
            self.tick_hardware(n);
            self.cycles_pre_ticked = n;
        }
    }

    /// Loads through abs,X / abs,Y / (zp),Y take an extra cycle on page crossing,
    /// which also delays their data read.
    #[inline]
    fn access_adds_page(op: u8) -> bool {
        matches!(op, 0xBD | 0xB9 | 0xBC | 0xBE | 0xBF | 0xB1 | 0xB3)
    }

    /// Bus cycle offset of the data access for opcodes that can target PPU registers.
    #[inline]
    fn bus_access_offset(op: u8) -> u32 {
        match op {
            // LDA/LAX (zp),Y: read on cycle 5 (+1 on page cross)
            0xB1 | 0xB3 => 4,
            // LDA/LAX (zp,X): read on cycle 6
            0xA1 | 0xA3 => 5,
            // STA/STX/STY/SAX zp: write on cycle 3
            0x85 | 0x86 | 0x84 | 0x87 => 2,
            // zp,X / zp,Y and absolute loads/stores/BIT: data on cycle 4
            0x95 | 0x96 | 0x94 | 0x97 => 3,
            0x8D | 0x8E | 0x8C | 0x8F => 3,
            0xAD | 0xAE | 0xAC | 0x2C | 0xAF => 3,
            0xBD | 0xB9 | 0xBC | 0xBE | 0xBF => 3,
            // STA abs,X / abs,Y: write on cycle 5
            0x9D | 0x99 => 4,
            // STA (zp,X) / (zp),Y: write on cycle 6
            0x81 | 0x91 => 5,
            _ => 0,
        }
    }

    #[inline]
    fn read_u16(&mut self, addr: u16) -> u16 {
        let lo = self.read(addr) as u16;
        let hi = self.read(addr.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }

    #[inline]
    fn push(&mut self, val: u8) {
        let addr = 0x0100 | (self.sp as u16);
        self.write(addr, val);
        self.sp = self.sp.wrapping_sub(1);
    }

    #[inline]
    fn pop(&mut self) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        let addr = 0x0100 | (self.sp as u16);
        self.read(addr)
    }

    #[inline]
    fn push_u16(&mut self, val: u16) {
        self.push((val >> 8) as u8);
        self.push((val & 0xFF) as u8);
    }

    #[inline]
    fn pop_u16(&mut self) -> u16 {
        let lo = self.pop() as u16;
        let hi = self.pop() as u16;
        (hi << 8) | lo
    }

    #[inline]
    fn set_zn(&mut self, val: u8) {
        if val == 0 {
            self.status |= FLAG_Z;
        } else {
            self.status &= !FLAG_Z;
        }
        if (val & 0x80) != 0 {
            self.status |= FLAG_N;
        } else {
            self.status &= !FLAG_N;
        }
    }

    #[inline]
    fn set_flag(&mut self, flag: u8, cond: bool) {
        if cond {
            self.status |= flag;
        } else {
            self.status &= !flag;
        }
    }

    #[inline]
    fn get_flag(&self, flag: u8) -> bool {
        (self.status & flag) != 0
    }

    #[inline]
    fn tick_hardware(&mut self, cpu_cycles: u32) {
        self.total_cycles = self.total_cycles.wrapping_add(cpu_cycles as u64);
        // Universal Post-Render Scanline 240 CPU Overclock:
        // After all 240 visible scanlines (0..=239) and mid-frame raster effects complete,
        // if the 6502 is still actively computing the frame (either inside an NMI-driven
        // frame loop or before hitting its VBlank wait spin-loop), grant extra post-render
        // CPU cycles before scanline 241 VBlank NMI fires.
        if self.overclock_enabled
            && self.ppu.scanline == 240
            && (self.ppu.ctrl & 0x80) != 0
            && (self.in_nmi_handler || self.spin_loop_hits < 4)
            && self.overclock_cycles_remaining > 0
        {
            self.overclock_cycles_remaining =
                self.overclock_cycles_remaining.saturating_sub(cpu_cycles);
            return;
        }
        self.ppu.step(cpu_cycles * 3, &mut self.mapper);
        self.apu.step(cpu_cycles, &self.mapper);
        // DMC sample-fetch DMA halts the CPU while PPU/APU keep running.
        loop {
            let stall = std::mem::take(&mut self.apu.dmc.stall_cycles);
            if stall == 0 {
                break;
            }
            self.total_cycles = self.total_cycles.wrapping_add(stall as u64);
            self.ppu.step(stall * 3, &mut self.mapper);
            self.apu.step(stall, &self.mapper);
        }
    }

    pub fn run_frame(&mut self) {
        self.ppu.frame_complete = false;
        self.overclock_cycles_remaining = if self.overclock_enabled { 60_000 } else { 0 };
        self.spin_loop_hits = 0;
        while !self.ppu.frame_complete {
            self.step_instruction();
        }
    }

    pub fn step_instruction(&mut self) {
        // Interrupts are taken based on the line state sampled during the previous
        // instruction's second-to-last cycle (6502 interrupt polling latency).
        if self.nmi_latched {
            self.nmi_latched = false;
            self.irq_latched = false;
            self.ppu.nmi_pending = false;
            self.nmi();
        } else if self.irq_latched {
            self.irq_latched = false;
            self.irq();
        }

        let instr_pc = self.pc;
        let i_flag_before = self.get_flag(FLAG_I);
        let op = self.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        self.access_cycle_offset = Self::bus_access_offset(op);
        self.access_adds_page = Self::access_adds_page(op);
        self.access_page_extra = 0;
        self.cycles_pre_ticked = 0;

        let cycles = match op {
            // LDA
            0xA9 => { let v = self.imm(); self.a = v; self.set_zn(self.a); 2 }
            0xA5 => { let a = self.zp(); self.a = self.read(a); self.set_zn(self.a); 3 }
            0xB5 => { let a = self.zpx(); self.a = self.read(a); self.set_zn(self.a); 4 }
            0xAD => { let a = self.abs(); self.a = self.read(a); self.set_zn(self.a); 4 }
            0xBD => { let (a, p) = self.abx(); self.a = self.read(a); self.set_zn(self.a); 4 + p }
            0xB9 => { let (a, p) = self.aby(); self.a = self.read(a); self.set_zn(self.a); 4 + p }
            0xA1 => { let a = self.inx(); self.a = self.read(a); self.set_zn(self.a); 6 }
            0xB1 => { let (a, p) = self.iny(); self.a = self.read(a); self.set_zn(self.a); 5 + p }

            // LDX
            0xA2 => { let v = self.imm(); self.x = v; self.set_zn(self.x); 2 }
            0xA6 => { let a = self.zp(); self.x = self.read(a); self.set_zn(self.x); 3 }
            0xB6 => { let a = self.zpy(); self.x = self.read(a); self.set_zn(self.x); 4 }
            0xAE => { let a = self.abs(); self.x = self.read(a); self.set_zn(self.x); 4 }
            0xBE => { let (a, p) = self.aby(); self.x = self.read(a); self.set_zn(self.x); 4 + p }

            // LDY
            0xA0 => { let v = self.imm(); self.y = v; self.set_zn(self.y); 2 }
            0xA4 => { let a = self.zp(); self.y = self.read(a); self.set_zn(self.y); 3 }
            0xB4 => { let a = self.zpx(); self.y = self.read(a); self.set_zn(self.y); 4 }
            0xAC => { let a = self.abs(); self.y = self.read(a); self.set_zn(self.y); 4 }
            0xBC => { let (a, p) = self.abx(); self.y = self.read(a); self.set_zn(self.y); 4 + p }

            // STA
            0x85 => { let a = self.zp(); self.write(a, self.a); 3 }
            0x95 => { let a = self.zpx(); self.write(a, self.a); 4 }
            0x8D => { let a = self.abs(); self.write(a, self.a); 4 }
            0x9D => { let (a, _) = self.abx(); self.write(a, self.a); 5 }
            0x99 => { let (a, _) = self.aby(); self.write(a, self.a); 5 }
            0x81 => { let a = self.inx(); self.write(a, self.a); 6 }
            0x91 => { let (a, _) = self.iny(); self.write(a, self.a); 6 }

            // STX
            0x86 => { let a = self.zp(); self.write(a, self.x); 3 }
            0x96 => { let a = self.zpy(); self.write(a, self.x); 4 }
            0x8E => { let a = self.abs(); self.write(a, self.x); 4 }

            // STY
            0x84 => { let a = self.zp(); self.write(a, self.y); 3 }
            0x94 => { let a = self.zpx(); self.write(a, self.y); 4 }
            0x8C => { let a = self.abs(); self.write(a, self.y); 4 }

            // Transfers
            0xAA => { self.x = self.a; self.set_zn(self.x); 2 }
            0xA8 => { self.y = self.a; self.set_zn(self.y); 2 }
            0xBA => { self.x = self.sp; self.set_zn(self.x); 2 }
            0x8A => { self.a = self.x; self.set_zn(self.a); 2 }
            0x9A => { self.sp = self.x; 2 }
            0x98 => { self.a = self.y; self.set_zn(self.a); 2 }

            // Stack
            0x48 => { self.push(self.a); 3 }
            0x08 => { self.push(self.status | FLAG_B | FLAG_U); 3 }
            0x68 => { self.a = self.pop(); self.set_zn(self.a); 4 }
            0x28 => { self.status = (self.pop() & !FLAG_B) | FLAG_U; 4 }

            // Logical AND
            0x29 => { let v = self.imm(); self.a &= v; self.set_zn(self.a); 2 }
            0x25 => { let a = self.zp(); self.a &= self.read(a); self.set_zn(self.a); 3 }
            0x35 => { let a = self.zpx(); self.a &= self.read(a); self.set_zn(self.a); 4 }
            0x2D => { let a = self.abs(); self.a &= self.read(a); self.set_zn(self.a); 4 }
            0x3D => { let (a, p) = self.abx(); self.a &= self.read(a); self.set_zn(self.a); 4 + p }
            0x39 => { let (a, p) = self.aby(); self.a &= self.read(a); self.set_zn(self.a); 4 + p }
            0x21 => { let a = self.inx(); self.a &= self.read(a); self.set_zn(self.a); 6 }
            0x31 => { let (a, p) = self.iny(); self.a &= self.read(a); self.set_zn(self.a); 5 + p }

            // Logical ORA
            0x09 => { let v = self.imm(); self.a |= v; self.set_zn(self.a); 2 }
            0x05 => { let a = self.zp(); self.a |= self.read(a); self.set_zn(self.a); 3 }
            0x15 => { let a = self.zpx(); self.a |= self.read(a); self.set_zn(self.a); 4 }
            0x0D => { let a = self.abs(); self.a |= self.read(a); self.set_zn(self.a); 4 }
            0x1D => { let (a, p) = self.abx(); self.a |= self.read(a); self.set_zn(self.a); 4 + p }
            0x19 => { let (a, p) = self.aby(); self.a |= self.read(a); self.set_zn(self.a); 4 + p }
            0x01 => { let a = self.inx(); self.a |= self.read(a); self.set_zn(self.a); 6 }
            0x11 => { let (a, p) = self.iny(); self.a |= self.read(a); self.set_zn(self.a); 5 + p }

            // Logical EOR
            0x49 => { let v = self.imm(); self.a ^= v; self.set_zn(self.a); 2 }
            0x45 => { let a = self.zp(); self.a ^= self.read(a); self.set_zn(self.a); 3 }
            0x55 => { let a = self.zpx(); self.a ^= self.read(a); self.set_zn(self.a); 4 }
            0x4D => { let a = self.abs(); self.a ^= self.read(a); self.set_zn(self.a); 4 }
            0x5D => { let (a, p) = self.abx(); self.a ^= self.read(a); self.set_zn(self.a); 4 + p }
            0x59 => { let (a, p) = self.aby(); self.a ^= self.read(a); self.set_zn(self.a); 4 + p }
            0x41 => { let a = self.inx(); self.a ^= self.read(a); self.set_zn(self.a); 6 }
            0x51 => { let (a, p) = self.iny(); self.a ^= self.read(a); self.set_zn(self.a); 5 + p }

            // BIT
            0x24 => {
                let a = self.zp();
                let v = self.read(a);
                self.set_flag(FLAG_Z, (self.a & v) == 0);
                self.set_flag(FLAG_V, (v & 0x40) != 0);
                self.set_flag(FLAG_N, (v & 0x80) != 0);
                3
            }
            0x2C => {
                let a = self.abs();
                let v = self.read(a);
                self.set_flag(FLAG_Z, (self.a & v) == 0);
                self.set_flag(FLAG_V, (v & 0x40) != 0);
                self.set_flag(FLAG_N, (v & 0x80) != 0);
                4
            }

            // ADC
            0x69 => { let v = self.imm(); self.adc(v); 2 }
            0x65 => { let a = self.zp(); let v = self.read(a); self.adc(v); 3 }
            0x75 => { let a = self.zpx(); let v = self.read(a); self.adc(v); 4 }
            0x6D => { let a = self.abs(); let v = self.read(a); self.adc(v); 4 }
            0x7D => { let (a, p) = self.abx(); let v = self.read(a); self.adc(v); 4 + p }
            0x79 => { let (a, p) = self.aby(); let v = self.read(a); self.adc(v); 4 + p }
            0x61 => { let a = self.inx(); let v = self.read(a); self.adc(v); 6 }
            0x71 => { let (a, p) = self.iny(); let v = self.read(a); self.adc(v); 5 + p }

            // SBC
            0xE9 | 0xEB => { let v = self.imm(); self.sbc(v); 2 }
            0xE5 => { let a = self.zp(); let v = self.read(a); self.sbc(v); 3 }
            0xF5 => { let a = self.zpx(); let v = self.read(a); self.sbc(v); 4 }
            0xED => { let a = self.abs(); let v = self.read(a); self.sbc(v); 4 }
            0xFD => { let (a, p) = self.abx(); let v = self.read(a); self.sbc(v); 4 + p }
            0xF9 => { let (a, p) = self.aby(); let v = self.read(a); self.sbc(v); 4 + p }
            0xE1 => { let a = self.inx(); let v = self.read(a); self.sbc(v); 6 }
            0xF1 => { let (a, p) = self.iny(); let v = self.read(a); self.sbc(v); 5 + p }

            // CMP
            0xC9 => { let v = self.imm(); self.cmp(self.a, v); 2 }
            0xC5 => { let a = self.zp(); let v = self.read(a); self.cmp(self.a, v); 3 }
            0xD5 => { let a = self.zpx(); let v = self.read(a); self.cmp(self.a, v); 4 }
            0xCD => { let a = self.abs(); let v = self.read(a); self.cmp(self.a, v); 4 }
            0xDD => { let (a, p) = self.abx(); let v = self.read(a); self.cmp(self.a, v); 4 + p }
            0xD9 => { let (a, p) = self.aby(); let v = self.read(a); self.cmp(self.a, v); 4 + p }
            0xC1 => { let a = self.inx(); let v = self.read(a); self.cmp(self.a, v); 6 }
            0xD1 => { let (a, p) = self.iny(); let v = self.read(a); self.cmp(self.a, v); 5 + p }

            // CPX
            0xE0 => { let v = self.imm(); self.cmp(self.x, v); 2 }
            0xE4 => { let a = self.zp(); let v = self.read(a); self.cmp(self.x, v); 3 }
            0xEC => { let a = self.abs(); let v = self.read(a); self.cmp(self.x, v); 4 }

            // CPY
            0xC0 => { let v = self.imm(); self.cmp(self.y, v); 2 }
            0xC4 => { let a = self.zp(); let v = self.read(a); self.cmp(self.y, v); 3 }
            0xCC => { let a = self.abs(); let v = self.read(a); self.cmp(self.y, v); 4 }

            // INC / DEC
            0xE6 => { let a = self.zp(); let o = self.read(a); self.write(a, o); let v = o.wrapping_add(1); self.write(a, v); self.set_zn(v); 5 }
            0xF6 => { let a = self.zpx(); let o = self.read(a); self.write(a, o); let v = o.wrapping_add(1); self.write(a, v); self.set_zn(v); 6 }
            0xEE => { let a = self.abs(); let o = self.read(a); self.write(a, o); let v = o.wrapping_add(1); self.write(a, v); self.set_zn(v); 6 }
            0xFE => { let (a, _) = self.abx(); let o = self.read(a); self.write(a, o); let v = o.wrapping_add(1); self.write(a, v); self.set_zn(v); 7 }

            0xC6 => { let a = self.zp(); let o = self.read(a); self.write(a, o); let v = o.wrapping_sub(1); self.write(a, v); self.set_zn(v); 5 }
            0xD6 => { let a = self.zpx(); let o = self.read(a); self.write(a, o); let v = o.wrapping_sub(1); self.write(a, v); self.set_zn(v); 6 }
            0xCE => { let a = self.abs(); let o = self.read(a); self.write(a, o); let v = o.wrapping_sub(1); self.write(a, v); self.set_zn(v); 6 }
            0xDE => { let (a, _) = self.abx(); let o = self.read(a); self.write(a, o); let v = o.wrapping_sub(1); self.write(a, v); self.set_zn(v); 7 }

            0xE8 => { self.x = self.x.wrapping_add(1); self.set_zn(self.x); 2 }
            0xCA => { self.x = self.x.wrapping_sub(1); self.set_zn(self.x); 2 }
            0xC8 => { self.y = self.y.wrapping_add(1); self.set_zn(self.y); 2 }
            0x88 => { self.y = self.y.wrapping_sub(1); self.set_zn(self.y); 2 }

            // Shifts & Rotates
            0x0A => { self.a = self.asl(self.a); 2 }
            0x06 => { let a = self.zp(); let v = self.read(a); self.write(a, v); let r = self.asl(v); self.write(a, r); 5 }
            0x16 => { let a = self.zpx(); let v = self.read(a); self.write(a, v); let r = self.asl(v); self.write(a, r); 6 }
            0x0E => { let a = self.abs(); let v = self.read(a); self.write(a, v); let r = self.asl(v); self.write(a, r); 6 }
            0x1E => { let (a, _) = self.abx(); let v = self.read(a); self.write(a, v); let r = self.asl(v); self.write(a, r); 7 }

            0x4A => { self.a = self.lsr(self.a); 2 }
            0x46 => { let a = self.zp(); let v = self.read(a); self.write(a, v); let r = self.lsr(v); self.write(a, r); 5 }
            0x56 => { let a = self.zpx(); let v = self.read(a); self.write(a, v); let r = self.lsr(v); self.write(a, r); 6 }
            0x4E => { let a = self.abs(); let v = self.read(a); self.write(a, v); let r = self.lsr(v); self.write(a, r); 6 }
            0x5E => { let (a, _) = self.abx(); let v = self.read(a); self.write(a, v); let r = self.lsr(v); self.write(a, r); 7 }

            0x2A => { self.a = self.rol(self.a); 2 }
            0x26 => { let a = self.zp(); let v = self.read(a); self.write(a, v); let r = self.rol(v); self.write(a, r); 5 }
            0x36 => { let a = self.zpx(); let v = self.read(a); self.write(a, v); let r = self.rol(v); self.write(a, r); 6 }
            0x2E => { let a = self.abs(); let v = self.read(a); self.write(a, v); let r = self.rol(v); self.write(a, r); 6 }
            0x3E => { let (a, _) = self.abx(); let v = self.read(a); self.write(a, v); let r = self.rol(v); self.write(a, r); 7 }

            0x6A => { self.a = self.ror(self.a); 2 }
            0x66 => { let a = self.zp(); let v = self.read(a); self.write(a, v); let r = self.ror(v); self.write(a, r); 5 }
            0x76 => { let a = self.zpx(); let v = self.read(a); self.write(a, v); let r = self.ror(v); self.write(a, r); 6 }
            0x6E => { let a = self.abs(); let v = self.read(a); self.write(a, v); let r = self.ror(v); self.write(a, r); 6 }
            0x7E => { let (a, _) = self.abx(); let v = self.read(a); self.write(a, v); let r = self.ror(v); self.write(a, r); 7 }

            // Jumps & Calls
            0x4C => {
                let target = self.abs();
                if target == instr_pc {
                    self.spin_loop_hits = self.spin_loop_hits.saturating_add(4);
                }
                self.pc = target;
                3
            }
            0x6C => {
                let ptr = self.abs();
                let lo = self.read(ptr) as u16;
                let hi_addr = (ptr & 0xFF00) | ((ptr.wrapping_add(1)) & 0x00FF);
                let hi = self.read(hi_addr) as u16;
                self.pc = (hi << 8) | lo;
                5
            }
            0x20 => {
                let target = self.abs();
                self.push_u16(self.pc.wrapping_sub(1));
                self.pc = target;
                self.spin_loop_hits = 0;
                6
            }
            0x60 => {
                self.pc = self.pop_u16().wrapping_add(1);
                6
            }
            0x40 => {
                self.status = (self.pop() & !FLAG_B) | FLAG_U;
                self.pc = self.pop_u16();
                self.in_nmi_handler = false;
                6
            }

            // Branches
            0x10 => self.branch(!self.get_flag(FLAG_N)),
            0x30 => self.branch(self.get_flag(FLAG_N)),
            0x50 => self.branch(!self.get_flag(FLAG_V)),
            0x70 => self.branch(self.get_flag(FLAG_V)),
            0x90 => self.branch(!self.get_flag(FLAG_C)),
            0xB0 => self.branch(self.get_flag(FLAG_C)),
            0xD0 => self.branch(!self.get_flag(FLAG_Z)),
            0xF0 => self.branch(self.get_flag(FLAG_Z)),

            // Flag control
            0x18 => { self.set_flag(FLAG_C, false); 2 }
            0x38 => { self.set_flag(FLAG_C, true); 2 }
            0x58 => { self.set_flag(FLAG_I, false); 2 }
            0x78 => { self.set_flag(FLAG_I, true); 2 }
            0xB8 => { self.set_flag(FLAG_V, false); 2 }
            0xD8 => { self.set_flag(FLAG_D, false); 2 }
            0xF8 => { self.set_flag(FLAG_D, true); 2 }

            // BRK
            0x00 => {
                self.pc = self.pc.wrapping_add(1);
                self.push_u16(self.pc);
                self.push(self.status | FLAG_B | FLAG_U);
                self.set_flag(FLAG_I, true);
                self.pc = self.read_u16(0xFFFE);
                7
            }

            // NOPs
            0xEA | 0x1A | 0x3A | 0x5A | 0x7A | 0xDA | 0xFA => 2,
            0x80 | 0x82 | 0x89 | 0xC2 | 0xE2 => {
                self.pc = self.pc.wrapping_add(1);
                2
            }
            0x04 | 0x44 | 0x64 => {
                self.pc = self.pc.wrapping_add(1);
                3
            }
            0x14 | 0x34 | 0x54 | 0x74 | 0xD4 | 0xF4 => {
                self.pc = self.pc.wrapping_add(1);
                4
            }
            0x0C => {
                self.pc = self.pc.wrapping_add(2);
                4
            }
            0x1C | 0x3C | 0x5C | 0x7C | 0xDC | 0xFC => {
                let (_, p) = self.abx();
                4 + p
            }

            // Unofficial opcodes: LAX, SAX
            0xA7 => { let a = self.zp(); let v = self.read(a); self.a = v; self.x = v; self.set_zn(v); 3 }
            0xB7 => { let a = self.zpy(); let v = self.read(a); self.a = v; self.x = v; self.set_zn(v); 4 }
            0xAF => { let a = self.abs(); let v = self.read(a); self.a = v; self.x = v; self.set_zn(v); 4 }
            0xBF => { let (a, p) = self.aby(); let v = self.read(a); self.a = v; self.x = v; self.set_zn(v); 4 + p }
            0xA3 => { let a = self.inx(); let v = self.read(a); self.a = v; self.x = v; self.set_zn(v); 6 }
            0xB3 => { let (a, p) = self.iny(); let v = self.read(a); self.a = v; self.x = v; self.set_zn(v); 5 + p }
            0x87 => { let a = self.zp(); self.write(a, self.a & self.x); 3 }
            0x97 => { let a = self.zpy(); self.write(a, self.a & self.x); 4 }
            0x8F => { let a = self.abs(); self.write(a, self.a & self.x); 4 }
            0x83 => { let a = self.inx(); self.write(a, self.a & self.x); 6 }

            // SLO / RLA / SRE / RRA / DCP / ISB (all addressing modes)
            0x03 | 0x07 | 0x0F | 0x13 | 0x17 | 0x1B | 0x1F
            | 0x23 | 0x27 | 0x2F | 0x33 | 0x37 | 0x3B | 0x3F
            | 0x43 | 0x47 | 0x4F | 0x53 | 0x57 | 0x5B | 0x5F
            | 0x63 | 0x67 | 0x6F | 0x73 | 0x77 | 0x7B | 0x7F
            | 0xC3 | 0xC7 | 0xCF | 0xD3 | 0xD7 | 0xDB | 0xDF
            | 0xE3 | 0xE7 | 0xEF | 0xF3 | 0xF7 | 0xFB | 0xFF => self.rmw_unofficial(op),

            // Immediate-mode unofficial opcodes
            0x0B | 0x2B => { let v = self.imm(); self.a &= v; self.set_zn(self.a); self.set_flag(FLAG_C, (self.a & 0x80) != 0); 2 } // ANC
            0x4B => { let v = self.imm(); self.a &= v; self.a = self.lsr(self.a); 2 } // ALR
            0x6B => { // ARR
                let v = self.imm();
                self.a &= v;
                let c = if self.get_flag(FLAG_C) { 0x80 } else { 0 };
                self.a = (self.a >> 1) | c;
                self.set_zn(self.a);
                self.set_flag(FLAG_C, (self.a & 0x40) != 0);
                self.set_flag(FLAG_V, (((self.a >> 6) ^ (self.a >> 5)) & 1) != 0);
                2
            }
            0xCB => { // AXS / SBX
                let v = self.imm();
                let ax = self.a & self.x;
                self.set_flag(FLAG_C, ax >= v);
                self.x = ax.wrapping_sub(v);
                self.set_zn(self.x);
                2
            }
            0xAB => { let v = self.imm(); self.a = v; self.x = v; self.set_zn(v); 2 } // LAX #imm (stable variant)

            // Unstable high-byte-AND stores
            0x9C => { let (a, _) = self.abx(); let hi = ((a.wrapping_sub(self.x as u16)) >> 8) as u8; self.write(a, self.y & hi.wrapping_add(1)); 5 } // SHY
            0x9E => { let (a, _) = self.aby(); let hi = ((a.wrapping_sub(self.y as u16)) >> 8) as u8; self.write(a, self.x & hi.wrapping_add(1)); 5 } // SHX
            0x9F => { let (a, _) = self.aby(); let hi = ((a.wrapping_sub(self.y as u16)) >> 8) as u8; self.write(a, self.a & self.x & hi.wrapping_add(1)); 5 } // SHA abs,Y
            0x93 => { let (a, _) = self.iny(); let hi = ((a.wrapping_sub(self.y as u16)) >> 8) as u8; self.write(a, self.a & self.x & hi.wrapping_add(1)); 6 } // SHA (zp),Y
            0x9B => { let (a, _) = self.aby(); self.sp = self.a & self.x; let hi = ((a.wrapping_sub(self.y as u16)) >> 8) as u8; self.write(a, self.sp & hi.wrapping_add(1)); 5 } // TAS
            0xBB => { let (a, p) = self.aby(); let v = self.read(a) & self.sp; self.a = v; self.x = v; self.sp = v; self.set_zn(v); 4 + p } // LAS

            // KIL / JAM: the 6502 locks up until reset.
            0x02 | 0x12 | 0x22 | 0x32 | 0x42 | 0x52 | 0x62 | 0x72 | 0x92 | 0xB2 | 0xD2 | 0xF2 => {
                self.pc = instr_pc;
                2
            }

            _ => 2,
        };

        self.access_cycle_offset = 0;
        let remaining = cycles.saturating_sub(self.cycles_pre_ticked);
        self.cycles_pre_ticked = 0;
        if remaining > 1 {
            self.tick_hardware(remaining - 1);
        }
        // Poll interrupt lines before the final cycle. CLI/SEI/PLP change I on their
        // last cycle, so the poll sees the *old* I flag (one-instruction delay).
        let i_poll = if matches!(op, 0x58 | 0x78 | 0x28) {
            i_flag_before
        } else {
            self.get_flag(FLAG_I)
        };
        self.nmi_latched = self.ppu.nmi_pending;
        self.irq_latched = !i_poll && self.irq_line();
        if remaining > 0 {
            self.tick_hardware(1);
        }
    }

    /// Combined level-triggered IRQ line from cartridge mapper and APU.
    #[inline]
    fn irq_line(&self) -> bool {
        self.mapper.irq_pending() || self.apu.irq_pending()
    }

    /// Unofficial read-modify-write combo opcodes: SLO, RLA, SRE, RRA, DCP, ISB.
    fn rmw_unofficial(&mut self, op: u8) -> u32 {
        let (addr, cyc) = match op & 0x1F {
            0x03 => (self.inx(), 8),
            0x07 => (self.zp(), 5),
            0x0F => (self.abs(), 6),
            0x13 => (self.iny().0, 8),
            0x17 => (self.zpx(), 6),
            0x1B => (self.aby().0, 7),
            _ => (self.abx().0, 7),
        };
        let v = self.read(addr);
        self.write(addr, v); // RMW dummy write of the unmodified value
        let r = match op >> 5 {
            0 => { let r = self.asl(v); self.a |= r; self.set_zn(self.a); r }
            1 => { let r = self.rol(v); self.a &= r; self.set_zn(self.a); r }
            2 => { let r = self.lsr(v); self.a ^= r; self.set_zn(self.a); r }
            3 => { let r = self.ror(v); self.adc(r); r }
            6 => { let r = v.wrapping_sub(1); self.cmp(self.a, r); r }
            _ => { let r = v.wrapping_add(1); self.sbc(r); r }
        };
        self.write(addr, r);
        cyc
    }

    fn nmi(&mut self) {
        self.in_nmi_handler = true;
        self.push_u16(self.pc);
        self.push((self.status & !FLAG_B) | FLAG_U);
        self.set_flag(FLAG_I, true);
        self.pc = self.read_u16(0xFFFA);
        self.tick_hardware(7);
    }

    fn irq(&mut self) {
        self.push_u16(self.pc);
        self.push((self.status & !FLAG_B) | FLAG_U);
        self.set_flag(FLAG_I, true);
        self.pc = self.read_u16(0xFFFE);
        self.tick_hardware(7);
    }

    #[inline]
    fn imm(&mut self) -> u8 {
        let v = self.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }

    #[inline]
    fn zp(&mut self) -> u16 {
        self.imm() as u16
    }

    #[inline]
    fn zpx(&mut self) -> u16 {
        self.imm().wrapping_add(self.x) as u16
    }

    #[inline]
    fn zpy(&mut self) -> u16 {
        self.imm().wrapping_add(self.y) as u16
    }

    #[inline]
    fn abs(&mut self) -> u16 {
        let a = self.read_u16(self.pc);
        self.pc = self.pc.wrapping_add(2);
        a
    }

    #[inline]
    fn abx(&mut self) -> (u16, u32) {
        let base = self.abs();
        let addr = base.wrapping_add(self.x as u16);
        let page_crossed = u32::from((base & 0xFF00) != (addr & 0xFF00));
        self.access_page_extra = page_crossed;
        (addr, page_crossed)
    }

    #[inline]
    fn aby(&mut self) -> (u16, u32) {
        let base = self.abs();
        let addr = base.wrapping_add(self.y as u16);
        let page_crossed = u32::from((base & 0xFF00) != (addr & 0xFF00));
        self.access_page_extra = page_crossed;
        (addr, page_crossed)
    }

    #[inline]
    fn inx(&mut self) -> u16 {
        let ptr = self.imm().wrapping_add(self.x);
        let lo = self.read(ptr as u16) as u16;
        let hi = self.read(ptr.wrapping_add(1) as u16) as u16;
        (hi << 8) | lo
    }

    #[inline]
    fn iny(&mut self) -> (u16, u32) {
        let ptr = self.imm();
        let lo = self.read(ptr as u16) as u16;
        let hi = self.read(ptr.wrapping_add(1) as u16) as u16;
        let base = (hi << 8) | lo;
        let addr = base.wrapping_add(self.y as u16);
        let page_crossed = u32::from((base & 0xFF00) != (addr & 0xFF00));
        self.access_page_extra = page_crossed;
        (addr, page_crossed)
    }

    #[inline]
    fn branch(&mut self, cond: bool) -> u32 {
        let offset = self.imm() as i8;
        if cond {
            let old_pc = self.pc;
            let target = self.pc.wrapping_add(offset as u16);
            if (-8..0).contains(&offset) {
                if target == self.last_branch_target {
                    self.spin_loop_hits = self.spin_loop_hits.saturating_add(1);
                } else {
                    self.last_branch_target = target;
                    self.spin_loop_hits = 1;
                }
            }
            self.pc = target;
            if (old_pc & 0xFF00) != (self.pc & 0xFF00) {
                4
            } else {
                3
            }
        } else {
            self.spin_loop_hits = 0;
            2
        }
    }

    #[inline]
    fn adc(&mut self, val: u8) {
        let c = if self.get_flag(FLAG_C) { 1u16 } else { 0u16 };
        let sum = (self.a as u16) + (val as u16) + c;
        let res = (sum & 0xFF) as u8;
        self.set_flag(FLAG_C, sum > 0xFF);
        self.set_flag(FLAG_V, ((!(self.a ^ val) & (self.a ^ res)) & 0x80) != 0);
        self.a = res;
        self.set_zn(self.a);
    }

    #[inline]
    fn sbc(&mut self, val: u8) {
        self.adc(val ^ 0xFF);
    }

    #[inline]
    fn cmp(&mut self, reg: u8, val: u8) {
        let diff = reg.wrapping_sub(val);
        self.set_flag(FLAG_C, reg >= val);
        self.set_zn(diff);
    }

    #[inline]
    fn asl(&mut self, val: u8) -> u8 {
        self.set_flag(FLAG_C, (val & 0x80) != 0);
        let res = val << 1;
        self.set_zn(res);
        res
    }

    #[inline]
    fn lsr(&mut self, val: u8) -> u8 {
        self.set_flag(FLAG_C, (val & 0x01) != 0);
        let res = val >> 1;
        self.set_zn(res);
        res
    }

    #[inline]
    fn rol(&mut self, val: u8) -> u8 {
        let c = if self.get_flag(FLAG_C) { 1 } else { 0 };
        self.set_flag(FLAG_C, (val & 0x80) != 0);
        let res = (val << 1) | c;
        self.set_zn(res);
        res
    }

    #[inline]
    fn ror(&mut self, val: u8) -> u8 {
        let c = if self.get_flag(FLAG_C) { 0x80 } else { 0 };
        self.set_flag(FLAG_C, (val & 0x01) != 0);
        let res = (val >> 1) | c;
        self.set_zn(res);
        res
    }
}
