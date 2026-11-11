# Accuracy and compatibility

How Lockstep's accuracy is measured, the current results, and every known inaccuracy.

- [Test-ROM results](#test-rom-results)
- [Game compatibility testing](#game-compatibility-testing)
- [Known inaccuracies and unsupported features](#known-inaccuracies-and-unsupported-features)
- [Header fixups](#header-fixups)

---

## Test-ROM results

Tests come from the community collection at
<https://github.com/christopherpow/nes-test-roms> (not distributed with Lockstep).
Tests that report through the `$6000` protocol are checked automatically with
`lockstep-headless test`. Older tests that only print to the screen were checked from
screenshots (`lockstep-headless shot … none`). See [TESTING.md](TESTING.md).

### CPU

| Test | Result |
|---|---|
| `instr_test-v5` (16 ROMs: all official + unofficial opcodes) | ✅ all pass |
| `instr_misc` (4) — wrap-around, dummy reads, APU dummy reads | ✅ all pass |
| `instr_timing` (2) — every opcode's cycle count, branch timing | ✅ all pass |
| `cpu_timing_test6` | ✅ pass |
| `branch_timing_tests` (3) | ✅ all pass |
| `cpu_interrupts_v2` (5) — CLI latency, NMI+BRK, NMI+IRQ, IRQ+DMA, branch delays IRQ | ✅ all pass |
| `cpu_dummy_reads` | ✅ pass |
| `cpu_dummy_writes_oam`, `cpu_dummy_writes_ppumem` | ✅ pass |
| `cpu_exec_space_ppuio`, `cpu_exec_space_apu` | ✅ pass |

### PPU

| Test | Result |
|---|---|
| `ppu_vbl_nmi` (10) — VBL set/clear time, NMI control/timing, suppression, even/odd frames | ✅ all pass |
| `vbl_nmi_timing` (7) | ✅ all pass |
| `sprite_hit_tests_2005.10.05` (11) | ✅ all pass |
| `sprite_overflow_tests` (5) — including the hardware evaluation bug | ✅ all pass |
| `blargg_ppu_tests_2005.09.15b` (5) — palette RAM, sprite RAM, VBL clear, VRAM access, power-up palette | ✅ all pass |
| `ppu_open_bus` | ✅ pass |
| `ppu_read_buffer` | ✅ pass |
| `oam_read`, `oam_stress` | ✅ pass |

### APU

| Test | Result |
|---|---|
| `apu_test` (8) — length counter/table, IRQ flag & timing, jitter, length timing, DMC basics & rates | ✅ all pass |

### DMA

| Test | Result |
|---|---|
| `sprdma_and_dmc_dma`, `sprdma_and_dmc_dma_512` | ✅ pass |
| `dmc_dma_during_read4/dma_2007_write`, `read_write_2007` | ✅ pass |
| `dmc_dma_during_read4/dma_2007_read`, `double_2007_read` | ℹ️ print a CRC with no reference value (alignment dependent) |
| `dmc_dma_during_read4/dma_4016_read` | ❌ fails: DMA halts during `$4016` reads don't clock the controller extra times |

### Mappers

| Test | Result |
|---|---|
| `mmc3_test_2` 1–5 (clocking, details, A12 clocking, scanline timing, MMC3) | ✅ all pass |
| `mmc3_test_2/6-MMC3_alt`, `mmc3_irq_tests/5.MMC3_rev_A` | ❌ expected: these test the revision A chip; rev B is the default (rev A is enabled per game by the fixup table) |
| `mmc3_irq_tests` 1–4, 6 (rev B) | ✅ all pass |

## Game compatibility testing

The development library contains **661 North-American (USA) releases** using mappers
0, 1, 2, 3, 4, 5, 7, 9, 13, 34, 47, 66, 69, 105, 118, 119 and 206. It includes 8 MMC5
games, 13 Acclaim MC-ACC games, both MMC6 games, 51 headerless dumps and ~45 ROMs with
bad headers.

Method (`lockstep-headless batch <dir> 1800 <out>`):

- each ROM runs for **1800 frames** (30 emulated seconds) with a scripted input pattern
  (Start taps to pass menus, then walking right, jumping and firing);
- the runner flags load errors, panics, CPU jams (`KIL` opcodes) and single-colour
  screens;
- every final screenshot was then **inspected by eye** in contact sheets.

Result: **661 / 661 load and run without errors or jams, and render correctly.**
The 12 initially flagged as "blank" were all mid fade-out at the 30 s mark and render
normally. This is a boot-and-early-gameplay check, not a playthrough. Please report
games that break later.

Games that need peripherals other than the standard controller boot, but cannot be
played; see below.

## Known inaccuracies and unsupported features

Behaviour that is **not** emulated, or is simplified. None of it affected the
compatibility test above, except where noted.

**CPU / DMA**
- DMA/controller conflicts: a DMC DMA halting the CPU during a `$4016`/`$4017` read
  doesn't cause the extra shift-register clocks ("bit deletion"). Games that work around
  that hardware bug (e.g. by re-reading the pad) are unaffected.
- The 2A03 CPU test mode registers (`$4018-$401F`) read as open bus.
- The CPU/PPU clock alignment at power-on is fixed (real consoles have 4 random
  alignments). Some test ROMs behave differently on different consoles for this reason.

**PPU**
- OAM decay (DRAM fading while rendering is off) and OAM row corruption when
  `OAMADDR` is written during rendering are not emulated.
- The `$2000`/`$2005` write-on-dot-257 scroll glitch is not emulated. The `$2006`
  AND-glitch is.
- Sprites are never drawn on scanline 0. The real PPU shows nothing there either, but
  garbage evaluation from the pre-render line is not reproduced.
- `$2004` reads during rendering return the evaluation latch, but not every
  intermediate secondary-OAM value.
- The palette is a common NTSC approximation; colour emphasis is approximated by
  attenuating the non-emphasised channels. There is no NTSC composite artefact filter.
- **PAL and Dendy consoles are not supported** (NTSC timing only).

**APU / audio**
- **MMC5** (two expansion pulse channels + 8-bit PCM DAC) and **Sunsoft 5B / FME-7**
  (3-channel YM2149F tone + noise PSG) expansion audio are emulated and mixed into the
  2A03 output stage. VRC6/VRC7, Namco 163, and FDS expansion audio are not emulated.
- The frame-IRQ flag is cleared immediately on a `$4015` read, rather than at the next
  APU cycle.
- DMC "sample duplication" glitches of later CPU revisions are not modelled.

**Input**
- Standard controllers (Player 1 and Player 2 via keyboard and 1000 Hz plug-and-play
  USB/Bluetooth gamepads) and the **NES Zapper light gun** (`$4017` bits 3 and 4, with
  dot-level CRT scanline/phosphor-decay light detection for *Duck Hunt*, *Wild Gunman*,
  *Hogan's Alley*, *Gumshoe*, etc.) are supported. The **Power Pad** (Stadium Events,
  World Class Track Meet), **Arkanoid Vaus paddle**, **Miracle Piano** keyboard,
  **Four Score** and R.O.B. are not emulated.

**Mappers**
- NES-EVENT (Nintendo World Championships 1990) uses a fixed DIP-switch setting
  (6.25-minute timer).
- MMC3 revision A behaviour is selected per game by CRC rather than generally.
- Mappers 10, 11, 71 and 79 are implemented but no game in the test library uses them,
  so they are untested.

## Header fixups

Many ROM dumps in circulation have iNES headers that don't match the actual cartridge:
wrong mapper (e.g. Karnov and Ring King are Namco 108 boards labelled as MMC3), wrong
hard-wired mirroring, missing battery flags (losing save games), or missing submapper
information (MMC6, MC-ACC, MMC3 rev A, RAM sizes). Some dumps have no header at all.

`cartridge_fixups.rs` and `cartridge_headerless.rs` correct these by CRC32 of the ROM
data. The tables were generated by comparing the development library against the Mesen
game database (see the credits in the README). They cover the games in that library. To
add an entry for another dump, see [MAPPERS.md](MAPPERS.md#header-fixups).
