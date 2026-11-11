# Cartridge boards (mappers)

NES cartridges contain more than ROM. Most also carry bank-switching logic, extra RAM,
and sometimes IRQ timers or scanline counters. Emulators identify the board by its
**iNES mapper number** (plus an optional NES 2.0 *submapper*). This page lists what
Lockstep supports, how header problems are corrected, and how to add a board.

- [Supported boards](#supported-boards)
- [Board notes](#board-notes)
- [Header fixups](#header-fixups)
- [Adding a mapper](#adding-a-mapper)

---

## Supported boards

"Library games" is how many of the 661 USA releases in the development test library use
the board (see [ACCURACY.md](ACCURACY.md#game-compatibility-testing)).

| Mapper | Board(s) | Source | Library games | Examples |
|---|---|---|---|---|
| 0 | NROM | `nrom.rs` | 43 | Super Mario Bros., Excitebike, Ice Climber |
| 1 | MMC1 (SxROM: SLROM, SNROM, SUROM, SOROM, SXROM, SEROM…) | `mmc1.rs` | 216 | Zelda, Metroid, Mega Man 2, Final Fantasy, Dragon Warrior III/IV (SUROM) |
| 2 | UxROM (UNROM, UOROM) | `uxrom.rs` | 89 | Mega Man, Castlevania, Contra, DuckTales |
| 3 | CNROM | `cnrom.rs` | 59 | Gradius, Arkanoid, Paperboy |
| 4 | MMC3 (TxROM), rev B and rev A | `mmc3.rs` | 182 | Super Mario Bros. 2 & 3, Kirby's Adventure, Mega Man 3–6 |
| 4.1 | MMC6 (HKROM) | `mmc3.rs` | 2 | StarTropics, StarTropics 2 |
| 4.3 | Acclaim MC-ACC | `mmc3.rs` | 13 | Alien 3, Spider-Man: Return of the Sinister Six, WWF King of the Ring |
| 5 | MMC5 (ExROM) | `mmc5.rs` | 8 | Castlevania III, Uncharted Waters, Romance of the Three Kingdoms II |
| 7 | AxROM (ANROM, AOROM, AMROM) | `axrom.rs` | 32 | Battletoads, Marble Madness, R.C. Pro-Am |
| 9 | MMC2 (PxROM) | `mmc2.rs` | 1 | Mike Tyson's Punch-Out!! |
| 10 | MMC4 (FxROM) | `mmc2.rs` | 0 | Fire Emblem (JP) — *untested* |
| 11 | Color Dreams | `gxrom.rs` | 0 | Crystal Mines — *untested* |
| 13 | CPROM | `cprom.rs` | 1 | Videomation |
| 19 | Namco 163 / 129 | `n163.rs` | 0 | Megami Tensei II, Rolling Thunder (JP), Splatterhouse (JP), Erika to Satoru |
| 24, 26 | Konami VRC6a / VRC6b | `vrc6.rs` | 0 | Akumajou Densetsu (Castlevania III JP), Madara, Esper Dream 2 |
| 34 | BNROM / NINA-001 | `gxrom.rs` | 1 | Deadly Towers |
| 47 | NES-QJ (MMC3 multicart) | `mmc3.rs` | 1 | Super Spike V'Ball + Nintendo World Cup |
| 66 | GxROM / MxROM | `gxrom.rs` | 4 | Super Mario Bros. + Duck Hunt, Gumshoe, Dragon Power |
| 69 | Sunsoft FME-7 | `fme7.rs` | 1 | Batman: Return of the Joker |
| 71 | Camerica / Codemasters BF909x | `uxrom.rs` | 0 | Micro Machines, Fire Hawk — *untested* |
| 79 | AVE NINA-03/06 | `gxrom.rs` | 0 | Deathbots — *untested* |
| 105 | NES-EVENT | `mmc1.rs` | 1 | Nintendo World Championships 1990 |
| 118 | TxSROM (TKSROM, TLSROM) | `mmc3.rs` | 3 | NES Play Action Football, Goal! Two, Pro Sport Hockey |
| 119 | TQROM | `mmc3.rs` | 2 | Pin-Bot, High Speed |
| 206 | Namco 108 / DxROM | `mmc3.rs` | 2 | Karnov, Ring King |

Loading a ROM with any other mapper number shows "Unsupported mapper N" in the ROM
browser instead of silently mis-running it.

## Board notes

**MMC1.** The serial 5-write register interface. A write on the CPU cycle immediately
after another write is ignored, as the hardware ignores the second half of a
read-modify-write instruction's double write. Bit 7 resets the shift register. 512 KB
SUROM boards use CHR register bit 4 as the PRG outer bank. SOROM/SXROM bank 16/32 KB of
PRG-RAM through the CHR registers. Submapper 5 (SEROM/SHROM/SH1ROM) has fixed 32 KB PRG.
PRG-RAM can be disabled through bit 4 of the PRG register.

**NES-EVENT (105).** MMC1 plus a countdown timer that raises an IRQ when the competition
time expires. It boots through a fixed 32 KB bank until the program toggles CHR register
bit 4, then switches to the second 128 KB chip.

**MMC3.** Eight bank registers with PRG and CHR inversion modes, PRG-RAM enable/protect,
and the **scanline IRQ**: an 8-bit counter clocked by rising edges of PPU address line
A12 after A12 has been low for at least 3 CPU cycles. This filter rejects the rapid
toggling within a single sprite fetch. Revision B fires whenever the counter is zero after
clocking. Revision A, enabled by the fixup table for boards known to use it, only fires
on the transition to zero.
- *MMC6*: 1 KB of internal RAM at `$7000-$7FFF` (mirrored), split into two 512-byte halves
  with independent read/write enables.
- *MC-ACC*: clocks on **falling** A12 edges, once per 8 edges.
- *TxSROM (118)*: CHR register bit 7 selects the nametable page per 1 KB, replacing
  `$A000` mirroring.
- *TQROM (119)*: CHR banks `$40-$7F` select 8 KB of CHR-RAM instead of CHR-ROM.
- *NES-QJ (47)*: a `$6000-$7FFF` write selects one of two 128 KB PRG / 128 KB CHR halves.
- *Namco 108 (206)*: the MMC3's ancestor: no IRQ, no inversion modes, fixed mirroring.

**MMC5.** Four PRG modes with per-8 KB ROM/RAM selection and RAM write protection; four
CHR modes with **separate background and sprite banks** when 8×16 sprites are enabled;
1 KB ExRAM usable as an extra nametable, as per-tile **extended attributes**
(mode 1: per-tile palette and 4 KB CHR bank), or as plain RAM; fill-mode nametables;
the **vertical split screen**; an 8×8→16 multiplier; and the scanline IRQ.

The MMC5 has no scanline input, so it infers PPU state from the fetches it sees: three
consecutive reads of the same nametable address mark the start of a scanline, and 3 CPU
cycles without PPU reads mean rendering has stopped. Lockstep reproduces this exactly
from the PPU's real fetch pattern. Two expansion pulse channels (`$5000-$5007`, `$5015`)
and the 8-bit raw PCM channel + IRQ (`$5010-$5011`) are also emulated.

**MMC2 / MMC4.** Two CHR banks per pattern table, selected by latches that flip when the
PPU fetches tile `$FD` or `$FE` (Punch-Out!!'s large opponent sprites). The latch
switches *after* the triggering fetch, which `notify_vram_addr` models.

**FME-7 / Sunsoft 5B (69).** Eight 1 KB CHR banks, four 8 KB PRG banks (including `$6000`,
which can be ROM or RAM), mirroring control, a 16-bit CPU-cycle IRQ counter, and the
Sunsoft 5B's 3-channel YM2149F tone + noise PSG expansion audio (`$C000`/`$E000`).

**Konami VRC6 (24, 26).** 16 KB + 8 KB switchable PRG banks, eight 1 KB CHR banks with
configurable 1 KB / 2 KB modes and mirroring (`$B003`), scanline/cycle IRQ counter
(`$F000-$F002`), and VRC6 expansion audio (two 16-step variable-duty pulse channels plus
a 6-bit sawtooth accumulator).

**Namco 163 (19).** Three 8 KB switchable PRG banks, eight 1 KB CHR banks and four 1 KB
nametable banks (each selectable between CHR-ROM and console CIRAM), a 15-bit up-counting
CPU-cycle IRQ timer, and 128 bytes of internal wavetable RAM driving a 1–8 channel
multiplexed wavetable synthesizer.

**Discrete boards (0, 2, 3, 7, 11, 13, 34, 66, 71, 79).** Simple latches. UxROM, CNROM, BNROM,
CPROM, Color Dreams and GxROM model **bus conflicts**: the written value is ANDed with
the ROM byte at the target address, as on those boards. AxROM does not, because
ANROM-family boards lack them and AOROM games avoid them.

## Header fixups

`src/cartridge_fixups.rs` holds entries like:

```rust
HeaderFixup { crc: 0x548A2C3C, mapper: Some(206), submapper: None, battery: None,
              mirroring: None, mmc3a: false, save_ram_kb: None, chr_ram_kb: None }, // Karnov (USA)
```

`crc` is the CRC32 of the PRG-ROM and CHR-ROM data, excluding the 16-byte header and any
trainer. That is the usual key for cartridge databases such as NesCartDB. Any `Some`
field overrides the header.

`src/cartridge_headerless.rs` describes dumps with no header at all, keyed by the CRC32
of the whole file. Lockstep synthesises an NES 2.0 header from the entry.

To add a fixup for another dump:

1. compute the CRC of the ROM data, e.g. in Python:
   `zlib.crc32(data[16 + (512 if data[6] & 4 else 0):])`;
2. look up the correct board in a cartridge database (NesCartDB, or a NES 2.0 header
   database);
3. add a line to the table. Comments with the game title are encouraged.

## Adding a mapper

1. **Create `src/mappers/<board>.rs`** with a struct that embeds a `CartBase` and derives
   `Clone`:

   ```rust
   // SPDX-License-Identifier: GPL-3.0-or-later
   //! Mapper NN: <board name> (<example game>).

   use super::{CartBase, Mapper};

   #[derive(Clone)]
   pub struct MyBoard {
       base: CartBase,
       // board registers…
   }

   impl MyBoard {
       pub fn new(mut base: CartBase) -> Self {
           base.set_prg_16k(0, 0);   // power-on banking
           base.set_prg_16k(1, -1);  // negative = counted from the last bank
           Self { base }
       }
   }

   impl Mapper for MyBoard {
       fn base(&self) -> &CartBase { &self.base }
       fn base_mut(&mut self) -> &mut CartBase { &mut self.base }
       fn name(&self) -> &'static str { "MyBoard (NN)" }

       fn write_register(&mut self, addr: u16, val: u8, _cycle: u64) {
           // called for CPU writes to $8000-$FFFF
           self.base.set_prg_16k(0, (val & 0x0F) as i32);
       }
   }
   ```

2. **Use the `CartBase` helpers** for banking (`set_prg_8k/16k/32k`, `set_prg_ram_8k`,
   `unmap_prg`, `set_chr_1k/2k/4k/8k`, `set_chr_1k_kind`, `set_mirroring`,
   `set_nametable`). They reduce bank numbers modulo the memory size, so registers can be
   stored verbatim.
3. **Override hooks only as needed**:
   - registers outside `$8000-$FFFF` → override `cpu_write` (and `cpu_read` for readable registers);
   - scanline counters → `wants_vram_addr` + `notify_vram_addr` (watch A12, like `mmc3.rs`);
   - CPU-cycle timers → `wants_cpu_clock` + `cpu_clock`;
   - IRQs → `irq()` returns the current line state;
   - unusual nametable or CHR behaviour → override `ppu_read` / `ppu_write` (see `mmc5.rs`).
4. **Register it** in `mappers/mod.rs`: add `mod <board>;`, a match arm in
   `create_mapper`, and the number in `is_supported`.
5. **Test it**: run its games with `lockstep-headless shot` / `batch` and any available
   test ROMs, and add a game to `boots_one_game_per_mapper` in `tests/smoke.rs`.
6. **Document it**: add a row to the table above.
