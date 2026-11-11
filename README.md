# Lockstep

**A cycle-accurate NES emulator built for low latency — real-time on Linux, native on Windows.**

Lockstep emulates the Nintendo Entertainment System at the level of individual CPU bus
cycles and PPU dots. The CPU, PPU, APU and cartridge hardware all advance in lockstep
from one master clock, which is where the name comes from. It then runs that emulation on a
real-time scheduled thread whose frames are locked to your monitor's refresh, started
*just in time*, and optionally *run ahead* so that a button press reaches the screen as
quickly as possible.

```
lockstep "Super Mario Bros. 3 (USA).zip"
```

- [Highlights](#highlights)
- [Building](#building)
- [Running](#running)
- [Controls](#controls)
- [Configuration](#configuration)
- [Compatibility and accuracy](#compatibility-and-accuracy)
- [Real-time operation](#real-time-operation)
- [The headless runner](#the-headless-runner)
- [Project layout](#project-layout)
- [Documentation](#documentation)
- [License and credits](#license-and-credits)

---

## Highlights

**Accuracy**
- **Cycle-exact 2A03 CPU**: every bus access, including dummy reads and writes, is its own
  cycle. Interrupt polling happens on the penultimate cycle, with the documented CLI/SEI/PLP
  and branch latency quirks. All unofficial opcodes are implemented.
- **Dot-based 2C02 PPU**: the real background fetch pipeline and shift registers, cycle-exact
  sprite evaluation (including the overflow bug), sprite pattern fetches on dots 257-320,
  delayed `$2006`/`$2007` effects, the odd-frame dot skip, VBlank/NMI races, and open-bus decay.
- **APU**: exact frame-counter and `$4017` timing, length-counter reload quirks, and DMC
  sample fetches performed as real DMA that steals CPU cycles and interacts with OAM DMA.
- **Mappers**: NROM, MMC1 (incl. SUROM / SEROM / NES-EVENT), UxROM, CNROM, MMC3 (incl. MMC6,
  MC-ACC, TxSROM, TQROM, rev A), MMC5, AxROM, MMC2, MMC4, Color Dreams, CPROM, BNROM /
  NINA-001, NES-QJ, GxROM, FME-7, Camerica, NINA-03/06, Namco 108. See
  [docs/MAPPERS.md](docs/MAPPERS.md).
- **Test ROMs**: passes blargg's CPU, PPU, APU, sprite-hit, sprite-overflow, VBL/NMI, MMC3
  and DMA test suites. See [docs/ACCURACY.md](docs/ACCURACY.md).

**Latency and real-time**
- **Display-locked pacing**: one emulated frame per monitor refresh, so no frame is ever
  dropped or repeated. Audio absorbs the 0.1–0.3 % speed difference.
- **Just-in-time frames**: emulation of each frame starts as late as possible so it
  finishes only ~4 ms before it is shown. The margin adapts to your compositor.
- **Run-ahead** (0/1/2 frames) hides a game's own built-in input lag. It is verified frame-exact by a test.
- **Late input sampling**: controller state is read at the instant the game strobes the pad,
  fed by a dedicated **1000 Hz (1 ms) background gamepad polling thread** so controller changes
  are captured even while the UI thread is blocked in vsync swap.
- **Low-latency audio** via ALSA/PipeWire (Linux) or WASAPI (Windows) with a lock-free ring
  buffer and a PI-controlled dynamic rate control loop whose buffer target adapts to your
  sound server (10 ms on low-quantum setups, ~20–25 ms on default PipeWire/WASAPI); no drift,
  no clicks.
- **Real-time & high-priority scheduling**: on Linux, `SCHED_FIFO` emulation and audio threads,
  `mlockall(MCL_ONFAULT)`, CPU pinning (honours `isolcpus=`), 1 ns timer slack, µs-precision
  spin-assisted deadline sleep, and `PREEMPT_RT` detection. On Windows (which has no real-time
  kernel scheduler), uses `QueryPerformanceCounter`, `timeBeginPeriod(1)`, `THREAD_PRIORITY_HIGHEST`
  / `THREAD_PRIORITY_TIME_CRITICAL`, and a 1.5 ms spin-guard window.

**Convenience**
- **Plug-and-play USB / Bluetooth gamepads** (Player 1 & Player 2) via `gilrs` (`evdev`/`udev`
  on Linux, `XInput`/`RawInput` on Windows) polled at 1000 Hz, with hot-plug notifications,
  runtime A/B layout swap (F9), and full ROM-browser navigation from the controller.
- **NES Zapper light-gun emulation** on controller port 2 (`$4017` bits 3 and 4): aim with the
  mouse cursor, left-click to fire at the screen, right-click to fire off-screen (*Duck Hunt*,
  *Wild Gunman*, *Hogan's Alley*, *Gumshoe*, etc.).
- Loads `.nes`, NES 2.0 headers, `.zip` archives, and known **headerless** dumps.
- Corrects known-bad iNES headers by CRC (wrong mapper, mirroring or battery flags).
- Battery saves (`<rom>.sav`, written every ~10 s and on exit), **9 persistent disk save-state
  slots** (`<rom>.s1`..`<rom>.s9`, gzip-compressed), reset.
- ROM browser with hold-to-scroll, overscan crop, fullscreen, optional "overclock".
- **Cross-platform (Linux & Windows)**: full real-time `PREEMPT_RT` / `SCHED_FIFO` support on
  Linux; runs natively on Windows with `QueryPerformanceCounter` timing, 1 ms multimedia timer
  periods (`timeBeginPeriod(1)`), high thread priority, WASAPI audio, and XInput gamepads.

---

## Building

Lockstep is a Rust 2024-edition crate. Build it with a recent stable toolchain via
[rustup](https://rustup.rs). It was developed on Rust 1.99.

Linux system libraries:

| Distribution | Packages |
|---|---|
| Arch | `alsa-lib libx11 libxi mesa` (usually already present) |
| Debian / Ubuntu | `libasound2-dev libx11-dev libxi-dev libgl1-mesa-dev pkg-config` |
| Fedora | `alsa-lib-devel libX11-devel libXi-devel mesa-libGL-devel` |

Windows requires no extra libraries beyond the standard Rust toolchain (`x86_64-pc-windows-msvc`
or `x86_64-pc-windows-gnu`); audio (WASAPI), graphics, and gamepads (XInput / RawInput) link
against standard Win32 system DLLs automatically.

```sh
cargo build --release          # optimized binaries in target/release/
cargo run --release -- path/to/game.nes
```

`cargo run` without `--release` is also fine. The dev profile is configured with
`opt-level = 3` because a cycle-accurate core is far too slow unoptimized. It runs at
roughly 350–400 emulated frames per second on one modern core.

Lockstep targets Linux first (where `PREEMPT_RT` and `SCHED_FIFO` are available) and also
builds and runs natively on Windows (using Win32 high-priority threads, `timeBeginPeriod(1)`,
and `QueryPerformanceCounter` spin-assisted pacing, since Windows does not offer a real-time
scheduler).

---

## Running

```
lockstep [ROM_FILE | ROM_DIRECTORY]
lockstep --help
lockstep --version
```

- With a ROM file (`.nes`, or a `.zip` containing one), the game starts immediately.
- With a directory, or no argument, the **ROM browser** opens. It starts in the first of:
  the argument, `$LOCKSTEP_ROM_DIR`, a `roms`/`ROMs`/`NES` folder in or next to the
  current directory, or the current directory.

No ROMs are included. Use dumps of cartridges you own.

### ROM browser

| Key | Action |
|---|---|
| Up / Down | Move the selection (hold to scroll; it accelerates after ~1.5 s) |
| Left / Right, PgUp / PgDn | Move by 20 entries (also repeats when held) |
| Home / End | First / last entry |
| Enter | Load ROM / enter directory |
| Backspace | Parent directory |
| Esc / F1 | Return to the game |

The panel header shows the loaded game, the kernel's real-time capabilities, and live
pipeline statistics: sync mode, measured refresh rate, frame cost, run-ahead, scheduling
priority, worst-case jitter, and audio buffer level, DRC correction and underruns.

---

## Controls

Up to two USB or Bluetooth controllers (Player 1 and Player 2) are detected automatically via
plug-and-play — even if plugged in or reconnected while a game is running. Player 1 combines
keyboard and the first gamepad; a second gamepad drives Player 2.

| NES | Keyboard (P1) | Gamepad (P1 & P2) |
|---|---|---|
| D-pad | Arrow keys or W A S D | D-pad or Left Analog Stick |
| A | Z, J or Space | South (A / Cross), North (Y / Triangle), R1 or R2 |
| B | X, K or Left Shift | West (X / Square), East (B / Circle), L1 or L2 |
| Start | Enter | Start |
| Select | Right Shift, Tab or Backspace | Select / Back |
| ROM Browser | Esc or F1 | Guide / Mode button, or Select + Start |

Set `LOCKSTEP_PAD_SWAP=1` if you prefer Nintendo face-button labels (`East` = A, `South` = B).
In the ROM browser, the D-pad / Left Stick moves the selection, L1 / R1 pages by 20 entries,
**A** or **Start** loads the selected ROM (or enters a folder), and **B** ascends to the parent
directory.

Pressing opposing directions at the same time (impossible on a real pad) is filtered, because
some games crash on it.

**Zapper light gun (Port 2)**: Move the mouse cursor over the NES screen to aim; **left-click**
pulls the trigger at the cursor position (with CRT beam-position and phosphor-decay light
detection on `$4017` bit 3), and **right-click** pulls the trigger aimed off-screen.

| Hotkey | Function |
|---|---|
| Esc / F1 | Toggle the ROM browser |
| F2 | Unload the cartridge |
| F3 | Toggle real-time (`SCHED_FIFO` on Linux / High-Priority 1 ms on Windows) scheduling |
| F4 | Toggle overclock (extra CPU time per frame; reduces slowdown in some games) |
| 1 – 9 | Select active save-state slot (1–9) |
| F5 / F8 | Save / load state to persistent disk file (`<rom>.s1`..`<rom>.s9`) |
| F6 | Toggle 8-pixel overscan crop |
| F7 | Cycle run-ahead: off → 1 frame → 2 frames |
| F9 | Toggle gamepad A/B button layout swap (`South=A` vs `East=A`) |
| F11 | Toggle fullscreen |
| R | Reset the console |

---

## Configuration

Everything is set through environment variables; there is no config file.

| Variable | Default | Meaning |
|---|---|---|
| `LOCKSTEP_ROM_DIR` | *(auto)* | Directory the ROM browser opens in |
| `LOCKSTEP_RT_PRIO` | `45` | SCHED_FIFO priority of the emulation thread (1–99, Linux only). Audio runs one step higher. |
| `LOCKSTEP_PAD_SWAP` | *(unset)* | If set, swap South/East gamepad buttons (`East` = A, `South` = B) |
| `LOCKSTEP_STATS` | *(unset)* | If set, print real-time statistics to stderr every 2 seconds |
| `LOCKSTEP_PROFILE` | *(unset)* | `lockstep-headless shot` only (Linux): print a sampling profile |

Real-time priorities on Linux require permission. See [docs/REALTIME.md](docs/REALTIME.md#permissions)
for the one-line `limits.conf` setup.

---

## Compatibility and accuracy

The development library of **661 North-American releases** was run through the batch
runner for 30 emulated seconds each with scripted input, and every screenshot was
checked by hand. All 661 load, boot and render correctly. That includes 51 headerless
dumps and ~45 ROMs with incorrect headers that are fixed up by CRC.

Test-ROM results (details and known gaps in [docs/ACCURACY.md](docs/ACCURACY.md)):

| Suite | Result |
|---|---|
| CPU: `instr_test-v5`, `instr_misc`, `instr_timing`, `cpu_timing_test6`, `branch_timing_tests` | all pass |
| CPU: `cpu_interrupts_v2`, `cpu_dummy_reads`, `cpu_dummy_writes`, `cpu_exec_space` | all pass |
| PPU: `ppu_vbl_nmi`, `vbl_nmi_timing`, `sprite_hit_tests`, `sprite_overflow_tests` | all pass |
| PPU: `blargg_ppu_tests`, `ppu_open_bus`, `ppu_read_buffer`, `oam_read`, `oam_stress` | all pass |
| APU: `apu_test` | all pass |
| Mappers: `mmc3_test_2`, `mmc3_irq_tests` | all pass except the MMC3 *rev A* tests (rev B is emulated by default) |
| DMA: `sprdma_and_dmc_dma`, `dma_2007_write`, `read_write_2007` | pass |
| DMA: `dma_4016_read` | **fails** (DMA/controller-read conflict not emulated) |

---

## Real-time operation

**Linux (true real-time)**
1. Give your user real-time permission (`@audio - rtprio 95` in `/etc/security/limits.d/`).
2. Run Lockstep. The browser header should say **Display-synced** and **SCHED_FIFO 45**.
3. Optionally reserve a core with `isolcpus=N nohz_full=N threadirqs` on the kernel
   command line; Lockstep pins itself to it automatically.

**Windows (best-effort low latency)**
- Windows does not provide a deterministic real-time scheduler (`PREEMPT_RT` / `SCHED_FIFO`).
- Instead, Lockstep uses `QueryPerformanceCounter` for sub-microsecond timestamps, requests
  a 1 ms kernel timer tick (`timeBeginPeriod(1)`), promotes the emulation and audio threads to
  `THREAD_PRIORITY_HIGHEST` / `THREAD_PRIORITY_TIME_CRITICAL`, and spin-waits the final 1.5 ms
  before each frame deadline so display-synced JIT pacing and run-ahead still work cleanly.

The full story covers the pacing modes, the just-in-time scheduler, run-ahead, the audio
control loop, latency budgets, tuning and troubleshooting. It is in
**[docs/REALTIME.md](docs/REALTIME.md)**.

---

## The headless runner

`lockstep-headless` runs the same core without a window, for testing:

```sh
# blargg-style test ROMs that report through $6000:
lockstep-headless test path/to/tests/*.nes

# Screenshot after N frames with a scripted input pattern (game | walk | start | hold | none):
lockstep-headless shot game.zip 1800 out.png game

# Boot every ROM in a directory, in parallel, writing screenshots and summary.tsv:
lockstep-headless batch ~/roms 1800 /tmp/shots 8
```

See [docs/TESTING.md](docs/TESTING.md) for the scripts, output format, the built-in
profiler and the regression-testing workflow.

---

## Project layout

```
src/
  lib.rs                  crate root (lockstep_core) and overview docs
  cpu.rs                  System: 2A03 CPU, bus, DMA, run-ahead, the whole machine
  ppu.rs                  2C02 PPU, dot by dot
  apu.rs                  2A03 APU and mixer
  mappers/                cartridge boards (mod.rs = framework + factory)
  cartridge.rs            ROM loading (iNES, NES 2.0, zip, headerless), saves
  cartridge_fixups.rs     CRC table of header corrections
  cartridge_headerless.rs CRC table of known headerless dumps
  input.rs                controllers
  palette.rs              NTSC palette, emphasis, RGBA lookup
  rt.rs                   real-time engine: scheduling, display sync, worker thread
  audio.rs                audio output, ring buffer, dynamic rate control
  png.rs                  tiny PNG encoder
  main.rs                 `lockstep` GUI (macroquad)
  bin/headless.rs         `lockstep-headless`
tests/smoke.rs            integration tests (need a local ROM library)
tools/contact-sheets.sh   tile batch screenshots into review sheets
docs/                     in-depth documentation
```

---

## Documentation

| Document | Contents |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | How the emulator works: master clock, CPU/PPU/APU timing, DMA, mappers, save states, threads |
| [docs/REALTIME.md](docs/REALTIME.md) | Scheduling, display sync, just-in-time frames, run-ahead, audio DRC, tuning, troubleshooting |
| [docs/ACCURACY.md](docs/ACCURACY.md) | Test-ROM results, compatibility testing, known inaccuracies |
| [docs/MAPPERS.md](docs/MAPPERS.md) | Supported boards, header fixups, how to add a mapper |
| [docs/TESTING.md](docs/TESTING.md) | Tests, the headless runner, screenshots, profiling |
| [CHANGELOG.md](CHANGELOG.md) | Release history |

API documentation: `cargo doc --open`. Every public item is documented, and
`#![warn(missing_docs)]` keeps it that way.

---

## License and credits

Lockstep is free software: you can redistribute it and/or modify it under the terms of
the **GNU General Public License, version 3 or (at your option) any later version**. It
is distributed WITHOUT ANY WARRANTY. See [LICENSE](LICENSE).

Copyright © 2026 Zach Jones.

Lockstep stands on the shoulders of the NES emulation community:

- **[Mesen](https://github.com/nesdev-org/MesenCE)** (© 2014–2026 Sour and contributors,
  GPL-3.0-or-later). Lockstep's CPU timing and DMA engine, PPU rendering pipeline and
  register timing, APU frame counter/DMC timing, and several mapper implementations
  closely follow Mesen's. Those source files carry an attribution notice. The CRC
  tables in `cartridge_fixups.rs` and `cartridge_headerless.rs` were generated from
  Mesen's game database, which is itself compiled from NesCartDB, Nestopia's database
  and NewRisingSun's NES 2.0 header database.
- **[Nestopia](https://github.com/0ldsk00l/nestopia)**, consulted as a second reference
  for hardware behaviour.
- **The [NESdev Wiki](https://www.nesdev.org/wiki/NES_reference_guide)** and its contributors:
  the primary hardware reference.
- **blargg** and other test-ROM authors, whose test suites make accuracy measurable.
  Test ROMs are not distributed with Lockstep.

"Nintendo Entertainment System", "NES" and "Famicom" are trademarks of Nintendo.
Lockstep is not affiliated with or endorsed by Nintendo.
