// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones
//
// Portions derived from Mesen (https://github.com/nesdev-org/MesenCE):
// the dot-level rendering pipeline, sprite evaluation and register timing.
// Mesen is Copyright (C) 2014-2026 Sour and contributors, GPL-3.0-or-later.

//! Ricoh 2C02 PPU, emulated dot-by-dot: real background fetch pipeline and shift
//! registers, cycle-accurate sprite evaluation (including the overflow bug), sprite
//! pattern fetches on dots 257-320 (so mapper A12 IRQ counters see the real bus
//! pattern), delayed $2006/$2007 side effects, odd-frame dot skip, VBL/NMI races and
//! open-bus decay. Timing follows Mesen's model, driven from the CPU's master clock.

use crate::mappers::Mapper;

/// Visible picture width in pixels.
pub const SCREEN_WIDTH: usize = 256;
/// Visible picture height in pixels (all 240 lines are emitted; front ends usually crop 8 top/bottom).
pub const SCREEN_HEIGHT: usize = 240;

/// Scanline on whose dot 1 the VBlank flag is set and NMI raised (before overclock lines are added).
const NMI_SCANLINE: i32 = 241;
/// Last VBlank scanline (before overclock lines); the line after it wraps to the pre-render line (-1).
const VBLANK_END: i32 = 260;

/// Background tile being fetched for the next 8 pixels (latched into the shift registers
/// every 8 dots by `load_tile_info`).
#[derive(Clone, Copy, Default)]
struct TileInfo {
    /// Pattern-table address of the tile's low plane: `(tile index << 4) | fine Y | bg_pt`.
    /// On dots 337/339 (and so on dot 0) it temporarily holds the raw nametable byte instead,
    /// which `process_first_cycle` turns into an address for the dot-0 bus access.
    tile_addr: u16,
    /// Pattern low-plane byte (bit 0 of each pixel).
    low: u8,
    /// Pattern high-plane byte (bit 1 of each pixel).
    high: u8,
    /// Attribute palette selector shifted into place: 0, 4, 8 or 12 (background palette base index).
    palette_offset: u8,
}

/// One sprite loaded for the current scanline during the dot 257-320 fetches.
#[derive(Clone, Copy, Default)]
struct SpriteInfo {
    /// Pattern low-plane byte for this scanline's row (already horizontally flipped if needed).
    low: u8,
    /// Pattern high-plane byte (already horizontally flipped if needed).
    high: u8,
    /// Palette base index: 0x10 | (palette << 2), i.e. 16, 20, 24 or 28.
    palette_offset: u8,
    /// Attribute bit 5: sprite is drawn behind non-transparent background pixels.
    behind_bg: bool,
    /// Left edge X coordinate (OAM byte 3).
    x: u8,
}

/// The 2C02 picture processing unit.
///
/// Advanced dot-by-dot (341 dots x 262 scanlines per NTSC frame) by [`Ppu::run`], which the
/// CPU calls twice per CPU cycle with the shared master clock. Field naming follows the
/// nesdev wiki ("Loopy" `v`/`t`/`x`/`w` scroll registers).
#[repr(C)]
#[derive(Clone)]
pub struct Ppu {
    /// Current dot within the scanline (0-340).
    pub cycle: u16,
    /// Current scanline: -1 = pre-render, 0-239 visible, 240 post-render, 241+ VBlank.
    pub scanline: i32,
    /// Frames started since power-on (its parity selects the odd-frame dot skip).
    pub frame_count: u64,
    /// PPU master clock already executed up to (4 master clocks per dot); see [`Ppu::run`].
    master_clock: u64,

    /// NMI output line (CPU edge-detects this).
    pub nmi_line: bool,
    /// Set when a full frame has been rendered (scanline 240 reached).
    pub frame_complete: bool,
    /// CPU cycle counter mirror for mapper A12 filtering.
    pub cpu_cycle: u64,
    /// Cached `Mapper::wants_vram_addr()` (set by the system at power-on).
    pub vram_hook: bool,

    // Registers.
    /// Current VRAM address / scroll position (15 bits, "Loopy v").
    pub v: u16,
    /// Temporary VRAM address / scroll latch ("Loopy t").
    pub t: u16,
    /// Fine X scroll (0-7).
    pub x: u8,
    /// First/second write toggle shared by $2005/$2006.
    pub w: bool,
    /// Last value written to $2000 (PPUCTRL).
    pub ctrl: u8,
    /// Last value written to $2001 (PPUMASK).
    pub mask: u8,
    /// VBlank flag (PPUSTATUS bit 7). Set on dot 1 of the NMI scanline, cleared by a $2002
    /// read and on dot 1 of the pre-render line.
    vbl: bool,
    /// Sprite-0 hit flag (PPUSTATUS bit 6). Cleared at the start of the pre-render line.
    sprite0_hit: bool,
    /// Sprite overflow flag (PPUSTATUS bit 5), set by the buggy evaluation in
    /// `process_sprite_evaluation`. Cleared at the start of the pre-render line.
    sprite_overflow: bool,
    /// OAMADDR ($2003).
    pub oam_addr: u8,
    /// Primary object attribute memory: 64 sprites x (Y, tile, attributes, X).
    pub oam: [u8; 256],
    /// Secondary OAM: up to 8 sprites (4 bytes each) selected for the next scanline. Cleared
    /// to $FF on dots 1-64 and filled by sprite evaluation on dots 65-256.
    sec_oam: [u8; 32],
    /// Palette RAM ($3F00-$3F1F), 6-bit colour indices.
    pub palette: [u8; 32],
    /// $2007 read buffer: a $2007 read of non-palette memory returns this, and the buffer is
    /// refilled from VRAM a few dots later (see `read_sm`).
    read_buffer: u8,
    /// PPU I/O data bus latch ("open bus"), returned for undriven bits of register reads.
    open_bus: u8,
    /// Frame number at which each open-bus bit (index = bit number) was last refreshed. A bit
    /// not refreshed for more than 3 frames decays to 0 on the next partial update (`set_open_bus`).
    open_bus_stamp: [u64; 8],

    // Derived control/mask state.
    /// PPUCTRL bit 2: $2007 accesses increment `v` by 32 (one row) instead of 1.
    vram_inc32: bool,
    /// PPUCTRL bit 3: sprite pattern table base ($0000 or $1000) for 8x8 sprites.
    sprite_pt: u16,
    /// PPUCTRL bit 4: background pattern table base ($0000 or $1000).
    bg_pt: u16,
    /// PPUCTRL bit 5: 8x16 sprites.
    large_sprites: bool,
    /// PPUCTRL bit 7: raise NMI at VBlank start.
    nmi_enabled: bool,
    /// PPUMASK bit 3: background rendering enabled.
    bg_enabled: bool,
    /// PPUMASK bit 4: sprite rendering enabled.
    sprites_enabled: bool,
    /// Background pixels are drawn only when `cycle` exceeds this: 0 (shown everywhere), 8
    /// (left 8 pixels clipped, PPUMASK bit 1 clear) or 300 (background disabled).
    min_bg_cycle: u16,
    /// Like `min_bg_cycle`, for sprites (PPUMASK bits 4 and 2).
    min_sprite_cycle: u16,
    /// Mask applied to colour indices: $30 in greyscale mode (PPUMASK bit 0), else $3F.
    palette_mask: u8,
    /// Colour emphasis bits (PPUMASK bits 5-7) pre-shifted to bits 6-8 of a `pixels` entry.
    emphasis: u16,

    // Rendering pipeline.
    /// Background tile currently being fetched.
    tile: TileInfo,
    /// Background pattern low-plane shift register. Upper byte = pixels being output,
    /// lower byte = next tile (reloaded every 8 dots). Shifted left once per dot; the pixel is
    /// taken from bit 15 after shifting by fine X.
    low_shift: u16,
    /// Background pattern high-plane shift register (same layout as `low_shift`; shifts in 1s).
    high_shift: u16,
    /// Palette base for the tile in the lower byte of the shift registers (the next tile).
    cur_palette: u8,
    /// Palette base for the tile currently in the upper byte (the tile being output).
    /// `get_pixel_color` chooses between this and `cur_palette` using fine X and the dot.
    prev_palette: u8,
    /// Sprites loaded for the current scanline (first `sprite_count` entries are valid), in
    /// OAM priority order (index 0 = highest priority).
    sprites: [SpriteInfo; 8],
    /// Number of valid entries in `sprites` (0-8).
    sprite_count: u8,
    /// Secondary-OAM slot being fetched during dots 257-320.
    sprite_index: u8,
    /// Set by sprite evaluation on the next line when the sprite examined first (on dot 66)
    /// is in range, i.e. it was copied to secondary OAM slot 0. Normally that is OAM sprite 0,
    /// but it is whatever OAMADDR pointed at. Becomes `sprite0_visible` on dots 257-320.
    sprite0_added: bool,
    /// Slot 0 of the current line's `sprites` holds the "sprite 0" from the previous line's
    /// evaluation (copied from `sprite0_added`), so it may trigger sprite-0 hit. Separate from
    /// `sprite0_added` because evaluation for the next line overwrites that during dots 65-256
    /// while the current line is still being drawn.
    sprite0_visible: bool,
    /// Evaluation: the sprite whose Y was just read is in range for the next scanline, so its
    /// remaining 3 bytes are being copied to secondary OAM.
    sprite_in_range: bool,
    /// Write index into `sec_oam` (0-32; 32 means secondary OAM is full).
    sec_oam_addr: u8,
    /// Byte latched by evaluation's odd-dot OAM read and written to secondary OAM on the even
    /// dot. Also what $2004 reads return during rendering ($FF on dots 1-64).
    oam_copy_buffer: u8,
    /// Evaluation has wrapped through all 64 OAM sprites (or the overflow search ended);
    /// remaining dots just read secondary OAM.
    oam_copy_done: bool,
    /// Emulates the hardware sprite-overflow search ending after a sprite is found with
    /// secondary OAM full: set to 3 when an in-range sprite is found and counted down on each
    /// further in-range read, ending evaluation when it reaches 0.
    overflow_bug_counter: u8,

    // Delayed state changes.
    /// Rendering (background or sprites) enabled as seen by the PPU. Follows PPUMASK one
    /// dot after a $2001 write (updated in `update_state`).
    rendering_enabled: bool,
    /// `rendering_enabled` delayed by one more dot. Most of the fetch/evaluation pipeline in
    /// `process_scanline` uses this, matching the extra delay with which a $2001 change takes
    /// effect on hardware; when it turns off mid-frame the bus address reverts to `v`.
    prev_rendering_enabled: bool,
    /// Some delayed effect is pending: `update_state` must run after the current dot.
    need_state_update: bool,
    /// Dots remaining until a second $2006 write lands in `v` (set to 3, applied when it
    /// reaches 0). If the copy coincides with a rendering scroll increment (dot 257, or a
    /// coarse-X increment on dots 8, 16 ... 256 / 328, 336), the new value is ANDed with the
    /// incremented `v` instead of replacing it (the "$2006 AND glitch"): all bits on dot 257,
    /// only the coarse X / horizontal nametable bits ($041F) on the coarse-X dots.
    update_vram_delay: u8,
    /// Value of `t` captured at the second $2006 write, copied to `v` after `update_vram_delay`.
    update_vram_value: u16,
    /// Dots during which a further $2007 read is ignored (returns open bus). Set to 6 by a
    /// $2007 read: two reads on consecutive CPU cycles only perform one access.
    ignore_vram_read: u8,
    /// $2007 read state machine: set to 5 by a read, counts down once per dot; at 0 the read
    /// buffer is refilled from `bus_address`, and `v` is incremented one dot later
    /// (`need_vram_increment`).
    read_sm: u8,
    /// $2007 write state machine: set to 5 by a write, counts down once per dot; at 0
    /// `write_latch` is written to `bus_address` (palette, VRAM, or during rendering the low
    /// byte of the address instead of the data), then `v` is incremented one dot later.
    write_sm: u8,
    /// Value of the pending $2007 write.
    write_latch: u8,
    /// Increment `v` after a $2007 access on the next `update_state` (by 1/32 outside
    /// rendering, or as a coarse X + Y increment during rendering).
    need_vram_increment: bool,
    /// Current PPU address bus value (last address driven by a fetch or by `v`). $2007 reads
    /// and writes act on this address.
    bus_address: u16,
    /// A $2002 read on dot 0 of the NMI scanline suppresses setting the VBlank flag (and so
    /// the NMI) for this frame.
    prevent_vbl: bool,

    /// Extra idle scanlines inserted before VBlank (CPU overclock); applied at frame start.
    pub extra_scanlines: u16,
    /// Overclock lines in effect for the current frame (copy of `extra_scanlines`, latched
    /// when the pre-render line starts).
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
    /// Power-on state (begins at the end of the pre-render line).
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
            vram_hook: true,
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

    /// Whether background or sprite rendering is on (as latched by the PPU, one dot late).
    #[inline]
    pub fn rendering_enabled(&self) -> bool {
        self.rendering_enabled
    }

    /// Scanline that starts VBlank, shifted by the active overclock lines.
    #[inline]
    fn nmi_scanline(&self) -> i32 {
        NMI_SCANLINE + self.active_extra as i32
    }

    /// Last VBlank scanline, shifted by the active overclock lines.
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

    /// Updates the open-bus latch: bits set in `mask` take the corresponding bit of `value`
    /// and refresh their decay stamp; other bits decay to 0 if not refreshed for more than
    /// 3 frames. A full mask ($FF) just replaces the latch.
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

    /// Completes a register read: bits set in `mask` come from the open-bus latch, the rest
    /// from `value` (and those driven bits refresh the latch).
    #[inline]
    fn apply_open_bus(&mut self, mask: u8, value: u8) -> u8 {
        self.set_open_bus(!mask, value);
        value | (self.open_bus & mask)
    }

    // ---------------------------------------------------------------- CPU interface

    /// CPU read of a PPU register ($2000-$3FFF, mirrored every 8 bytes), including the
    /// VBL-flag/NMI race, the delayed $2007 read buffer and open-bus behaviour.
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

    /// CPU write of a PPU register. $2006 and $2007 side effects land a few dots later, as on hardware.
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

    /// $2000 (PPUCTRL) write: nametable select into `t` bits 10-11, derived flags, and the
    /// NMI line (enabling NMI while VBlank is set raises it immediately; disabling drops it).
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

    /// $2001 (PPUMASK) write: derived enable/greyscale/emphasis/clipping state. A change in
    /// rendering enable is applied with a delay by `update_state`.
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

    /// Recomputes `min_bg_cycle` / `min_sprite_cycle` from PPUMASK.
    fn update_min_draw_cycles(&mut self) {
        self.min_bg_cycle = if self.bg_enabled { if self.mask & 0x02 != 0 { 0 } else { 8 } } else { 300 };
        self.min_sprite_cycle = if self.sprites_enabled { if self.mask & 0x04 != 0 { 0 } else { 8 } } else { 300 };
    }

    // ---------------------------------------------------------------- VRAM access

    /// Reads palette RAM; $3F10/$3F14/$3F18/$3F1C mirror $3F00/$3F04/$3F08/$3F0C.
    #[inline]
    fn read_palette(&self, addr: u16) -> u8 {
        let mut a = (addr & 0x1F) as usize;
        if a & 0x13 == 0x10 {
            a &= !0x10;
        }
        self.palette[a]
    }

    /// Writes palette RAM (6-bit). Entry 0 of each palette is shared between background and
    /// sprite halves, so it is written to both mirrors.
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

    /// Drives the PPU address bus, notifying the mapper (for A12-based IRQ counters) when
    /// `vram_hook` is set.
    #[inline]
    fn set_bus_address(&mut self, addr: u16, m: &mut dyn Mapper) {
        self.bus_address = addr;
        if self.vram_hook {
            m.notify_vram_addr(addr, self.cpu_cycle);
        }
    }

    /// Rendering fetch: drives `addr` on the bus and reads it through the mapper.
    #[inline]
    fn read_vram(&mut self, addr: u16, m: &mut dyn Mapper) -> u8 {
        self.set_bus_address(addr, m);
        m.ppu_read(addr & 0x3FFF)
    }

    /// Nametable byte address for the current `v`.
    #[inline]
    fn nt_addr(&self) -> u16 {
        0x2000 | (self.v & 0x0FFF)
    }

    /// Attribute byte address for the current `v`.
    #[inline]
    fn attr_addr(&self) -> u16 {
        0x23C0 | (self.v & 0x0C00) | ((self.v >> 4) & 0x38) | ((self.v >> 2) & 0x07)
    }

    /// Coarse X increment of `v`, wrapping into the horizontally adjacent nametable.
    #[inline]
    fn inc_h(&mut self) {
        if (self.v & 0x001F) == 31 {
            self.v = (self.v & !0x001F) ^ 0x0400;
        } else {
            self.v += 1;
        }
    }

    /// Y increment of `v`: fine Y, then coarse Y; row 29 wraps to the vertically adjacent
    /// nametable, rows 30-31 wrap to 0 without switching.
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

    /// Post-$2007 increment of `v`: by 1 or 32 outside rendering (then drives `v` on the bus);
    /// during rendering, a simultaneous coarse X and Y increment as on hardware.
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

    /// Advances one dot: visible/pre-render scanline work, VBlank start on dot 1 of the NMI
    /// scanline, the scanline wrap after dot 340, then any pending delayed state changes.
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

    /// Dot 0 of a new scanline: advances the scanline (wrapping to -1 and latching overclock
    /// state), updates `apu_enabled`, clears status flags on the pre-render line, performs the
    /// dot-0 pattern bus access (skipped on line 0 of odd frames) and emits the frame at line 240.
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

    /// Converts `pixels` to RGBA in `framebuffer` and sets `frame_complete`.
    fn send_frame(&mut self) {
        let lut = crate::palette::rgba_table();
        for (px, &p) in self.framebuffer.chunks_exact_mut(4).zip(self.pixels.iter()) {
            px.copy_from_slice(&lut[(p & 0x1FF) as usize].to_le_bytes());
        }
        self.frame_complete = true;
    }

    /// Per-dot work on the visible (0-239) and pre-render (-1) lines: pixel output and sprite
    /// evaluation (1-256), sprite fetches and horizontal `v` reload (257-320), vertical `v`
    /// reload on the pre-render line (280-304), next-line tile prefetch (321-336), dummy
    /// nametable fetches (337/339) and the odd-frame dot skip.
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

    /// Shifts the background pattern registers by one pixel.
    #[inline]
    fn shift_tile_registers(&mut self) {
        self.low_shift <<= 1;
        self.high_shift = (self.high_shift << 1) | 1;
    }

    /// Background fetch pipeline step for the current dot: nametable (dot 1 of 8), attribute (3),
    /// pattern low (5), pattern high (7); on dot 0 of 8 the fetched tile is loaded into the low
    /// byte of the shift registers.
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

    /// Sprite fetch for the secondary-OAM slot of the current dot (257-320).
    fn load_sprite_tile_info(&mut self, m: &mut dyn Mapper) {
        self.sprite_index = ((self.cycle - 257) >> 3) as u8;
        let i = self.sprite_index as usize * 4;
        let (y, tile, attr, x) = (self.sec_oam[i], self.sec_oam[i + 1], self.sec_oam[i + 2], self.sec_oam[i + 3]);
        self.load_sprite(y, tile, attr, x, m);
    }

    /// Fetches the pattern row of one sprite for the next scanline (both bus reads always
    /// happen so the mapper sees them). If the sprite is in range it is added to `sprites`,
    /// pre-flipped horizontally; empty/out-of-range slots ($FF filler) are discarded.
    fn load_sprite(&mut self, y: u8, tile: u8, attr: u8, x: u8, m: &mut dyn Mapper) {
        let flip_h = attr & 0x40 != 0;
        let flip_v = attr & 0x80 != 0;
        let size_mask: u16 = if self.large_sprites { 15 } else { 7 };
        // The hardware compares only 8 bits of the line number, so the pre-render line
        // (261) appears as 261 & 0xFF = 5. Sprites found here are never drawn (see below).
        let line8 = if self.scanline >= 0 { self.scanline as u8 } else { (261u16 & 0xFF) as u8 };
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

    /// Palette index (0-31) of the current pixel from background and sprite pixels,
    /// applying clipping, priority and sprite-0 hit detection (not at dot 256).
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

    /// Writes the current dot's pixel into `pixels`. With rendering disabled and `v` pointing
    /// at palette RAM, that palette entry is shown (the "background palette hack").
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

    /// Cycle-accurate sprite evaluation for the next scanline (dots 1-256). Dots 1-64 clear
    /// secondary OAM to $FF; from 65, odd dots read `oam[oam_addr]` and even dots copy it to
    /// secondary OAM and advance OAMADDR. Once 8 sprites are found, the hardware's buggy
    /// overflow search runs, incrementing both the sprite and byte index on a miss, which
    /// produces false positives and negatives for the overflow flag.
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

    /// Applies delayed effects once per dot while `need_state_update` is set: the
    /// `rendering_enabled` / `prev_rendering_enabled` pipeline, the delayed $2006 copy into `v`
    /// (with the AND glitch), the $2007 ignore window, the $2007 read/write state machines
    /// and the following `v` increment. Re-arms itself while anything is still pending.
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

    /// Returns `true` when the Zapper photodiode aimed at `(zap_x, zap_y)` senses CRT beam light
    /// at the current PPU scanline/dot (including the ~24-scanline phosphor persistence window).
    pub fn zapper_sees_light(&self, zap_x: i32, zap_y: i32) -> bool {
        if !(0..256).contains(&zap_x) || !(0..240).contains(&zap_y) {
            return false;
        }
        if !(0..240).contains(&self.scanline) {
            return false;
        }
        let lut = crate::palette::rgba_table();
        let y0 = (zap_y - 3).max(0);
        let y1 = (zap_y + 3).min(239);
        let x0 = (zap_x - 3).max(0);
        let x1 = (zap_x + 3).min(255);
        for py in y0..=y1 {
            if py > self.scanline || self.scanline - py > 24 {
                continue;
            }
            for px in x0..=x1 {
                if py == self.scanline && px >= self.cycle as i32 {
                    continue;
                }
                let raw = self.pixels[(py as usize) * SCREEN_WIDTH + (px as usize)];
                let rgba = lut[(raw & 0x1FF) as usize].to_le_bytes();
                let luma = rgba[0] as u32 + rgba[1] as u32 + rgba[2] as u32;
                if luma >= 350 {
                    return true;
                }
            }
        }
        false
    }

    /// Serializes the PPU state into `out`.
    pub fn save_state(&self, out: &mut Vec<u8>) {
        let pod_len = std::mem::offset_of!(Self, pixels);
        out.extend_from_slice(&(pod_len as u32).to_le_bytes());
        // SAFETY: All fields of `Ppu` before `pixels` are plain-old-data (`Copy`).
        let pod = unsafe { std::slice::from_raw_parts(self as *const Self as *const u8, pod_len) };
        out.extend_from_slice(pod);
        // Save the in-progress 9-bit `pixels` buffer so mid-frame save states are bit-exact.
        let px_bytes = unsafe {
            std::slice::from_raw_parts(
                self.pixels.as_ptr() as *const u8,
                self.pixels.len() * std::mem::size_of::<u16>(),
            )
        };
        out.extend_from_slice(&(px_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(px_bytes);
    }

    /// Restores the PPU state from `cur`.
    pub fn load_state(&mut self, cur: &mut &[u8]) -> Result<(), String> {
        let pod_len = std::mem::offset_of!(Self, pixels);
        if cur.len() < 4 {
            return Err("Truncated save state (PPU header)".into());
        }
        let (len_b, rest) = cur.split_at(4);
        *cur = rest;
        let saved_pod = u32::from_le_bytes(len_b.try_into().unwrap()) as usize;
        if saved_pod != pod_len || cur.len() < pod_len {
            return Err("PPU state size mismatch".into());
        }
        let (pod, rest2) = cur.split_at(pod_len);
        *cur = rest2;
        // SAFETY: All fields before `pixels` are plain-old-data.
        unsafe {
            std::ptr::copy_nonoverlapping(pod.as_ptr(), self as *mut Self as *mut u8, pod_len);
        }
        if cur.len() < 4 {
            return Err("Truncated save state (PPU pixels)".into());
        }
        let (plen_b, rest3) = cur.split_at(4);
        *cur = rest3;
        let px_byte_len = u32::from_le_bytes(plen_b.try_into().unwrap()) as usize;
        let expected_px_bytes = self.pixels.len() * std::mem::size_of::<u16>();
        if px_byte_len != expected_px_bytes || cur.len() < expected_px_bytes {
            return Err("PPU pixel buffer size mismatch".into());
        }
        let (px_data, rest4) = cur.split_at(expected_px_bytes);
        *cur = rest4;
        unsafe {
            std::ptr::copy_nonoverlapping(
                px_data.as_ptr(),
                self.pixels.as_mut_ptr() as *mut u8,
                expected_px_bytes,
            );
        }
        let lut = crate::palette::rgba_table();
        for (px, &p) in self.framebuffer.chunks_exact_mut(4).zip(self.pixels.iter()) {
            px.copy_from_slice(&lut[(p & 0x1FF) as usize].to_le_bytes());
        }
        Ok(())
    }
}
