// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones

//! Standard 64-color Ricoh 2C02 NTSC PPU RGB palette.

/// RGB for each of the 64 2C02 colour indices (a commonly used NTSC approximation).
pub static NES_PALETTE: [[u8; 3]; 64] = [
    [84, 84, 84],
    [0, 30, 116],
    [8, 16, 144],
    [48, 0, 136],
    [68, 0, 100],
    [92, 0, 48],
    [84, 4, 0],
    [60, 24, 0],
    [32, 42, 0],
    [8, 58, 0],
    [0, 64, 0],
    [0, 60, 0],
    [0, 50, 60],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [152, 150, 152],
    [8, 76, 196],
    [48, 50, 236],
    [92, 30, 228],
    [136, 20, 176],
    [160, 20, 100],
    [152, 34, 32],
    [120, 60, 0],
    [84, 90, 0],
    [40, 114, 0],
    [8, 124, 0],
    [0, 118, 40],
    [0, 102, 120],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [236, 238, 236],
    [76, 154, 236],
    [120, 124, 236],
    [176, 98, 236],
    [228, 84, 236],
    [236, 88, 180],
    [236, 106, 100],
    [212, 136, 32],
    [160, 170, 0],
    [116, 196, 0],
    [76, 208, 32],
    [56, 204, 108],
    [56, 180, 204],
    [60, 60, 60],
    [0, 0, 0],
    [0, 0, 0],
    [236, 238, 236],
    [168, 204, 236],
    [188, 188, 236],
    [212, 178, 236],
    [236, 174, 236],
    [236, 174, 212],
    [236, 180, 176],
    [228, 196, 144],
    [204, 210, 120],
    [180, 222, 120],
    [168, 226, 144],
    [152, 226, 180],
    [160, 214, 228],
    [160, 162, 160],
    [0, 0, 0],
    [0, 0, 0],
];

/// Applies the $2001 colour-emphasis bits (bit 0 = red, 1 = green, 2 = blue): the
/// non-emphasised channels are attenuated, as the 2C02's NTSC output does.
#[inline]
pub fn apply_emphasis(rgb: [u8; 3], emphasis: u8) -> [u8; 3] {
    if emphasis == 0 {
        return rgb;
    }
    let mut out = rgb;
    for (i, c) in out.iter_mut().enumerate() {
        if emphasis & (1 << i) == 0 {
            *c = ((*c as u16 * 3) / 4) as u8;
        }
    }
    if emphasis == 7 {
        for c in out.iter_mut() {
            *c = ((*c as u16 * 3) / 4) as u8;
        }
    }
    out
}

/// RGBA bytes for every (emphasis << 6 | colour) pixel value, little-endian packed.
pub fn rgba_table() -> &'static [u32; 512] {
    static T: std::sync::OnceLock<[u32; 512]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0u32; 512];
        for (i, v) in t.iter_mut().enumerate() {
            let rgb = apply_emphasis(NES_PALETTE[i & 0x3F], (i >> 6) as u8);
            *v = u32::from_le_bytes([rgb[0], rgb[1], rgb[2], 255]);
        }
        t
    })
}
