//! Ricoh 2C02 PPU, emulated dot-by-dot: real background fetch pipeline and shift
//! registers, cycle-accurate sprite evaluation (including the overflow bug), sprite
//! pattern fetches on dots 257-320 (so mapper A12 IRQ counters see the real bus
//! pattern), delayed $2006/$2007 side effects, odd-frame dot skip, VBL/NMI races and
//! open-bus decay. Timing follows Mesen's model, driven from the CPU's master clock.

use crate::mappers::Mapper;
use crate::palette::NES_PALETTE;

pub const SCREEN_WIDTH: usize = 256;
pub const SCREEN_HEIGHT: usize = 240;

const NMI_SCANLINE: i32 = 241;
const VBLANK_END: i32 = 260;

#[derive(Clone, Copy, Default)]
struct TileInfo {
    tile_addr: u16,
    low: u8,
    high: u8,
    palette_offset: u8,
}

#[derive(Clone, Copy, Default)]
struct SpriteInfo {
    low: u8,
    high: u8,
    palette_offset: u8,
    behind_bg: bool,
    x: u8,
}

#[derive(Clone)]
pub struct Ppu {
    pub cycle: u16,
    pub scanline: i32,
    pub frame_count: u64,
    master_clock: u64,

    /// NMI output line (CPU edge-detects this).
    pub nmi_line: bool,
    /// Set when a full frame has been rendered (scanline 240 reached).
    pub frame_complete: bool,
    /// CPU cycle counter mirror for mapper A12 filtering.
    pub cpu_cycle: u64,

    // Registers.
    pub v: u16,
    pub t: u16,
    pub x: u8,
    pub w: bool,
    pub ctrl: u8,
    pub mask: u8,
    vbl: bool,
    sprite0_hit: bool,
    sprite_overflow: bool,
    pub oam_addr: u8,
    pub oam: [u8; 256],
    sec_oam: [u8; 32],
    pub palette: [u8; 32],
    read_buffer: u8,
    open_bus: u8,
    open_bus_stamp: [u64; 8],

    // Derived control/mask state.
    vram_inc32: bool,
    sprite_pt: u16,
    bg_pt: u16,
    large_sprites: bool,
    nmi_enabled: bool,
    bg_enabled: bool,
    sprites_enabled: bool,
    min_bg_cycle: u16,
    min_sprite_cycle: u16,
    palette_mask: u8,
    emphasis: u16,

    // Rendering pipeline.
    tile: TileInfo,
    low_shift: u16,
    high_shift: u16,
    cur_palette: u8,
    prev_palette: u8,
    sprites: [SpriteInfo; 8],
    sprite_count: u8,
    sprite_index: u8,
    sprite0_added: bool,
    sprite0_visible: bool,
    sprite_in_range: bool,
    sec_oam_addr: u8,
    oam_copy_buffer: u8,
    oam_copy_done: bool,
    overflow_bug_counter: u8,

    // Delayed state changes.
    rendering_enabled: bool,
    prev_rendering_enabled: bool,
    need_state_update: bool,
    update_vram_delay: u8,
    update_vram_value: u16,
    ignore_vram_read: u8,
    read_sm: u8,
    write_sm: u8,
    write_latch: u8,
    need_vram_increment: bool,
    bus_address: u16,
    prevent_vbl: bool,

    /// Extra idle scanlines inserted before VBlank (CPU overclock); applied at frame start.
    pub extra_scanlines: u16,
    active_extra: u16,
    /// False while the PPU is in overclock-only scanlines (APU is paused).
    pub apu_enabled: bool,

    /// Palette indices (6 bits) | emphasis (3 bits << 6) for the frame being drawn.
    pixels: Box<[u16]>,
    /// RGBA output of the last completed frame.
    pub framebuffer: Box<[u8]>,
}

impl Default for Ppu {
    fn default() -> Self {
        Self::new()
    }
}

impl Ppu {
    pub fn new() -> Self {
        let mut fb = vec![0u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4].into_boxed_slice();
        for px in fb.chunks_exact_mut(4) {
            px[3] = 255;
        }
        let mut p = Self {
            cycle: 340,
            scanline: -1,
            frame_count: 1,
            master_clock: 0,
            nmi_line: false,
            frame_complete: false,
            cpu_cycle: 0,
            v: 0,
            t: 0,
            x: 0,
            w: false,
            ctrl: 0,
            mask: 0,
            vbl: false,
            sprite0_hit: false,
            sprite_overflow: false,
            oam_addr: 0,
            oam: [0; 256],
            sec_oam: [0; 32],
            palette: [
                0x09, 0x01, 0x00, 0x01, 0x00, 0x02, 0x02, 0x0D, 0x08, 0x10, 0x08, 0x24, 0x00, 0x00, 0x04, 0x2C, 0x09, 0x01, 0x34, 0x03,
                0x00, 0x04, 0x00, 0x14, 0x08, 0x3A, 0x00, 0x02, 0x00, 0x20, 0x2C, 0x08,
            ],
            read_buffer: 0,
            open_bus: 0,
            open_bus_stamp: [0; 8],
            vram_inc32: false,
            sprite_pt: 0,
            bg_pt: 0,
            large_sprites: false,
            nmi_enabled: false,
            bg_enabled: false,
            sprites_enabled: false,
            min_bg_cycle: 300,
            min_sprite_cycle: 300,
            palette_mask: 0x3F,
            emphasis: 0,
            tile: TileInfo::default(),
            low_shift: 0,
            high_shift: 0,
            cur_palette: 0,
            prev_palette: 0,
            sprites: [SpriteInfo::default(); 8],
            sprite_count: 0,
            sprite_index: 0,
            sprite0_added: false,
            sprite0_visible: false,
            sprite_in_range: false,
            sec_oam_addr: 0,
            oam_copy_buffer: 0,
            oam_copy_done: false,
            overflow_bug_counter: 0,
            rendering_enabled: false,
            prev_rendering_enabled: false,
            need_state_update: false,
            update_vram_delay: 0,
            update_vram_value: 0,
            ignore_vram_read: 0,
            read_sm: 0,
            write_sm: 0,
            write_latch: 0,
            need_vram_increment: false,
            bus_address: 0,
            prevent_vbl: false,
            extra_scanlines: 0,
            active_extra: 0,
            apu_enabled: true,
            pixels: vec![0u16; SCREEN_WIDTH * SCREEN_HEIGHT].into_boxed_slice(),
            framebuffer: fb,
        };
        p.update_min_draw_cycles();
        p
    }

    /// Soft reset: clears control/mask/scroll latches (VBL flag and OAM are kept).
    pub fn reset(&mut self) {
        self.write_ctrl(0);
        self.write_mask(0);
        self.w = false;
        self.t = 0;
        self.x = 0;
        self.read_buffer = 0;
        self.nmi_line = false;
    }

    #[inline]
    pub fn rendering_enabled(&self) -> bool {
        self.rendering_enabled
    }

    #[inline]
    fn nmi_scanline(&self) -> i32 {
        NMI_SCANLINE + self.active_extra as i32
    }

    #[inline]
    fn vblank_end(&self) -> i32 {
        VBLANK_END + self.active_extra as i32
    }

    /// Run the PPU up to the given master clock (4 master clocks per dot).
    #[inline]
    pub fn run(&mut self, run_to: u64, m: &mut dyn Mapper) {
        loop {
            self.exec(m);
            self.master_clock += 4;
            if self.master_clock + 4 > run_to {
                break;
            }
        }
    }

    // ---------------------------------------------------------------- open bus

    fn set_open_bus(&mut self, mask: u8, value: u8) {
        if mask == 0xFF {
            self.open_bus = value;
            self.open_bus_stamp = [self.frame_count; 8];
            return;
        }
        let mut ob = (self.open_bus as u16) << 8;
        let (mut m, mut v) = (mask, value);
        for i in 0..8 {
            ob >>= 1;
            if m & 1 != 0 {
                if v & 1 != 0 {
                    ob |= 0x80;
                } else {
                    ob &= 0xFF7F;
                }
                self.open_bus_stamp[i] = self.frame_count;
            } else if self.frame_count - self.open_bus_stamp[i] > 3 {
                ob &= 0xFF7F;
            }
            v >>= 1;
            m >>= 1;
        }
        self.open_bus = ob as u8;
    }

    #[inline]
    fn apply_open_bus(&mut self, mask: u8, value: u8) -> u8 {
        self.set_open_bus(!mask, value);
        value | (self.open_bus & mask)
    }

    // ---------------------------------------------------------------- CPU interface

    pub fn read_register(&mut self, addr: u16, m: &mut dyn Mapper) -> u8 {
        let _ = m;
        let mut ob_mask = 0xFFu8;
        let mut ret = 0u8;
        match addr & 7 {
            2 => {
                self.w = false;
                ret = ((self.sprite_overflow as u8) << 5) | ((self.sprite0_hit as u8) << 6) | ((self.vbl as u8) << 7);
                self.vbl = false;
                self.nmi_line = false;
                if self.scanline == self.nmi_scanline() && self.cycle == 0 {
                    self.prevent_vbl = true;
                }
                ob_mask = 0x1F;
            }
            4 => {
                if self.scanline <= 239 && self.rendering_enabled {
                    if (257..=320).contains(&self.cycle) || self.cycle == 0 {
                        self.oam_copy_buffer = self.sec_oam[(self.sec_oam_addr & 0x1F) as usize];
                    }
                    ret = self.oam_copy_buffer;
                } else {
                    ret = self.oam[self.oam_addr as usize];
                }
                ob_mask = 0;
            }
            7 => {
                if self.ignore_vram_read == 0 {
                    ret = self.read_buffer;
                    self.read_sm = 5;
                    if (self.bus_address & 0x3FFF) >= 0x3F00 {
                        ret = (self.read_palette(self.bus_address) & self.palette_mask) | (self.open_bus & 0xC0);
                        ob_mask = 0xC0;
                    } else {
                        ob_mask = 0;
                    }
                    self.ignore_vram_read = 6;
                    self.need_state_update = true;
                }
            }
            _ => {}
        }
        self.apply_open_bus(ob_mask, ret)
    }

    pub fn write_register(&mut self, addr: u16, val: u8, m: &mut dyn Mapper) {
        self.set_open_bus(0xFF, val);
        match addr & 7 {
            0 => self.write_ctrl(val),
            1 => self.write_mask(val),
            3 => self.oam_addr = val,
            4 => {
                if self.scanline >= 240 || !self.rendering_enabled {
                    let v = if self.oam_addr & 3 == 2 { val & 0xE3 } else { val };
                    self.oam[self.oam_addr as usize] = v;
                    self.oam_addr = self.oam_addr.wrapping_add(1);
                } else {
                    // Writes during rendering only bump the high 6 bits of OAMADDR.
                    self.oam_addr = self.oam_addr.wrapping_add(4) & 0xFC;
                }
            }
            5 => {
                if self.w {
                    self.t = (self.t & !0x73E0) | (((val as u16) & 0xF8) << 2) | (((val as u16) & 0x07) << 12);
                } else {
                    self.x = val & 7;
                    self.t = (self.t & !0x001F) | ((val as u16) >> 3);
                }
                self.w = !self.w;
            }
            6 => {
                if self.w {
                    self.t = (self.t & !0x00FF) | val as u16;
                    self.need_state_update = true;
                    self.update_vram_delay = 3;
                    self.update_vram_value = self.t;
                } else {
                    self.t = (self.t & !0xFF00) | (((val as u16) & 0x3F) << 8);
                }
                self.w = !self.w;
            }
            7 => {
                self.write_sm = 5;
                self.write_latch = val;
                self.need_state_update = true;
            }
            _ => {}
        }
        let _ = m;
    }

    fn write_ctrl(&mut self, val: u8) {
        self.ctrl = val;
        self.t = (self.t & !0x0C00) | (((val as u16) & 3) << 10);
        self.vram_inc32 = val & 0x04 != 0;
        self.sprite_pt = if val & 0x08 != 0 { 0x1000 } else { 0 };
        self.bg_pt = if val & 0x10 != 0 { 0x1000 } else { 0 };
        self.large_sprites = val & 0x20 != 0;
        self.nmi_enabled = val & 0x80 != 0;
        if !self.nmi_enabled {
            self.nmi_line = false;
        } else if self.vbl {
            self.nmi_line = true;
        }
    }

    fn write_mask(&mut self, val: u8) {
        self.mask = val;
        self.bg_enabled = val & 0x08 != 0;
        self.sprites_enabled = val & 0x10 != 0;
        self.palette_mask = if val & 0x01 != 0 { 0x30 } else { 0x3F };
        self.emphasis = ((val as u16) >> 5) << 6;
        if self.rendering_enabled != (self.bg_enabled || self.sprites_enabled) {
            self.need_state_update = true;
        }
        self.update_min_draw_cycles();
    }

    fn update_min_draw_cycles(&mut self) {
        self.min_bg_cycle = if self.bg_enabled { if self.mask & 0x02 != 0 { 0 } else { 8 } } else { 300 };
        self.min_sprite_cycle = if self.sprites_enabled { if self.mask & 0x04 != 0 { 0 } else { 8 } } else { 300 };
    }

    // ---------------------------------------------------------------- VRAM access

    #[inline]
    fn read_palette(&self, addr: u16) -> u8 {
        let mut a = (addr & 0x1F) as usize;
        if a & 0x13 == 0x10 {
            a &= !0x10;
        }
        self.palette[a]
    }

    fn write_palette(&mut self, addr: u16, val: u8) {
        let a = (addr & 0x1F) as usize;
        let v = val & 0x3F;
        if a & 0x03 == 0 {
            self.palette[a & 0x0F] = v;
            self.palette[(a & 0x0F) | 0x10] = v;
        } else {
            self.palette[a] = v;
        }
    }

    #[inline]
    fn set_bus_address(&mut self, addr: u16, m: &mut dyn Mapper) {
        self.bus_address = addr;
        if m.wants_vram_addr() {
            m.notify_vram_addr(addr, self.cpu_cycle);
        }
    }

    #[inline]
    fn read_vram(&mut self, addr: u16, m: &mut dyn Mapper) -> u8 {
        self.set_bus_address(addr, m);
        m.ppu_read(addr & 0x3FFF)
    }

    #[inline]
    fn nt_addr(&self) -> u16 {
        0x2000 | (self.v & 0x0FFF)
    }

    #[inline]
    fn attr_addr(&self) -> u16 {
        0x23C0 | (self.v & 0x0C00) | ((self.v >> 4) & 0x38) | ((self.v >> 2) & 0x07)
    }

    #[inline]
    fn inc_h(&mut self) {
        if (self.v & 0x001F) == 31 {
            self.v = (self.v & !0x001F) ^ 0x0400;
        } else {
            self.v += 1;
        }
    }

    #[inline]
    fn inc_v(&mut self) {
        let mut a = self.v;
        if (a & 0x7000) != 0x7000 {
            a += 0x1000;
        } else {
            a &= !0x7000;
            let mut y = (a & 0x03E0) >> 5;
            if y == 29 {
                y = 0;
                a ^= 0x0800;
            } else if y == 31 {
                y = 0;
            } else {
                y += 1;
            }
            a = (a & !0x03E0) | (y << 5);
        }
        self.v = a;
    }

    fn update_vram_addr(&mut self, m: &mut dyn Mapper) {
        if self.scanline >= 240 || !self.rendering_enabled {
            self.v = (self.v + if self.vram_inc32 { 32 } else { 1 }) & 0x7FFF;
            self.set_bus_address(self.v & 0x3FFF, m);
        } else {
            self.inc_h();
            self.inc_v();
        }
    }

    // ---------------------------------------------------------------- main loop

    #[inline]
    fn exec(&mut self, m: &mut dyn Mapper) {
        if self.cycle < 340 {
            self.cycle += 1;
            if self.scanline < 240 {
                self.process_scanline(m);
            } else if self.cycle == 1 && self.scanline == self.nmi_scanline() {
                if !self.prevent_vbl {
                    self.vbl = true;
                    if self.nmi_enabled {
                        self.nmi_line = true;
                    }
                }
                self.prevent_vbl = false;
            }
        } else {
            self.process_first_cycle(m);
        }
        if self.need_state_update {
            self.update_state(m);
        }
    }

    fn process_first_cycle(&mut self, m: &mut dyn Mapper) {
        self.cycle = 0;
        self.scanline += 1;
        if self.scanline > self.vblank_end() {
            self.scanline = -1;
            self.sprite_count = 0;
            self.active_extra = self.extra_scanlines;
            self.update_min_draw_cycles();
        }
        self.apu_enabled = !(self.scanline > 240 && self.scanline < self.nmi_scanline());

        if self.scanline < 240 {
            if self.scanline == -1 {
                self.sprite_overflow = false;
                self.sprite0_hit = false;
            } else if self.prev_rendering_enabled {
                self.sec_oam_addr = 0;
                if self.scanline > 0 || self.frame_count & 1 == 0 {
                    let a = (self.tile.tile_addr << 4) | (self.v >> 12) | self.bg_pt;
                    self.set_bus_address(a, m);
                }
            }
        } else if self.scanline == 240 {
            self.set_bus_address(self.v & 0x3FFF, m);
            self.send_frame();
            self.frame_count += 1;
        }
    }

    fn send_frame(&mut self) {
        for (i, &p) in self.pixels.iter().enumerate() {
            let rgb = crate::palette::apply_emphasis(NES_PALETTE[(p & 0x3F) as usize], (p >> 6) as u8);
            let o = i * 4;
            self.framebuffer[o] = rgb[0];
            self.framebuffer[o + 1] = rgb[1];
            self.framebuffer[o + 2] = rgb[2];
        }
        self.frame_complete = true;
    }

    fn process_scanline(&mut self, m: &mut dyn Mapper) {
        let cycle = self.cycle;
        if cycle <= 256 {
            if self.prev_rendering_enabled {
                if self.scanline >= 0 {
                    self.draw_pixel();
                    self.process_sprite_evaluation();
                    self.shift_tile_registers();
                } else if cycle == 1 {
                    self.vbl = false;
                    self.nmi_line = false;
                }
                if cycle & 7 == 0 {
                    self.inc_h();
                    if cycle == 256 {
                        self.inc_v();
                    }
                }
                self.load_tile_info(m);
            } else if self.scanline >= 0 {
                self.draw_pixel();
            } else if cycle == 1 {
                self.vbl = false;
                self.nmi_line = false;
            }
        } else if cycle <= 320 {
            if cycle == 257 {
                self.sprite_index = 0;
                self.sprite_count = 0;
                if self.prev_rendering_enabled {
                    self.v = (self.v & !0x041F) | (self.t & 0x041F);
                }
            }
            if self.prev_rendering_enabled {
                self.sprite0_visible = self.sprite0_added;
                self.oam_addr = 0;
                match (cycle - 257) % 8 {
                    0 | 2 => {
                        let a = self.nt_addr();
                        self.read_vram(a, m);
                    }
                    4 => self.load_sprite_tile_info(m),
                    _ => {}
                }
                if self.scanline == -1 && (280..=304).contains(&cycle) {
                    self.v = (self.v & !0x7BE0) | (self.t & 0x7BE0);
                }
            }
        } else if cycle <= 336 {
            if self.prev_rendering_enabled {
                if cycle == 328 || cycle == 336 {
                    self.inc_h();
                }
                self.shift_tile_registers();
                self.load_tile_info(m);
            }
        } else if cycle == 337 || cycle == 339 {
            if self.rendering_enabled {
                let a = self.nt_addr();
                self.tile.tile_addr = self.read_vram(a, m) as u16;
                if cycle == 339 && self.scanline == -1 && self.frame_count & 1 == 1 {
                    // Odd frames skip the last dot of the pre-render line.
                    self.cycle = 340;
                }
            }
        }
    }

    #[inline]
    fn shift_tile_registers(&mut self) {
        self.low_shift <<= 1;
        self.high_shift = (self.high_shift << 1) | 1;
    }

    fn load_tile_info(&mut self, m: &mut dyn Mapper) {
        match self.cycle & 7 {
            0 => {
                self.high_shift = (self.high_shift & 0xFF00) | self.tile.high as u16;
                self.low_shift = (self.low_shift & 0xFF00) | self.tile.low as u16;
                self.prev_palette = self.cur_palette;
                self.cur_palette = self.tile.palette_offset;
            }
            1 => {
                let a = self.nt_addr();
                let idx = self.read_vram(a, m) as u16;
                self.tile.tile_addr = (idx << 4) | (self.v >> 12) | self.bg_pt;
            }
            3 => {
                let shift = ((self.v >> 4) & 0x04) | (self.v & 0x02);
                let a = self.attr_addr();
                self.tile.palette_offset = ((self.read_vram(a, m) >> shift) & 3) << 2;
            }
            5 => {
                let a = self.tile.tile_addr;
                self.tile.low = self.read_vram(a, m);
            }
            7 => {
                let a = self.tile.tile_addr + 8;
                self.tile.high = self.read_vram(a, m);
            }
            _ => {}
        }
    }

    fn load_sprite_tile_info(&mut self, m: &mut dyn Mapper) {
        self.sprite_index = ((self.cycle - 257) >> 3) as u8;
        let i = self.sprite_index as usize * 4;
        let (y, tile, attr, x) = (self.sec_oam[i], self.sec_oam[i + 1], self.sec_oam[i + 2], self.sec_oam[i + 3]);
        self.load_sprite(y, tile, attr, x, m);
    }

    fn load_sprite(&mut self, y: u8, tile: u8, attr: u8, x: u8, m: &mut dyn Mapper) {
        let flip_h = attr & 0x40 != 0;
        let flip_v = attr & 0x80 != 0;
        let size_mask: u16 = if self.large_sprites { 15 } else { 7 };
        let line8 = if self.scanline >= 0 { self.scanline as u8 } else { 261u16 as u8 };
        let mut range = (line8 as i32 - y as i32) as u16;
        if flip_v {
            range ^= size_mask;
        }
        let tile = tile as u16;
        let addr = if self.large_sprites {
            (((tile & 1) << 12) | ((tile & !1) << 4)) + ((range & 8) << 1) + (range & 7)
        } else {
            (self.sprite_pt | (tile << 4)) + (range & 7)
        };
        let mut low = self.read_vram(addr, m);
        let mut high = self.read_vram(addr + 8, m);
        let in_range = range <= size_mask && self.scanline >= 0;
        if in_range {
            if flip_h {
                low = low.reverse_bits();
                high = high.reverse_bits();
            }
            let slot = &mut self.sprites[self.sprite_count as usize];
            slot.low = low;
            slot.high = high;
            slot.palette_offset = ((attr & 3) << 2) | 0x10;
            slot.behind_bg = attr & 0x20 != 0;
            slot.x = x;
            self.sprite_count += 1;
        }
        self.sprite_index += 1;
    }

    #[inline]
    fn get_pixel_color(&mut self) -> u8 {
        let cycle = self.cycle;
        let offset = self.x;
        let mut bg = 0u8;
        if cycle > self.min_bg_cycle {
            bg = ((((self.low_shift << offset) & 0x8000) >> 15) | (((self.high_shift << offset) & 0x8000) >> 14)) as u8;
        }
        if self.sprite_count > 0 && cycle > self.min_sprite_cycle {
            for i in 0..self.sprite_count as usize {
                let s = self.sprites[i];
                let shift = cycle as i32 - s.x as i32 - 1;
                if (0..8).contains(&shift) {
                    let color = ((s.low << shift) & 0x80) >> 7 | ((s.high << shift) & 0x80) >> 6;
                    if color != 0 {
                        if i == 0
                            && self.sprite0_visible
                            && bg != 0
                            && cycle != 256
                            && self.bg_enabled
                            && !self.sprite0_hit
                        {
                            self.sprite0_hit = true;
                        }
                        if bg == 0 || !s.behind_bg {
                            return s.palette_offset + color;
                        }
                        break;
                    }
                }
            }
        }
        let pal = if offset as u16 + ((cycle - 1) & 7) < 8 { self.prev_palette } else { self.cur_palette };
        pal + bg
    }

    #[inline]
    fn draw_pixel(&mut self) {
        let idx = ((self.scanline as usize) << 8) + self.cycle as usize - 1;
        let color = if self.rendering_enabled || (self.v & 0x3F00) != 0x3F00 {
            let c = self.get_pixel_color();
            self.palette[if c & 3 != 0 { c as usize } else { 0 }]
        } else {
            self.read_palette(self.v)
        };
        self.pixels[idx] = ((color & self.palette_mask) as u16) | self.emphasis;
    }

    fn process_sprite_evaluation(&mut self) {
        let cycle = self.cycle;
        if cycle < 65 {
            self.oam_copy_buffer = 0xFF;
            self.sec_oam[(self.sec_oam_addr & 0x1F) as usize] = 0xFF;
            if cycle & 1 == 0 {
                self.sec_oam_addr = self.sec_oam_addr.wrapping_add(1);
            }
            return;
        }
        if cycle & 1 == 1 {
            if cycle == 65 {
                self.sprite0_added = false;
                self.sprite_in_range = false;
                self.sec_oam_addr = 0;
                self.overflow_bug_counter = 0;
                self.oam_copy_done = false;
            }
            self.oam_copy_buffer = self.oam[self.oam_addr as usize];
            return;
        }
        let line = self.scanline;
        let height = if self.large_sprites { 16 } else { 8 };
        let buf = self.oam_copy_buffer as i32;
        let mut h = self.oam_addr >> 2;
        let mut l = self.oam_addr & 3;
        if self.oam_copy_done {
            h = (h + 1) & 0x3F;
            self.oam_copy_buffer = self.sec_oam[(self.sec_oam_addr & 0x1F) as usize];
        } else {
            if !self.sprite_in_range && line >= buf && line < buf + height {
                self.sprite_in_range = true;
            }
            if self.sec_oam_addr < 0x20 {
                self.sec_oam[self.sec_oam_addr as usize] = self.oam_copy_buffer;
                if self.sprite_in_range {
                    if cycle == 66 {
                        self.sprite0_added = true;
                    }
                    l += 1;
                    self.sec_oam_addr += 1;
                    if l >= 4 {
                        h = (h + 1) & 0x3F;
                        l = 0;
                        if h == 0 {
                            self.oam_copy_done = true;
                        }
                    }
                    if self.sec_oam_addr & 3 == 0 {
                        self.sprite_in_range = false;
                        if l != 0 {
                            let in_range = line >= buf && line < buf + height;
                            if !in_range {
                                l = 0;
                            }
                        }
                    }
                } else {
                    h = (h + 1) & 0x3F;
                    l = 0;
                    if h == 0 {
                        self.oam_copy_done = true;
                    }
                }
            } else {
                self.oam_copy_buffer = self.sec_oam[(self.sec_oam_addr & 0x1F) as usize];
                if self.sprite_in_range {
                    self.sprite_overflow = true;
                    l += 1;
                    if l == 4 {
                        h = (h + 1) & 0x3F;
                        l = 0;
                    }
                    if self.overflow_bug_counter == 0 {
                        self.overflow_bug_counter = 3;
                    } else {
                        self.overflow_bug_counter -= 1;
                        if self.overflow_bug_counter == 0 {
                            self.oam_copy_done = true;
                            l = 0;
                        }
                    }
                } else {
                    // Hardware bug: both the sprite and byte index increment.
                    h = (h + 1) & 0x3F;
                    l = (l + 1) & 3;
                    if h == 0 {
                        self.oam_copy_done = true;
                    }
                }
            }
        }
        self.oam_addr = (l & 3) | (h << 2);
    }

    fn update_state(&mut self, m: &mut dyn Mapper) {
        self.need_state_update = false;

        if self.prev_rendering_enabled != self.rendering_enabled {
            self.prev_rendering_enabled = self.rendering_enabled;
            if self.scanline < 240 && !self.prev_rendering_enabled {
                let a = self.v & 0x3FFF;
                self.set_bus_address(a, m);
                if (65..=256).contains(&self.cycle) {
                    self.oam_addr = self.oam_addr.wrapping_add(1);
                }
            }
        }

        let want = self.bg_enabled || self.sprites_enabled;
        if self.rendering_enabled != want {
            self.rendering_enabled = want;
            self.need_state_update = true;
        }

        if self.update_vram_delay > 0 {
            self.update_vram_delay -= 1;
            if self.update_vram_delay == 0 {
                if self.scanline < 240 && self.rendering_enabled {
                    // $2006 landing on a scroll increment corrupts v (AND glitch).
                    if self.cycle == 257 {
                        self.v &= self.update_vram_value;
                    } else if self.cycle > 0 && self.cycle & 7 == 0 && (self.cycle <= 256 || self.cycle > 320) {
                        self.v = (self.update_vram_value & !0x41F) | (self.v & self.update_vram_value & 0x41F);
                    } else {
                        self.v = self.update_vram_value;
                    }
                } else {
                    self.v = self.update_vram_value;
                }
                self.t = self.v;
                if self.scanline >= 240 || !self.rendering_enabled {
                    let a = self.v & 0x3FFF;
                    self.set_bus_address(a, m);
                }
            } else {
                self.need_state_update = true;
            }
        }

        if self.ignore_vram_read > 0 {
            self.ignore_vram_read -= 1;
            if self.ignore_vram_read > 0 {
                self.need_state_update = true;
            }
        }

        if self.need_vram_increment {
            self.need_vram_increment = false;
            self.update_vram_addr(m);
        }

        if self.read_sm > 0 {
            self.read_sm -= 1;
            if self.read_sm == 0 {
                let a = self.bus_address & 0x3FFF;
                self.read_buffer = m.ppu_read(a);
                self.need_vram_increment = true;
            }
            self.need_state_update = true;
        }

        if self.write_sm > 0 {
            self.write_sm -= 1;
            if self.write_sm == 0 {
                let a = self.bus_address & 0x3FFF;
                if a >= 0x3F00 {
                    self.write_palette(a, self.write_latch);
                } else if self.scanline >= 240 || !self.rendering_enabled {
                    m.ppu_write(a, self.write_latch);
                } else {
                    m.ppu_write(a, (a & 0xFF) as u8);
                }
                self.need_vram_increment = true;
            }
            self.need_state_update = true;
        }
    }
}
