// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones
//
// Portions derived from Mesen (https://github.com/nesdev-org/MesenCE):
// cartridge data from the Mesen game database.
// Mesen is Copyright (C) 2014-2026 Sour and contributors, GPL-3.0-or-later.
// (Mesen's database is itself compiled from NesCartDB, Nestopia's database and
// NewRisingSun's NES 2.0 header database.)

// Header corrections keyed by PRG+CHR CRC32, for ROMs in the local library whose
// iNES headers disagree with known cartridge hardware.
/// Header corrections, matched by [`HeaderFixup::crc`].
pub static HEADER_FIXUPS: &[HeaderFixup] = &[
    HeaderFixup { crc: 0x326AB3B6, mapper: None, submapper: None, battery: None, mirroring: None, mmc3a: true, save_ram_kb: None, chr_ram_kb: None }, // 8 Eyes (USA)
    HeaderFixup { crc: 0xC527C297, mapper: None, submapper: Some(3), battery: None, mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Alien 3 (USA)
    HeaderFixup { crc: 0xF304F1B9, mapper: None, submapper: None, battery: None, mirroring: Some(Mirroring::Vertical), mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Archon (USA)
    HeaderFixup { crc: 0x1F6EA423, mapper: None, submapper: None, battery: Some(true), mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Baseball Simulator 1.000 (USA)
    HeaderFixup { crc: 0xEAC38105, mapper: None, submapper: None, battery: None, mirroring: Some(Mirroring::Vertical), mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Championship Bowling (USA)
    HeaderFixup { crc: 0xEB9960EE, mapper: None, submapper: None, battery: None, mirroring: Some(Mirroring::Vertical), mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Destination Earthstar (USA)
    HeaderFixup { crc: 0xE7D2C49D, mapper: None, submapper: None, battery: None, mirroring: Some(Mirroring::Horizontal), mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Golf (USA)
    HeaderFixup { crc: 0xA80A0F01, mapper: None, submapper: Some(3), battery: None, mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Incredible Crash Dummies, The (USA)
    HeaderFixup { crc: 0x00E95D86, mapper: None, submapper: None, battery: None, mirroring: Some(Mirroring::Vertical), mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Jimmy Connors Tennis (USA)
    HeaderFixup { crc: 0x548A2C3C, mapper: Some(206), submapper: None, battery: None, mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Karnov (USA)
    HeaderFixup { crc: 0x00837960, mapper: None, submapper: None, battery: Some(false), mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // King's Quest V (USA)
    HeaderFixup { crc: 0xF181C021, mapper: None, submapper: None, battery: None, mirroring: None, mmc3a: true, save_ram_kb: None, chr_ram_kb: None }, // Legacy of the Wizard (USA)
    HeaderFixup { crc: 0xFD8D6C75, mapper: None, submapper: None, battery: None, mirroring: Some(Mirroring::Vertical), mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Loopz (USA)
    HeaderFixup { crc: 0x59977A46, mapper: None, submapper: None, battery: None, mirroring: Some(Mirroring::Vertical), mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Mach Rider (Japan, USA)
    HeaderFixup { crc: 0x83FC38F8, mapper: None, submapper: None, battery: None, mirroring: None, mmc3a: true, save_ram_kb: None, chr_ram_kb: None }, // Mappy-Land (USA)
    HeaderFixup { crc: 0xC4C3949A, mapper: None, submapper: None, battery: None, mirroring: Some(Mirroring::Horizontal), mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Mario Bros. (Japan, USA)
    HeaderFixup { crc: 0x982DFB38, mapper: None, submapper: Some(3), battery: None, mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Mickey's Safari in Letterland (USA)
    HeaderFixup { crc: 0xAE52DECE, mapper: None, submapper: None, battery: None, mirroring: Some(Mirroring::Vertical), mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Millipede (USA)
    HeaderFixup { crc: 0xB6D2D300, mapper: None, submapper: None, battery: None, mirroring: Some(Mirroring::Vertical), mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Ms. Pac-Man (USA)
    HeaderFixup { crc: 0x0B0E128F, mapper: None, submapper: None, battery: Some(false), mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Nintendo World Championships 1990 (USA)
    HeaderFixup { crc: 0x2856111F, mapper: None, submapper: None, battery: Some(true), mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Overlord (USA)
    HeaderFixup { crc: 0xD19ADDEB, mapper: None, submapper: None, battery: None, mirroring: None, mmc3a: true, save_ram_kb: None, chr_ram_kb: None }, // Pin-Bot (USA)
    HeaderFixup { crc: 0xEB61133B, mapper: None, submapper: None, battery: None, mirroring: Some(Mirroring::Horizontal), mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Puzznic (USA)
    HeaderFixup { crc: 0xC0B23520, mapper: None, submapper: None, battery: None, mirroring: Some(Mirroring::Vertical), mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Q-bert (USA)
    HeaderFixup { crc: 0x263AC8A0, mapper: None, submapper: None, battery: None, mirroring: None, mmc3a: true, save_ram_kb: None, chr_ram_kb: None }, // Rampage (USA)
    HeaderFixup { crc: 0x5BB62688, mapper: Some(206), submapper: None, battery: None, mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Ring King (USA)
    HeaderFixup { crc: 0x018A8699, mapper: None, submapper: Some(3), battery: None, mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Roger Clemens' MVP Baseball (USA)
    HeaderFixup { crc: 0x2370C0A9, mapper: None, submapper: Some(3), battery: None, mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Rollerblade Racer (USA)
    HeaderFixup { crc: 0xF011E490, mapper: None, submapper: None, battery: None, mirroring: None, mmc3a: false, save_ram_kb: Some(32), chr_ram_kb: None }, // Romance of the Three Kingdoms II (USA)
    HeaderFixup { crc: 0xBE3BF3B3, mapper: None, submapper: None, battery: Some(true), mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Shingen the Ruler (USA)
    HeaderFixup { crc: 0x5991B9D0, mapper: None, submapper: Some(3), battery: None, mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Simpsons, The - Bartman Meets Radioactive Man (USA)
    HeaderFixup { crc: 0xD679627A, mapper: None, submapper: Some(3), battery: None, mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Spider-Man - Return of the Sinister Six (USA)
    HeaderFixup { crc: 0xC7197FB1, mapper: None, submapper: None, battery: None, mirroring: Some(Mirroring::Horizontal), mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Spy Hunter (USA)
    HeaderFixup { crc: 0x889129CB, mapper: None, submapper: Some(1), battery: None, mirroring: None, mmc3a: false, save_ram_kb: Some(1), chr_ram_kb: None }, // Startropics (USA)
    HeaderFixup { crc: 0xD054FFB0, mapper: None, submapper: Some(1), battery: None, mirroring: None, mmc3a: false, save_ram_kb: Some(1), chr_ram_kb: None }, // Startropics 2 - Zoda's Revenge (USA)
    HeaderFixup { crc: 0x419461D0, mapper: None, submapper: None, battery: None, mirroring: Some(Mirroring::Vertical), mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Super Cars (USA)
    HeaderFixup { crc: 0x57AC67AF, mapper: None, submapper: None, battery: None, mirroring: None, mmc3a: true, save_ram_kb: None, chr_ram_kb: None }, // Super Mario Bros. 2 (USA)
    HeaderFixup { crc: 0xE840FD21, mapper: None, submapper: None, battery: None, mirroring: None, mmc3a: true, save_ram_kb: None, chr_ram_kb: None }, // Super Spike V'Ball (USA)
    HeaderFixup { crc: 0xD74B2719, mapper: None, submapper: None, battery: None, mirroring: Some(Mirroring::Vertical), mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Super Team Games (USA)
    HeaderFixup { crc: 0x7E57FBEC, mapper: None, submapper: Some(3), battery: None, mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // T&C 2 - Thrilla's Surfari (USA)
    HeaderFixup { crc: 0xDE8FD935, mapper: None, submapper: None, battery: None, mirroring: None, mmc3a: true, save_ram_kb: None, chr_ram_kb: None }, // To the Earth (USA)
    HeaderFixup { crc: 0x5E66EAEA, mapper: None, submapper: None, battery: None, mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: Some(16) }, // Videomation (USA)
    HeaderFixup { crc: 0x7B4ED0BB, mapper: None, submapper: Some(3), battery: None, mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // WWF King of the Ring (USA)
    HeaderFixup { crc: 0xD4611B79, mapper: None, submapper: Some(3), battery: None, mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // WWF Wrestlemania Steel Cage Challenge (USA)
    HeaderFixup { crc: 0xF79A75D7, mapper: None, submapper: None, battery: Some(true), mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Wario's Woods (USA)
];
