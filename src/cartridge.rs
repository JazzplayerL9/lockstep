//! iNES 1.0 / NES 2.0 ROM loader, .zip archive extractor, header parser,
//! and battery-backed SRAM persistence.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use flate2::read::DeflateDecoder;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mirroring {
    OneScreenLower,
    OneScreenUpper,
    Vertical,
    Horizontal,
    /// Cartridge supplies 2 KB extra VRAM: four independent nametables.
    FourScreen,
    /// Per-nametable CIRAM page selection (TxSROM / mapper 118).
    Custom([u8; 4]),
}

#[derive(Clone, Debug)]
pub struct Cartridge {
    pub title: String,
    pub rom_path: Option<PathBuf>,
    pub mapper_id: u8,
    pub prg_rom: Vec<u8>,
    pub chr_mem: Vec<u8>,
    pub chr_is_ram: bool,
    pub prg_ram: Vec<u8>,
    pub initial_mirroring: Mirroring,
    pub has_battery: bool,
}

impl Cartridge {
    /// Loads either a raw `.nes` ROM file or a `.zip` archive containing a `.nes` ROM.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let path_ref = path.as_ref();
        let bytes = fs::read(path_ref)
            .map_err(|e| format!("Failed to read '{}': {}", path_ref.display(), e))?;

        let (nes_bytes, title) = if bytes.starts_with(b"PK\x03\x04") {
            let (extracted, inner_name) = extract_nes_from_zip(&bytes)?;
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

        let prg_banks_16k = bytes[4] as usize;
        let chr_banks_8k = bytes[5] as usize;
        let flags6 = bytes[6];
        let mut flags7 = bytes[7];
        // Old dumping tools ("DiskDude!" etc.) left junk in bytes 7-15. For non-NES-2.0
        // headers with garbage in the padding bytes, ignore the high mapper nibble.
        let is_nes2 = (flags7 & 0x0C) == 0x08;
        if !is_nes2 && (bytes[7..16].windows(8).any(|w| w == b"DiskDude") || bytes[12..16].iter().any(|&b| b != 0)) {
            flags7 = 0;
        }

        let has_trainer = (flags6 & 0x04) != 0;
        let has_battery = (flags6 & 0x02) != 0;
        let initial_mirroring = if (flags6 & 0x01) != 0 {
            Mirroring::Vertical
        } else {
            Mirroring::Horizontal
        };

        let mapper_id = (flags7 & 0xF0) | (flags6 >> 4);
        // Four-screen VRAM is only honoured for MMC3 boards (Gauntlet, Rad Racer II);
        // other mappers with bit 3 set are almost always bad headers.
        let initial_mirroring = if (flags6 & 0x08) != 0 && mapper_id == 4 {
            Mirroring::FourScreen
        } else {
            initial_mirroring
        };

        let mut offset = 16usize;
        if has_trainer {
            offset += 512;
        }

        let prg_size = prg_banks_16k * 16384;
        if prg_size == 0 || bytes.len() < offset + prg_size {
            return Err(format!(
                "Truncated PRG-ROM in '{}' (expected {} bytes at offset {}, file len {})",
                title,
                prg_size,
                offset,
                bytes.len()
            ));
        }
        let prg_rom = bytes[offset..offset + prg_size].to_vec();
        offset += prg_size;

        let (chr_mem, chr_is_ram) = if chr_banks_8k == 0 {
            (vec![0u8; 8192], true)
        } else {
            let chr_size = chr_banks_8k * 8192;
            if bytes.len() < offset + chr_size {
                return Err(format!(
                    "Truncated CHR-ROM in '{}' (expected {} bytes)",
                    title, chr_size
                ));
            }
            (bytes[offset..offset + chr_size].to_vec(), false)
        };

        Ok(Self {
            title,
            rom_path: None,
            mapper_id,
            prg_rom,
            chr_mem,
            chr_is_ram,
            prg_ram: vec![0u8; 8192],
            initial_mirroring,
            has_battery,
        })
    }

    pub fn sav_path(&self) -> Option<PathBuf> {
        self.rom_path.as_ref().map(|p| p.with_extension("sav"))
    }

    pub fn load_battery_sram(&mut self) {
        if !self.has_battery {
            return;
        }
        if let Some(sav) = self.sav_path()
            && let Ok(data) = fs::read(&sav) {
                let len = data.len().min(self.prg_ram.len());
                self.prg_ram[..len].copy_from_slice(&data[..len]);
            }
    }

    pub fn save_battery_sram(&self, prg_ram: &[u8]) {
        if !self.has_battery {
            return;
        }
        if let Some(sav) = self.sav_path() {
            let _ = fs::write(sav, prg_ram);
        }
    }
}

#[inline]
fn read_u16_le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

#[inline]
fn read_u32_le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Extracts the first `.nes` file (or valid `NES\x1A` stream) from a `.zip` byte buffer.
pub fn extract_nes_from_zip(zip_bytes: &[u8]) -> Result<(Vec<u8>, String), String> {
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
                    if out.starts_with(b"NES\x1A") {
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
            if out.starts_with(b"NES\x1A") {
                return Ok((out, name));
            }
        }
    }

    Err("No valid .nes ROM found inside .zip archive".to_string())
}

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
