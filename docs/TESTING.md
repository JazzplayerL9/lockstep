# Testing, debugging and profiling

Lockstep's correctness rests on three layers of testing: hardware **test ROMs**, a
**whole-library boot test** with visual review, and **cargo integration tests**. This
page covers how to run each, the `lockstep-headless` tool, and the built-in profiler.

- [Cargo tests](#cargo-tests)
- [lockstep-headless](#lockstep-headless)
- [Test ROMs](#test-roms)
- [Whole-library regression run](#whole-library-regression-run)
- [Profiling](#profiling)
- [Regression checklist](#regression-checklist)

---

## Cargo tests

```sh
cargo test --release
```

| Test | What it checks |
|---|---|
| `boots_one_game_per_mapper` | one game per major board runs 300 frames without a CPU jam |
| `save_state_clone_is_deterministic` | a cloned `System` produces bit-identical frames and RAM |
| `run_ahead_shows_the_next_frame_early` | run-ahead's displayed frame equals the real next frame, and the real state is untouched |
| `overclock_mode_runs_games` | games run with overclock scanlines enabled |
| doctest in `lib.rs` | the crate-level usage example compiles |

The integration tests need commercial ROMs, which are not distributed. They look in
`$LOCKSTEP_ROM_DIR`, defaulting to `../NES` next to the crate, and **skip** any game
that's missing.

## lockstep-headless

A windowless front end to the same core, built alongside the GUI:

```sh
cargo build --release
target/release/lockstep-headless <command> …
```

### `test` — automated test ROMs

```sh
lockstep-headless test path/to/*.nes
```

Runs each ROM for up to 60 emulated seconds using blargg's `$6000` reporting protocol.
The signature `DE B0 61` at `$6001-$6003` marks a protocol-aware test; `$6000` is `$80`
while running, `$81` when the test wants a reset (performed automatically), and the
result code when done (0 = pass). Prints PASS/FAIL with the test's text output from
`$6004`. Tests that never report this way get a screenshot in `/tmp/nestest_<name>.png`
to check by eye.

### `shot` — run and screenshot

```sh
lockstep-headless shot <rom> <frames> <out.png> [script]
```

Runs `<frames>` frames with an input script, writes the final frame as PNG, and prints
the emulation speed:

```
3000 frames in 7.31s (410 fps), mapper NROM (0), jammed=false
```

| Script | Input |
|---|---|
| `start` (default) | tap Start every 2 s for ~25 s, tap A, walk right later |
| `game` | Start taps through menus for 15 s, then walk right, jump (A) and fire (B) |
| `walk` | a single Start press at 2 s, then run right and jump |
| `hold` | Start every 2 s, otherwise hold Right and pulse B |
| `none` | no input (test ROMs, attract modes) |

### `batch` — whole directories

```sh
lockstep-headless batch <rom-dir> <frames> <out-dir> [threads]
```

Runs every `.nes`/`.zip` in `<rom-dir>` in parallel (default 8 threads) with the `game`
script, saving `<out-dir>/<name>.png` and a tab-separated `<out-dir>/summary.tsv`:

| Column | Meaning |
|---|---|
| `name` | ROM file name |
| `status` | `ok`, `BLANK` (single-colour final frame), `JAM@<frame> pc=<addr>` (CPU hit a KIL opcode), `ERR <message>` (load error, unsupported mapper), `PANIC` |
| `mapper` | board name |
| `colors` | distinct colours in the final frame |
| `ms` | wall time for the ROM |

Non-`ok` rows are also printed. `BLANK` is often a false positive (a fade between
scenes), so check the screenshot.

## Test ROMs

Get the standard collection:

```sh
mkdir -p ~/nes-test-roms && cd ~/nes-test-roms
curl -L -o tr.zip https://github.com/christopherpow/nes-test-roms/archive/refs/heads/master.zip
unzip -q tr.zip
```

Suites with automatic reporting:

```sh
T=~/nes-test-roms/nes-test-roms-master
lockstep-headless test $T/instr_test-v5/rom_singles/*.nes $T/instr_misc/rom_singles/*.nes \
    $T/instr_timing/rom_singles/*.nes $T/cpu_interrupts_v2/rom_singles/*.nes \
    $T/cpu_dummy_writes/*.nes $T/cpu_exec_space/*.nes \
    $T/ppu_vbl_nmi/rom_singles/*.nes $T/ppu_open_bus/*.nes $T/ppu_read_buffer/*.nes \
    $T/oam_read/*.nes $T/oam_stress/*.nes $T/apu_test/rom_singles/*.nes \
    $T/sprdma_and_dmc_dma/*.nes
```

Screen-only suites (run with `shot … none` for ~900 frames and read the result):
`sprite_hit_tests_2005.10.05`, `sprite_overflow_tests`, `blargg_ppu_tests_2005.09.15b`
(`$01` = pass), `vbl_nmi_timing`, `branch_timing_tests`, `cpu_timing_test6`,
`mmc3_test_2`, `mmc3_irq_tests`, `dmc_dma_during_read4`, `cpu_dummy_reads`.

```sh
for f in $T/sprite_overflow_tests/*.nes; do
  lockstep-headless shot "$f" 900 "/tmp/t_$(basename "$f" .nes).png" none
done
```

Current results are in [ACCURACY.md](ACCURACY.md).

## Whole-library regression run

```sh
lockstep-headless batch ~/roms 1800 /tmp/shots 8      # ~19 min for 661 ROMs on 8 cores
tools/contact-sheets.sh /tmp/shots /tmp/sheet 48       # needs ImageMagick
```

Then page through `/tmp/sheet_*.png`. A sheet of 48 games is enough to spot garbled
tiles, broken scrolling splits, wrong palettes, missing sprites and black screens. To
investigate a game, take a series of screenshots over time:

```sh
for n in 300 600 900 1200; do lockstep-headless shot game.zip $n /tmp/g$n.png game; done
montage /tmp/g*.png -tile 4x1 -geometry 256x240+2+2 /tmp/series.png
```

Things that look wrong but aren't: the `game` script can leave a game **paused** (it
taps Start), fade-outs show as black, and some titles legitimately use one-colour
screens.

## Profiling

`perf` is the best tool if it's available:
`perf record -g target/release/lockstep-headless shot game.zip 3000 /tmp/x.png`.

Otherwise, `lockstep-headless` has a built-in sampling profiler. A `SIGPROF` timer records
the instruction pointer every 200 µs of CPU time, and `addr2line` (binutils) symbolises
the samples at the end:

```sh
LOCKSTEP_PROFILE=1 lockstep-headless shot game.zip 3000 /tmp/x.png game
```

```
--- profile: 10152 samples ---
  8.5%  <lockstep_core::ppu::Ppu>::draw_pixel  [ppu.rs:631]
  5.2%  <lockstep_core::ppu::Ppu>::load_tile_info  [ppu.rs:583]
  …
```

Caveats: with aggressive inlining, samples can be attributed to the wrong inlined
function ("skid"). **Always confirm an optimisation with an A/B run of `shot`** (the fps
it prints); the profile is only a hint. For example, on this core, removing the
per-access modulo in the banking code and gating the APU mixer gave measurable gains,
while other hotspots the profile suggested did not.

## Regression checklist

Before committing a change to the core:

1. `cargo test --release`. This includes the run-ahead and save-state determinism tests.
2. The automatic test-ROM command above: no new failures.
3. For PPU or mapper changes: the screen-only suites for the area touched (e.g.
   `mmc3_test_2` for anything involving `notify_vram_addr`).
4. For performance work: `shot` fps for an NROM, MMC3 and MMC5 game, before and after.
5. For anything broad: a `batch` run and a pass over the contact sheets.
6. `cargo doc --no-deps`: no warnings (the crate denies undocumented public items via
   `#![warn(missing_docs)]`).
