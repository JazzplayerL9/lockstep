// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones
//
// Portions derived from Mesen (https://github.com/nesdev-org/MesenCE):
// cartridge data from the Mesen game database.
// Mesen is Copyright (C) 2014-2026 Sour and contributors, GPL-3.0-or-later.
// (Mesen's database is itself compiled from NesCartDB, Nestopia's database and
// NewRisingSun's NES 2.0 header database.)

// Board descriptions for headerless (raw PRG+CHR) dumps in the local library, keyed by CRC32.
/// Known headerless dumps, matched by [`HeaderlessRom::crc`] of the whole file.
pub static HEADERLESS_ROMS: &[HeaderlessRom] = &[
    HeaderlessRom { crc: 0x063E5653, mapper: 1, submapper: 0, prg_kb: 128, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Addams Family, The - Pugsley's Scavenger Hunt (USA)
    HeaderlessRom { crc: 0x71BF075F, mapper: 1, submapper: 5, prg_kb: 32, chr_kb: 32, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Adventures of Lolo (USA)
    HeaderlessRom { crc: 0xC740EB46, mapper: 7, submapper: 2, prg_kb: 128, chr_kb: 0, chr_ram_kb: 8, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Arch Rivals - A Basket Brawl! (USA)
    HeaderlessRom { crc: 0xB4241FCC, mapper: 1, submapper: 0, prg_kb: 256, chr_kb: 64, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Bases Loaded (USA)
    HeaderlessRom { crc: 0x3ECA3DDA, mapper: 4, submapper: 0, prg_kb: 256, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Bases Loaded 3 (USA)
    HeaderlessRom { crc: 0x50CCC8ED, mapper: 3, submapper: 2, prg_kb: 32, chr_kb: 32, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Battleship (USA)
    HeaderlessRom { crc: 0xC42E648A, mapper: 1, submapper: 0, prg_kb: 128, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Bigfoot (USA)
    HeaderlessRom { crc: 0xA8F4D99E, mapper: 1, submapper: 5, prg_kb: 32, chr_kb: 32, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Boulder Dash (USA)
    HeaderlessRom { crc: 0x4D1AC58C, mapper: 1, submapper: 0, prg_kb: 128, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Boy and His Blob, A - Trouble on Blobolonia (USA)
    HeaderlessRom { crc: 0xBDF046EF, mapper: 7, submapper: 2, prg_kb: 128, chr_kb: 0, chr_ram_kb: 8, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Cabal (USA)
    HeaderlessRom { crc: 0xD1EA84C3, mapper: 1, submapper: 0, prg_kb: 128, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Caveman Games (USA)
    HeaderlessRom { crc: 0xA3C0D49F, mapper: 2, submapper: 2, prg_kb: 128, chr_kb: 0, chr_ram_kb: 8, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Vertical }, // Classic Concentration (USA)
    HeaderlessRom { crc: 0x4681691A, mapper: 1, submapper: 0, prg_kb: 128, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Demon Sword - Release the Power (USA)
    HeaderlessRom { crc: 0x13C774DD, mapper: 4, submapper: 0, prg_kb: 128, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Double Dragon II - The Revenge (USA)
    HeaderlessRom { crc: 0xAF05F37E, mapper: 4, submapper: 3, prg_kb: 128, chr_kb: 256, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // George Foreman's KO Boxing (USA)
    HeaderlessRom { crc: 0x383CABBF, mapper: 119, submapper: 0, prg_kb: 128, chr_kb: 64, chr_ram_kb: 8, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // High Speed (USA)
    HeaderlessRom { crc: 0x3869E598, mapper: 2, submapper: 2, prg_kb: 128, chr_kb: 0, chr_ram_kb: 8, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Vertical }, // Hollywood Squares (USA)
    HeaderlessRom { crc: 0xCA5EDBFC, mapper: 4, submapper: 0, prg_kb: 128, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 8, battery: false, mirroring: Mirroring::Horizontal }, // Home Alone (USA)
    HeaderlessRom { crc: 0xA0C31A57, mapper: 4, submapper: 0, prg_kb: 128, chr_kb: 64, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Indiana Jones and the Temple of Doom (USA)
    HeaderlessRom { crc: 0xDF64963B, mapper: 4, submapper: 0, prg_kb: 128, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Infiltrator (USA)
    HeaderlessRom { crc: 0xB5D10D5C, mapper: 7, submapper: 1, prg_kb: 128, chr_kb: 0, chr_ram_kb: 8, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Jeopardy! (USA)
    HeaderlessRom { crc: 0xBE387AF0, mapper: 3, submapper: 2, prg_kb: 16, chr_kb: 16, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Joust (USA)
    HeaderlessRom { crc: 0xACA145D8, mapper: 3, submapper: 2, prg_kb: 32, chr_kb: 32, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Karate Champ (USA)
    HeaderlessRom { crc: 0xD9F0749F, mapper: 1, submapper: 0, prg_kb: 128, chr_kb: 0, chr_ram_kb: 8, prg_ram_kb: 8, battery: false, mirroring: Mirroring::Horizontal }, // Kid Icarus - Angel Land Story (Europe, USA)
    HeaderlessRom { crc: 0xAA74A4D8, mapper: 3, submapper: 2, prg_kb: 32, chr_kb: 32, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Kung-Fu Heroes (USA)
    HeaderlessRom { crc: 0xBBED6E6E, mapper: 3, submapper: 2, prg_kb: 32, chr_kb: 16, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Legend of Kage, The (USA)
    HeaderlessRom { crc: 0x8FF31896, mapper: 0, submapper: 0, prg_kb: 16, chr_kb: 8, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Vertical }, // M.U.S.C.L.E. - Tag Team Match (USA)
    HeaderlessRom { crc: 0xC6C2EDB5, mapper: 1, submapper: 0, prg_kb: 64, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Magic Johnson's Fast Break (USA)
    HeaderlessRom { crc: 0x12078AFD, mapper: 4, submapper: 0, prg_kb: 128, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Mendel Palace (USA)
    HeaderlessRom { crc: 0x817431EC, mapper: 2, submapper: 2, prg_kb: 128, chr_kb: 0, chr_ram_kb: 8, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Vertical }, // Metal Gear (USA)
    HeaderlessRom { crc: 0xE3C5BB3D, mapper: 4, submapper: 0, prg_kb: 128, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Mission Impossible (USA)
    HeaderlessRom { crc: 0xAE9F33D0, mapper: 4, submapper: 0, prg_kb: 128, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 8, battery: false, mirroring: Mirroring::Horizontal }, // North & South (USA)
    HeaderlessRom { crc: 0x20353E63, mapper: 1, submapper: 0, prg_kb: 128, chr_kb: 64, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Peter Pan & The Pirates - The Revenge of Captain Hook (USA)
    HeaderlessRom { crc: 0x63FCC0DD, mapper: 1, submapper: 0, prg_kb: 128, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Rescue - The Embassy Mission (USA)
    HeaderlessRom { crc: 0xB4CDF95F, mapper: 1, submapper: 0, prg_kb: 128, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // RoboCop 2 (USA)
    HeaderlessRom { crc: 0xB3783F2A, mapper: 2, submapper: 2, prg_kb: 128, chr_kb: 0, chr_ram_kb: 8, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Vertical }, // Rygar (USA)
    HeaderlessRom { crc: 0x5CF6A82E, mapper: 1, submapper: 0, prg_kb: 128, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Sesame Street Countdown (USA)
    HeaderlessRom { crc: 0x7416903F, mapper: 4, submapper: 3, prg_kb: 256, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Simpsons, The - Bart vs. the World (USA)
    HeaderlessRom { crc: 0x3322105A, mapper: 1, submapper: 5, prg_kb: 32, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Sky Kid (USA)
    HeaderlessRom { crc: 0xFDF4569B, mapper: 1, submapper: 5, prg_kb: 32, chr_kb: 32, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Snake Rattle n Roll (USA)
    HeaderlessRom { crc: 0x1DAC6208, mapper: 1, submapper: 0, prg_kb: 128, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Snow Brothers (USA)
    HeaderlessRom { crc: 0x40684E95, mapper: 3, submapper: 2, prg_kb: 32, chr_kb: 32, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Solomon's Key (USA)
    HeaderlessRom { crc: 0x529B621F, mapper: 1, submapper: 0, prg_kb: 128, chr_kb: 64, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Super Mario Bros. + Duck Hunt + World Class Track Meet (USA)
    HeaderlessRom { crc: 0xEA27B477, mapper: 4, submapper: 3, prg_kb: 128, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Terminator 2 - Judgment Day (USA)
    HeaderlessRom { crc: 0x2A46B57F, mapper: 2, submapper: 2, prg_kb: 128, chr_kb: 0, chr_ram_kb: 8, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Terra Cresta (USA)
    HeaderlessRom { crc: 0x9C537919, mapper: 4, submapper: 0, prg_kb: 128, chr_kb: 128, chr_ram_kb: 0, prg_ram_kb: 8, battery: false, mirroring: Mirroring::Horizontal }, // Tetris 2 (USA)
    HeaderlessRom { crc: 0xD80B44BC, mapper: 66, submapper: 0, prg_kb: 128, chr_kb: 32, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Thunder & Lightning (USA)
    HeaderlessRom { crc: 0x52880295, mapper: 1, submapper: 0, prg_kb: 128, chr_kb: 0, chr_ram_kb: 8, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Winter Games (USA)
    HeaderlessRom { crc: 0x505F9715, mapper: 7, submapper: 1, prg_kb: 128, chr_kb: 0, chr_ram_kb: 8, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Wizards & Warriors (USA)
    HeaderlessRom { crc: 0x5734EB9E, mapper: 3, submapper: 2, prg_kb: 32, chr_kb: 32, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // World Class Track Meet (USA)
    HeaderlessRom { crc: 0xE7EAD93B, mapper: 1, submapper: 0, prg_kb: 128, chr_kb: 32, chr_ram_kb: 0, prg_ram_kb: 0, battery: false, mirroring: Mirroring::Horizontal }, // Yoshi (USA)
];
