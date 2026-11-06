// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones

//! iNES 1.0 / NES 2.0 ROM loader, .zip archive extractor, header parser (with a
//! CRC-keyed fixup table for known-bad headers), and battery-backed SRAM persistence.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use flate2::read::DeflateDecoder;

/// How the four logical nametables ($2000/$2400/$2800/$2C00) map onto the console's
/// 2 KB of nametable RAM (CIRAM). Boards with mapper-controlled mirroring change this at run time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mirroring {
    /// All four nametables show the first 1 KB page.
    OneScreenLower,
    /// All four nametables show the second 1 KB page.
    OneScreenUpper,
    /// $2000=$2800, $2400=$2C00: side-by-side screens (horizontal scrolling games).
    Vertical,
    /// $2000=$2400, $2800=$2C00: stacked screens (vertical scrolling games).
    Horizontal,
    /// Cartridge supplies 2 KB extra VRAM: four independent nametables.
    FourScreen,
}

/// A parsed ROM image: memory contents plus the board description needed to pick and
/// configure a mapper. Produced by [`Cartridge::from_file`] / [`Cartridge::from_ines_bytes`].
#[derive(Clone, Debug)]
pub struct Cartridge {
    /// Display title (file name without extension).
    pub title: String,
    /// Where the ROM was loaded from; the `.sav` file is stored next to it.
    pub rom_path: Option<PathBuf>,
    /// iNES / NES 2.0 mapper number (after any header fixups).
    pub mapper_id: u16,
    /// NES 2.0 submapper (e.g. 4.1 = MMC6, 4.3 = MC-ACC, 1.5 = fixed-PRG SEROM).
    pub submapper: u8,
    /// Program ROM (CPU $8000-$FFFF, banked by the mapper).
    pub prg_rom: Vec<u8>,
    /// CHR-ROM contents (empty when the board only has CHR-RAM).
    pub chr_rom: Vec<u8>,
    /// Bytes of CHR-RAM on the board (0 if it only has CHR-ROM).
    pub chr_ram_size: usize,
    /// Work RAM + battery-backed save RAM, presented as one contiguous block.
    pub prg_ram: Vec<u8>,
    /// Bytes at the start of `prg_ram` that are battery-backed.
    pub save_ram_size: usize,
    /// Nametable mirroring at power-on (solder-pad setting for fixed-mirroring boards).
    pub initial_mirroring: Mirroring,
    /// Whether PRG-RAM is battery-backed (saved to `<rom>.sav`).
    pub has_battery: bool,
    /// CRC32 of PRG+CHR (the usual cartridge database key).
    pub crc32: u32,
    /// Board uses the MMC3 revision A IRQ behaviour.
    pub mmc3_rev_a: bool,
    /// The header was in NES 2.0 format.
    pub is_nes2: bool,
}

/// Correction for a ROM whose iNES header disagrees with the real cartridge hardware.
/// `None` fields keep the header's value.
pub struct HeaderFixup {
    /// CRC32 of PRG-ROM + CHR-ROM (header and trainer excluded).
    pub crc: u32,
    /// Correct mapper number.
    pub mapper: Option<u16>,
    /// Correct submapper.
    pub submapper: Option<u8>,
    /// Correct battery flag.
    pub battery: Option<bool>,
    /// Correct hard-wired mirroring.
    pub mirroring: Option<Mirroring>,
    /// Board uses an MMC3 revision A (different IRQ-at-zero behaviour).
    pub mmc3a: bool,
    /// Battery-backed PRG-RAM size, if not 8 KB.
    pub save_ram_kb: Option<usize>,
    /// CHR-RAM size, if not 8 KB.
    pub chr_ram_kb: Option<usize>,
}

include!("cartridge_fixups.rs");

/// Board info for a known headerless dump.
pub struct HeaderlessRom {
    /// CRC32 of the entire headerless file.
    pub crc: u32,
    /// Mapper number.
    pub mapper: u16,
    /// Submapper number.
    pub submapper: u8,
    /// PRG-ROM size in KB.
    pub prg_kb: usize,
    /// CHR-ROM size in KB (0 = CHR-RAM only).
    pub chr_kb: usize,
    /// CHR-RAM size in KB.
    pub chr_ram_kb: usize,
    /// PRG-RAM (work or save) size in KB.
    pub prg_ram_kb: usize,
    /// Whether PRG-RAM is battery-backed.
    pub battery: bool,
    /// Hard-wired nametable mirroring.
    pub mirroring: Mirroring,
}

include!("cartridge_headerless.rs");

/// Builds an NES 2.0 image for a recognised headerless dump.
fn synthesize_header(raw: &[u8]) -> Option<Vec<u8>> {
    let mut crc = flate2::Crc::new();
    crc.update(raw);
    let info = HEADERLESS_ROMS.iter().find(|r| r.crc == crc.sum())?;
    let size_code = |kb: usize| -> u8 {
        if kb == 0 { 0 } else { ((kb * 1024).trailing_zeros() - 6) as u8 }
    };
    let mut h = vec![0u8; 16];
    h[0..4].copy_from_slice(b"NES\x1A");
    h[4] = (info.prg_kb / 16) as u8;
    h[5] = (info.chr_kb / 8) as u8;
    let mut f6 = ((info.mapper & 0x0F) << 4) as u8;
    match info.mirroring {
        Mirroring::Vertical => f6 |= 1,
        Mirroring::FourScreen => f6 |= 8,
        _ => {}
    }
    if info.battery {
        f6 |= 2;
    }
    h[6] = f6;
    h[7] = (info.mapper & 0xF0) as u8 | 0x08;
    h[8] = ((info.mapper >> 8) as u8 & 0x0F) | (info.submapper << 4);
    h[10] = if info.battery { size_code(info.prg_ram_kb.max(8)) << 4 } else { size_code(info.prg_ram_kb.max(8)) };
    h[11] = size_code(info.chr_ram_kb);
    h.extend_from_slice(raw);
    Some(h)
}

impl Cartridge {
    /// Loads either a raw `.nes` ROM file or a `.zip` archive containing a `.nes` ROM.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let path_ref = path.as_ref();
        let bytes = fs::read(path_ref)
            .map_err(|e| format!("Failed to read '{}': {}", path_ref.display(), e))?;

        let (nes_bytes, title) = if bytes.starts_with(b"PK\x03\x04") {
            let (extracted, inner_name) = match extract_nes_from_zip(&bytes, true) {
                Ok(r) => r,
                Err(e) => {
                    let (raw, name) = extract_nes_from_zip(&bytes, false)?;
                    (synthesize_header(&raw).ok_or(e)?, name)
                }
            };
            let stem = Path::new(&inner_name)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "NES ROM".to_string());
            (extracted, stem)
        } else {
            let stem = path_ref
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "NES ROM".to_string());
            (bytes, stem)
        };

        let nes_bytes = if nes_bytes.starts_with(b"NES\x1A") {
            nes_bytes
        } else {
            synthesize_header(&nes_bytes).ok_or_else(|| "Unrecognised headerless ROM (no iNES header)".to_string())?
        };
        let mut cart = Self::from_ines_bytes(&nes_bytes, title)?;
        cart.rom_path = Some(path_ref.to_path_buf());
        cart.load_battery_sram();
        Ok(cart)
    }

    /// Parses raw iNES 1.0 / NES 2.0 `.nes` image bytes.
    pub fn from_ines_bytes(bytes: &[u8], title: String) -> Result<Self, String> {
        if bytes.len() < 16 || &bytes[0..4] != b"NES\x1A" {
            return Err("Invalid iNES header (missing 'NES\\x1A' magic)".to_string());
        }

        let flags6 = bytes[6];
        let mut flags7 = bytes[7];
        let is_nes2 = (flags7 & 0x0C) == 0x08;
        // Old dumping tools ("DiskDude!" etc.) left junk in bytes 7-15. For non-NES-2.0
        // headers with garbage in the padding bytes, ignore the high mapper nibble.
        if !is_nes2 && bytes[12..16].iter().any(|&b| b != 0) {
            flags7 = 0;
        }

        let mut mapper_id = ((flags7 & 0xF0) | (flags6 >> 4)) as u16;
        let mut submapper = 0u8;
        let mut prg_size = bytes[4] as usize * 16384;
        let mut chr_size = bytes[5] as usize * 8192;
        let mut work_ram = 8192usize;
        let mut save_ram = 0usize;
        let mut chr_ram = if chr_size == 0 { 8192 } else { 0 };
        let mut has_battery = (flags6 & 0x02) != 0;

        if is_nes2 {
            mapper_id |= ((bytes[8] & 0x0F) as u16) << 8;
            submapper = bytes[8] >> 4;
            let prg_msb = (bytes[9] & 0x0F) as usize;
            let chr_msb = (bytes[9] >> 4) as usize;
            if prg_msb != 0x0F {
                prg_size = ((prg_msb << 8) | bytes[4] as usize) * 16384;
            }
            if chr_msb != 0x0F {
                chr_size = ((chr_msb << 8) | bytes[5] as usize) * 8192;
            }
            let shift = |v: u8| if v == 0 { 0 } else { 64usize << v };
            work_ram = shift(bytes[10] & 0x0F);
            save_ram = shift(bytes[10] >> 4);
            chr_ram = shift(bytes[11] & 0x0F) + shift(bytes[11] >> 4);
        }

        let has_trainer = (flags6 & 0x04) != 0;
        let mut initial_mirroring = if (flags6 & 0x08) != 0 {
            Mirroring::FourScreen
        } else if (flags6 & 0x01) != 0 {
            Mirroring::Vertical
        } else {
            Mirroring::Horizontal
        };

        let mut offset = 16usize;
        if has_trainer {
            offset += 512;
        }
        if prg_size == 0 || bytes.len() < offset + prg_size {
            return Err(format!(
                "Truncated PRG-ROM in '{}' (expected {} bytes at offset {}, file len {})",
                title,
                prg_size,
                offset,
                bytes.len()
            ));
        }
        let data_start = offset;
        let prg_rom = bytes[offset..offset + prg_size].to_vec();
        offset += prg_size;
        let chr_avail = bytes.len().saturating_sub(offset).min(chr_size);
        if chr_avail < chr_size {
            return Err(format!("Truncated CHR-ROM in '{}' (expected {} bytes)", title, chr_size));
        }
        let chr_rom = bytes[offset..offset + chr_size].to_vec();

        let mut crc = flate2::Crc::new();
        crc.update(&bytes[data_start..offset + chr_size]);
        let crc32 = crc.sum();

        let mut mmc3_rev_a = false;
        if let Some(fix) = HEADER_FIXUPS.iter().find(|f| f.crc == crc32) {
            if let Some(m) = fix.mapper {
                mapper_id = m;
            }
            if let Some(s) = fix.submapper {
                submapper = s;
            }
            if let Some(b) = fix.battery {
                has_battery = b;
            }
            if let Some(m) = fix.mirroring {
                initial_mirroring = m;
            }
            if let Some(kb) = fix.save_ram_kb {
                save_ram = kb * 1024;
                work_ram = 0;
            }
            if let Some(kb) = fix.chr_ram_kb {
                chr_ram = kb * 1024;
            }
            mmc3_rev_a = fix.mmc3a;
        }

        // Four-screen is only meaningful for boards that actually carry the extra VRAM.
        if initial_mirroring == Mirroring::FourScreen && !matches!(mapper_id, 4 | 118 | 119 | 206) {
            initial_mirroring = if (flags6 & 0x01) != 0 { Mirroring::Vertical } else { Mirroring::Horizontal };
        }

        if !is_nes2 {
            if has_battery && save_ram == 0 {
                save_ram = work_ram.max(8192);
                work_ram = 0;
            }
            // MMC5 boards: give enough RAM for every licensed configuration.
            if mapper_id == 5 && save_ram + work_ram < 0x10000 {
                if has_battery {
                    save_ram = save_ram.max(0x8000);
                } else {
                    work_ram = 0x10000;
                }
            }
            // MMC6 has 1 KB of internal battery-backed RAM.
            if mapper_id == 4 && submapper == 1 {
                save_ram = 1024;
                work_ram = 0;
            }
        }
        let save_ram_size = save_ram;
        let total_ram = (work_ram + save_ram).max(if mapper_id == 5 { 0 } else { 8192 });

        Ok(Self {
            title,
            rom_path: None,
            mapper_id,
            submapper,
            prg_rom,
            chr_rom,
            chr_ram_size: chr_ram,
            prg_ram: vec![0u8; total_ram.max(1024)],
            save_ram_size,
            initial_mirroring,
            has_battery,
            crc32,
            mmc3_rev_a,
            is_nes2,
        })
    }

    /// Path of the battery save file: the ROM path with a `.sav` extension.
    pub fn sav_path(&self) -> Option<PathBuf> {
        self.rom_path.as_ref().map(|p| p.with_extension("sav"))
    }

    /// Loads `<rom>.sav` into PRG-RAM if the board has a battery and the file exists.
    pub fn load_battery_sram(&mut self) {
        if !self.has_battery {
            return;
        }
        if let Some(sav) = self.sav_path()
            && let Ok(data) = fs::read(&sav)
        {
            let len = data.len().min(self.prg_ram.len());
            self.prg_ram[..len].copy_from_slice(&data[..len]);
        }
    }

    /// Writes the battery-backed part of `prg_ram` to `<rom>.sav` (ignored without a battery).
    pub fn save_battery_sram(&self, prg_ram: &[u8]) {
        if !self.has_battery {
            return;
        }
        if let Some(sav) = self.sav_path() {
            let len = if self.save_ram_size > 0 { self.save_ram_size.min(prg_ram.len()) } else { prg_ram.len() };
            let _ = fs::write(sav, &prg_ram[..len]);
        }
    }
}

/// Reads a little-endian `u16` at byte offset `off` (panics if out of bounds).
#[inline]
fn read_u16_le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

/// Reads a little-endian `u32` at byte offset `off` (panics if out of bounds).
#[inline]
fn read_u32_le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Extracts the first `.nes` file (or valid `NES\x1A` stream) from a `.zip` byte buffer.
pub fn extract_nes_from_zip(zip_bytes: &[u8], require_header: bool) -> Result<(Vec<u8>, String), String> {
    if zip_bytes.len() < 22 {
        return Err("ZIP archive too small".to_string());
    }

    // 1. Locate End of Central Directory (EOCD: PK\x05\x06) searching backwards from EOF
    let min_eocd = zip_bytes.len().saturating_sub(65557);
    let mut eocd_pos = None;
    for pos in (min_eocd..=zip_bytes.len() - 22).rev() {
        if &zip_bytes[pos..pos + 4] == b"PK\x05\x06" {
            eocd_pos = Some(pos);
            break;
        }
    }

    if let Some(eocd) = eocd_pos {
        let total_entries = read_u16_le(zip_bytes, eocd + 10) as usize;
        let mut cd_offset = read_u32_le(zip_bytes, eocd + 16) as usize;

        for _ in 0..total_entries {
            if cd_offset + 46 > zip_bytes.len()
                || &zip_bytes[cd_offset..cd_offset + 4] != b"PK\x01\x02"
            {
                break;
            }
            let method = read_u16_le(zip_bytes, cd_offset + 10);
            let comp_size = read_u32_le(zip_bytes, cd_offset + 20) as usize;
            let uncomp_size = read_u32_le(zip_bytes, cd_offset + 24) as usize;
            let name_len = read_u16_le(zip_bytes, cd_offset + 28) as usize;
            let extra_len = read_u16_le(zip_bytes, cd_offset + 30) as usize;
            let comment_len = read_u16_le(zip_bytes, cd_offset + 32) as usize;
            let local_hdr_off = read_u32_le(zip_bytes, cd_offset + 42) as usize;

            if cd_offset + 46 + name_len > zip_bytes.len() {
                break;
            }
            let name =
                String::from_utf8_lossy(&zip_bytes[cd_offset + 46..cd_offset + 46 + name_len])
                    .into_owned();

            if (name.to_lowercase().ends_with(".nes") || total_entries == 1)
                && local_hdr_off + 30 <= zip_bytes.len()
                && &zip_bytes[local_hdr_off..local_hdr_off + 4] == b"PK\x03\x04"
            {
                let l_name_len = read_u16_le(zip_bytes, local_hdr_off + 26) as usize;
                let l_extra_len = read_u16_le(zip_bytes, local_hdr_off + 28) as usize;
                let data_start = local_hdr_off + 30 + l_name_len + l_extra_len;
                let data_end = data_start + comp_size;

                if data_end <= zip_bytes.len() {
                    let comp_slice = &zip_bytes[data_start..data_end];
                    let out = decompress_zip_entry(method, comp_slice, uncomp_size)?;
                    if !require_header || out.starts_with(b"NES\x1A") {
                        return Ok((out, name));
                    }
                }
            }

            cd_offset += 46 + name_len + extra_len + comment_len;
        }
    }

    // 2. Fallback: parse first Local File Header (PK\x03\x04) directly
    if zip_bytes.len() >= 30 && &zip_bytes[0..4] == b"PK\x03\x04" {
        let method = read_u16_le(zip_bytes, 8);
        let comp_size = read_u32_le(zip_bytes, 18) as usize;
        let uncomp_size = read_u32_le(zip_bytes, 22) as usize;
        let name_len = read_u16_le(zip_bytes, 26) as usize;
        let extra_len = read_u16_le(zip_bytes, 28) as usize;
        let data_start = 30 + name_len + extra_len;
        if data_start + comp_size <= zip_bytes.len() {
            let name = String::from_utf8_lossy(&zip_bytes[30..30 + name_len]).into_owned();
            let out = decompress_zip_entry(
                method,
                &zip_bytes[data_start..data_start + comp_size],
                uncomp_size,
            )?;
            if !require_header || out.starts_with(b"NES\x1A") {
                return Ok((out, name));
            }
        }
    }

    Err("No valid .nes ROM found inside .zip archive".to_string())
}

/// Decompresses one ZIP entry: method 0 (stored) is copied verbatim, method 8 (raw
/// Deflate) is inflated; any other method is an error. `uncomp_size` is only a capacity hint.
fn decompress_zip_entry(
    method: u16,
    comp_slice: &[u8],
    uncomp_size: usize,
) -> Result<Vec<u8>, String> {
    match method {
        0 => Ok(comp_slice.to_vec()),
        8 => {
            let mut decoder = DeflateDecoder::new(comp_slice);
            let mut out = Vec::with_capacity(uncomp_size.max(32768));
            decoder
                .read_to_end(&mut out)
                .map_err(|e| format!("ZIP Deflate decompression error: {}", e))?;
            Ok(out)
        }
        other => Err(format!("Unsupported ZIP compression method {}", other)),
    }
}
