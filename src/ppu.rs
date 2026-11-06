//! Ricoh 2C02 Picture Processing Unit (PPU) scanline renderer with Loopy scroll,
//! Sprite 0 Hit, CHR-RAM write support, and MMC3 scanline IRQ clocking.

use crate::cartridge::Mirroring;
use crate::mapper::Mapper;
use crate::palette::NES_PALETTE;

pub const SCREEN_WIDTH: usize = 256;
pub const SCREEN_HEIGHT: usize = 240;

#[derive(Clone)]
pub struct Ppu {
    pub ctrl: u8,
    pub mask: u8,
    pub status: u8,
    pub oam_addr: u8,
    pub oam: [u8; 256],
    /// 2 KB console CIRAM + 2 KB for four-screen cartridges.
    pub vram: [u8; 4096],
    pub palette: [u8; 32],

    pub v: u16,
    pub t: u16,
    pub fine_x: u8,
    pub write_latch: bool,
    pub data_buffer: u8,

    pub scanline: u16,
    pub dot: u16,
    pub nmi_pending: bool,
    pub frame_complete: bool,
    pub render_enable_delay: u8,
    /// 2C02 delays the t->v copy after the second $2006 write by ~3 PPU dots.
    pub v_update_delay: u8,
    /// Toggles every frame; odd frames skip prerender dot 339 when rendering is on.
    pub odd_frame: bool,
    /// Set when a $2002 read races the VBlank flag; suppresses this frame's flag/NMI.
    pub suppress_vblank: bool,
    /// PPU I/O data bus latch ("open bus") returned by write-only registers and
    /// the low 5 bits of $2002.
    pub io_latch: u8,

    pub framebuffer: [u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4],
}

impl Default for Ppu {
    fn default() -> Self {
        Self::new()
    }
}

impl Ppu {
    pub fn new() -> Self {
        let mut fb = [0u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4];
        for px in fb.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
        Self {
            ctrl: 0,
            mask: 0,
            status: 0,
            oam_addr: 0,
            oam: [0; 256],
            vram: [0; 4096],
            palette: [0x0F; 32],
            v: 0,
            t: 0,
            fine_x: 0,
            write_latch: false,
            data_buffer: 0,
            scanline: 0,
            dot: 0,
            nmi_pending: false,
            frame_complete: false,
            render_enable_delay: 0,
            v_update_delay: 0,
            odd_frame: false,
            suppress_vblank: false,
            io_latch: 0,
            framebuffer: fb,
        }
    }

    #[inline]
    pub fn rendering_enabled(&self) -> bool {
        (self.mask & 0x18) != 0 && self.render_enable_delay == 0
    }

    #[inline]
    fn mirror_vram_addr(&self, addr: u16, mirroring: Mirroring) -> usize {
        let a = (addr - 0x2000) & 0x0FFF;
        let table = a / 0x0400;
        let offset = a % 0x0400;
        let mapped_table = match mirroring {
            Mirroring::OneScreenLower => 0,
            Mirroring::OneScreenUpper => 1,
            Mirroring::Vertical => table & 1,
            Mirroring::Horizontal => (table >> 1) & 1,
            Mirroring::FourScreen => table,
            Mirroring::Custom(pages) => (pages[table as usize] & 1) as u16,
        };
        ((mapped_table * 0x0400) + offset) as usize
    }

    #[inline]
    fn mirror_palette_addr(addr: u16) -> usize {
        let mut a = (addr & 0x1F) as usize;
        if a >= 16 && (a & 0x03) == 0 {
            a -= 16;
        }
        a
    }

    pub fn ppu_read(&self, addr: u16, mapper: &Mapper) -> u8 {
        let a = addr & 0x3FFF;
        match a {
            0x0000..=0x1FFF => mapper.read_chr(a),
            0x2000..=0x3EFF => {
                let idx = self.mirror_vram_addr(a, mapper.mirroring);
                self.vram[idx]
            }
            0x3F00..=0x3FFF => {
                let idx = Self::mirror_palette_addr(a);
                self.palette[idx]
            }
            _ => 0,
        }
    }

    pub fn ppu_write(&mut self, addr: u16, val: u8, mapper: &mut Mapper) {
        let a = addr & 0x3FFF;
        match a {
            0x0000..=0x1FFF => {
                mapper.write_chr(a, val);
            }
            0x2000..=0x3EFF => {
                let idx = self.mirror_vram_addr(a, mapper.mirroring);
                self.vram[idx] = val;
            }
            0x3F00..=0x3FFF => {
                let idx = Self::mirror_palette_addr(a);
                self.palette[idx] = val & 0x3F;
            }
            _ => {}
        }
    }

    pub fn cpu_read(&mut self, addr: u16, mapper: &Mapper) -> u8 {
        match addr & 0x0007 {
            2 => {
                // VBlank race: dot 1 of scanline 241 hasn't been processed yet when
                // `dot == 1`, so the read lands one dot before the flag sets; the flag
                // and NMI are both suppressed. A read on dots 2-3 sees the flag but
                // still cancels the NMI.
                if self.scanline == 241 {
                    if self.dot == 1 {
                        self.suppress_vblank = true;
                    } else if self.dot <= 3 {
                        self.nmi_pending = false;
                    }
                }
                let res = (self.status & 0xE0) | (self.io_latch & 0x1F);
                self.status &= !0x80;
                self.write_latch = false;
                self.io_latch = res;
                res
            }
            4 => {
                let v = self.oam[self.oam_addr as usize];
                self.io_latch = v;
                v
            }
            7 => {
                let a = self.v & 0x3FFF;
                let val = if a >= 0x3F00 {
                    self.data_buffer =
                        self.vram[self.mirror_vram_addr(a - 0x1000, mapper.mirroring)];
                    (self.ppu_read(a, mapper) & 0x3F) | (self.io_latch & 0xC0)
                } else {
                    let prev = self.data_buffer;
                    self.data_buffer = self.ppu_read(a, mapper);
                    prev
                };
                self.increment_vram_addr();
                self.io_latch = val;
                val
            }
            _ => self.io_latch,
        }
    }

    pub fn cpu_write(&mut self, addr: u16, val: u8, mapper: &mut Mapper) {
        self.io_latch = val;
        match addr & 0x0007 {
            0 => {
                let prev_nmi = (self.ctrl & 0x80) != 0;
                self.ctrl = val;
                self.t = (self.t & 0xF3FF) | (((val as u16) & 0x03) << 10);
                let new_nmi = (self.ctrl & 0x80) != 0;
                if !prev_nmi && new_nmi && (self.status & 0x80) != 0 {
                    self.nmi_pending = true;
                } else if !new_nmi {
                    // Clearing bit 7 drops the NMI line before the CPU samples it.
                    self.nmi_pending = false;
                }
            }
            1 => {
                let was_enabled = (self.mask & 0x18) != 0;
                self.mask = val;
                let now_enabled = (self.mask & 0x18) != 0;
                if !was_enabled && now_enabled {
                    // CPU now writes on the instruction's final bus cycle; model the
                    // 2C02's short rendering-enable latch delay (~3 PPU dots).
                    self.render_enable_delay = 3;
                }
            }
            3 => {
                self.oam_addr = val;
            }
            4 => {
                let v = if (self.oam_addr & 3) == 2 { val & 0xE3 } else { val };
                self.oam[self.oam_addr as usize] = v;
                self.oam_addr = self.oam_addr.wrapping_add(1);
            }
            5 => {
                if !self.write_latch {
                    self.fine_x = val & 0x07;
                    self.t = (self.t & 0xFFE0) | ((val as u16) >> 3);
                    self.write_latch = true;
                } else {
                    self.t = (self.t & 0x8FFF) | (((val as u16) & 0x07) << 12);
                    self.t = (self.t & 0xFC1F) | (((val as u16) & 0xF8) << 2);
                    self.write_latch = false;
                }
            }
            6 => {
                if !self.write_latch {
                    self.t = (self.t & 0x00FF) | (((val as u16) & 0x3F) << 8);
                    self.write_latch = true;
                } else {
                    self.t = (self.t & 0xFF00) | (val as u16);
                    // The copy into v lands ~3 dots later; a dot-256/257 scroll update racing
                    // with this write is overwritten (Kirby's Adventure status-bar split).
                    self.v_update_delay = 3;
                    self.write_latch = false;
                }
            }
            7 => {
                let a = self.v & 0x3FFF;
                self.ppu_write(a, val, mapper);
                self.increment_vram_addr();
            }
            _ => {}
        }
    }

    #[inline]
    fn increment_vram_addr(&mut self) {
        let step = if (self.ctrl & 0x04) != 0 { 32 } else { 1 };
        self.v = self.v.wrapping_add(step) & 0x7FFF;
    }

    #[inline]
    fn increment_scroll_y(&mut self) {
        if !self.rendering_enabled() {
            return;
        }
        if (self.v & 0x7000) != 0x7000 {
            self.v += 0x1000;
        } else {
            self.v &= !0x7000;
            let mut y = (self.v & 0x03E0) >> 5;
            if y == 29 {
                y = 0;
                self.v ^= 0x0800;
            } else if y == 31 {
                y = 0;
            } else {
                y += 1;
            }
            self.v = (self.v & !0x03E0) | (y << 5);
        }
    }

    #[inline]
    fn transfer_address_x(&mut self) {
        if self.rendering_enabled() {
            self.v = (self.v & !0x041F) | (self.t & 0x041F);
        }
    }

    #[inline]
    fn transfer_address_y(&mut self) {
        if self.rendering_enabled() {
            self.v = (self.v & !0x7BE0) | (self.t & 0x7BE0);
        }
    }

    pub fn step(&mut self, cycles: u32, mapper: &mut Mapper) {
        for _ in 0..cycles {
            if self.render_enable_delay > 0 {
                self.render_enable_delay -= 1;
            }
            if self.scanline < 240 {
                if self.dot == 256 {
                    self.render_scanline(mapper);
                    self.increment_scroll_y();
                } else if self.dot == 257 {
                    self.transfer_address_x();
                    if self.rendering_enabled() {
                        self.oam_addr = 0;
                    }
                } else if self.dot == 260 && self.rendering_enabled() {
                    mapper.clock_scanline_irq();
                }
            } else if self.scanline == 241 && self.dot == 1 {
                if !self.suppress_vblank {
                    self.status |= 0x80;
                    if (self.ctrl & 0x80) != 0 {
                        self.nmi_pending = true;
                    }
                }
                self.suppress_vblank = false;
                self.frame_complete = true;
            } else if self.scanline == 261 {
                if self.dot == 1 {
                    self.status &= !(0x80 | 0x40 | 0x20);
                } else if self.dot == 257 {
                    self.transfer_address_x();
                } else if self.dot == 260 && self.rendering_enabled() {
                    mapper.clock_scanline_irq();
                } else if self.dot >= 280 && self.dot <= 304 {
                    self.transfer_address_y();
                }
            }

            if self.v_update_delay > 0 {
                self.v_update_delay -= 1;
                if self.v_update_delay == 0 {
                    self.v = self.t;
                }
            }

            // Odd-frame skip: with rendering on, the prerender line is one dot shorter
            // on odd frames (dot 339 jumps straight to scanline 0, dot 0).
            if self.scanline == 261 && self.dot == 339 && self.odd_frame && self.rendering_enabled() {
                self.dot = 340;
            }

            self.dot += 1;
            if self.dot >= 341 {
                self.dot = 0;
                self.scanline += 1;
                if self.scanline >= 262 {
                    self.scanline = 0;
                    self.odd_frame = !self.odd_frame;
                }
            }
        }
    }

    fn render_scanline(&mut self, mapper: &Mapper) {
        let y = self.scanline as usize;
        let universal_bg = self.palette[0] & 0x3F;
        let show_bg = (self.mask & 0x08) != 0;
        let show_sp = (self.mask & 0x10) != 0;
        let show_left_bg = (self.mask & 0x02) != 0;
        let show_left_sp = (self.mask & 0x04) != 0;

        let mut bg_opaque = [false; SCREEN_WIDTH];
        let mut line_palette = [universal_bg; SCREEN_WIDTH];

        if show_bg {
            let bg_pattern_base: u16 = if (self.ctrl & 0x10) != 0 {
                0x1000
            } else {
                0x0000
            };
            let fine_y = (self.v >> 12) & 0x07;
            let coarse_y = (self.v >> 5) & 0x1F;
            let nt_y = (self.v >> 11) & 0x01;

            let mut cur_nt_x = (self.v >> 10) & 0x01;
            let mut cur_coarse_x = self.v & 0x1F;

            for tile_step in 0..33u16 {
                let nt_addr = 0x2000 | (nt_y << 11) | (cur_nt_x << 10) | (coarse_y << 5) | cur_coarse_x;
                let tile_id = self.vram[self.mirror_vram_addr(nt_addr, mapper.mirroring)] as u16;

                let attr_addr = 0x23C0
                    | (nt_y << 11)
                    | (cur_nt_x << 10)
                    | ((coarse_y >> 2) << 3)
                    | (cur_coarse_x >> 2);
                let attr_byte = self.vram[self.mirror_vram_addr(attr_addr, mapper.mirroring)];
                let shift = ((coarse_y & 2) << 1) | (cur_coarse_x & 2);
                let pal_high = ((attr_byte >> shift) & 0x03) << 2;

                let patt_addr = bg_pattern_base + (tile_id << 4) + fine_y;
                let lo = mapper.read_chr(patt_addr);
                let hi = mapper.read_chr(patt_addr + 8);

                for bit in 0..8u16 {
                    let screen_x = (tile_step * 8 + bit) as i32 - (self.fine_x as i32);
                    if (0..256).contains(&screen_x) {
                        let sx = screen_x as usize;
                        if sx >= 8 || show_left_bg {
                            let mask = 0x80 >> bit;
                            let p0 = u8::from((lo & mask) != 0);
                            let p1 = if (hi & mask) != 0 { 2 } else { 0 };
                            let pix = p0 | p1;
                            if pix != 0 {
                                bg_opaque[sx] = true;
                                let pal_idx = (pal_high | pix) as usize;
                                line_palette[sx] = self.palette[pal_idx] & 0x3F;
                            }
                        }
                    }
                }

                if cur_coarse_x == 31 {
                    cur_coarse_x = 0;
                    cur_nt_x ^= 1;
                } else {
                    cur_coarse_x += 1;
                }
            }
        }

        if show_sp {
            let sprite_height: i32 = if (self.ctrl & 0x20) != 0 { 16 } else { 8 };
            let sp_pattern_base: u16 = if (self.ctrl & 0x08) != 0 {
                0x1000
            } else {
                0x0000
            };

            let mut sprites_on_line = [0usize; 8];
            let mut sp_count = 0usize;

            for i in 0..64usize {
                let sp_y = (self.oam[i * 4] as i32) + 1;
                let row = (y as i32) - sp_y;
                if row >= 0 && row < sprite_height {
                    if sp_count < 8 {
                        sprites_on_line[sp_count] = i;
                        sp_count += 1;
                    } else {
                        self.status |= 0x20;
                        break;
                    }
                }
            }

            // Hardware sprite priority: the lowest-index opaque sprite pixel wins the
            // sprite multiplexer, *then* its priority bit decides sprite vs BG. A
            // behind-BG sprite therefore still masks higher-index front sprites.
            let mut sprite_px_taken = [false; SCREEN_WIDTH];
            for idx in 0..sp_count {
                let i = sprites_on_line[idx];
                let sp_y = (self.oam[i * 4] as i32) + 1;
                let tile = self.oam[i * 4 + 1] as u16;
                let attr = self.oam[i * 4 + 2];
                let sp_x = self.oam[i * 4 + 3] as i32;

                let flip_v = (attr & 0x80) != 0;
                let flip_h = (attr & 0x40) != 0;
                let behind_bg = (attr & 0x20) != 0;
                let pal_high = 0x10 | ((attr & 0x03) << 2);

                let mut row = (y as i32) - sp_y;
                if flip_v {
                    row = sprite_height - 1 - row;
                }

                let patt_addr = if sprite_height == 16 {
                    let bank = (tile & 1) << 12;
                    let top_tile = tile & 0xFE;
                    let t_num = if row < 8 { top_tile } else { top_tile + 1 };
                    bank + (t_num << 4) + ((row as u16) & 7)
                } else {
                    sp_pattern_base + (tile << 4) + (row as u16)
                };

                let lo = mapper.read_chr(patt_addr);
                let hi = mapper.read_chr(patt_addr + 8);

                for col in 0..8i32 {
                    let sx_i32 = sp_x + col;
                    if (0..256).contains(&sx_i32) {
                        let sx = sx_i32 as usize;
                        if sx >= 8 || show_left_sp {
                            let bit = if flip_h { col } else { 7 - col };
                            let mask = 1u8 << bit;
                            let p0 = u8::from((lo & mask) != 0);
                            let p1 = if (hi & mask) != 0 { 2 } else { 0 };
                            let pix = p0 | p1;

                            if pix != 0 {
                                if i == 0 && bg_opaque[sx] && sx < 255 && show_bg {
                                    self.status |= 0x40;
                                }
                                if sprite_px_taken[sx] {
                                    continue;
                                }
                                sprite_px_taken[sx] = true;
                                if !behind_bg || !bg_opaque[sx] {
                                    let pal_idx = Self::mirror_palette_addr((pal_high | pix) as u16);
                                    line_palette[sx] = self.palette[pal_idx] & 0x3F;
                                }
                            }
                        }
                    }
                }
            }
        }

        let row_offset = y * SCREEN_WIDTH * 4;
        for x in 0..SCREEN_WIDTH {
            let rgb = NES_PALETTE[(line_palette[x] & 0x3F) as usize];
            let off = row_offset + x * 4;
            self.framebuffer[off] = rgb[0];
            self.framebuffer[off + 1] = rgb[1];
            self.framebuffer[off + 2] = rgb[2];
            self.framebuffer[off + 3] = 255;
        }
    }
}
