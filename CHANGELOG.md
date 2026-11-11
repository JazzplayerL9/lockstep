# Changelog

All notable changes to Lockstep. Versions follow [Semantic Versioning](https://semver.org).

## 0.4.0 — Cross-Platform, Gamepads, Zapper, Save States, Rewind & Expansion Audio

### Input & Peripherals
- **Plug-and-play USB / Bluetooth gamepads**: automatic hot-plug detection for Player 1
  and Player 2 controllers via `gilrs` (`evdev`/`udev` on Linux, `XInput`/`RawInput` on
  Windows) polled on a dedicated **1000 Hz (1 ms) background thread** (`lockstep-pad`), full
  ROM-browser navigation from the pad, toast notifications on connect / disconnect, and F9 /
  `LOCKSTEP_PAD_SWAP=1` for Nintendo A/B button labels.
- **NES Zapper light-gun emulation** (`$4017` bits 3 and 4): mouse cursor aim + left/right click
  trigger with dot-accurate CRT beam and phosphor-persistence luminance sensing (*Duck Hunt*,
  *Wild Gunman*, *Hogan's Alley*, *Gumshoe*, etc.).

### Save States, Rewind & Gameplay Controls
- **Persistent 9-slot disk save states**: keys `1`–`9` select slots 1–9; F5/F8 save and load
  gzip-compressed bit-exact console snapshots (`<rom>.s1`..`<rom>.s9`) next to the ROM file
  (with in-memory fallback).
- **Real-time Rewind, Pause, Frame-Advance & Fast-Forward**: hold `Backspace` (or `Select + L1`)
  to rewind up to 10 seconds of gameplay; press `P` to pause and `.` to advance frame-by-frame;
  hold `` ` `` / `~` (or `Select + R1`) for 2x fast-forward.
- **CRT Scanline & NTSC 4:3 Display Filter** (`F10`, or `LOCKSTEP_CRT=1`): cycles between pixel-exact
  8:7, NTSC 4:3 pixel aspect ratio (`8:7` PAR), and CRT scanline + bezel mode.
- **PNG Screenshots (`F12`)**: saves `<rom>_<frame>.png`.
- **Game Genie & Raw Cheat Codes**: supports 6- and 8-letter Game Genie codes (`SXIOPO`, `ZEXPYGLA`)
  and raw hex patches (`AAAA:VV`, `AAAA:CC:VV`), loaded automatically from `<rom>.cht` or
  `LOCKSTEP_CHEATS`.

### Mappers & Expansion Audio
- **New Mappers**: added **Konami VRC6a / VRC6b** (Mappers 24 & 26) and **Namco 163 / 129** (Mapper 19).
- **Expansion Audio**: implemented **MMC5** (two expansion pulse channels + 8-bit PCM DAC and IRQ),
  **Konami VRC6** (two 16-step variable-duty pulse channels + 6-bit sawtooth), **Namco 163**
  (1–8 channel wavetable synthesizer), and **Sunsoft 5B / FME-7** (3-channel YM2149F tone + noise
  PSG with logarithmic DAC).

### Cross-Platform & CI
- **Windows support**: portable monotonic nanosecond clock (`QueryPerformanceCounter` via
  `std::time::Instant`), 1 ms multimedia timer period (`timeBeginPeriod(1)`), high-priority
  emulation (`THREAD_PRIORITY_HIGHEST`) and audio (`THREAD_PRIORITY_TIME_CRITICAL`) threads,
  and spin-assisted deadline sleeping on non-RT OSes.
- **GitHub Actions CI & Release workflow**: automated Linux and Windows builds, tests, and
  release archive packaging on tags.

## 0.3.0 — Lockstep

The project was renamed from `nes-rt-emu` to **Lockstep** and licensed under the
**GNU GPL, version 3 or later**.

### Real-time pipeline
- **Display-synchronised pacing**: one emulated frame per display refresh when the
  monitor runs at 59.4–60.8 Hz, so frames are never dropped or repeated. Otherwise it free-runs at 60.0988 Hz.
- **Just-in-time frame scheduling**: each frame starts as late as possible, with an
  adaptive presentation margin and a worst-case frame-cost estimator.
- **Run-ahead** (F7: 0/1/2 frames), implemented in the core as `System::run_frame_ahead`
  and verified bit-exact by a test.
- **Late input sampling**: the controller state is read when the game strobes `$4016`
  (both Player 1 and Player 2). Opposing D-pad directions are filtered.
- **New audio backend**: direct ALSA/PipeWire output through `cpal`, a lock-free ring
  buffer, PI dynamic rate control (±1.5 %) with anti-windup and a self-tuning buffer
  target, an underrun fade-out, and a SCHED_FIFO audio thread. Replaces the `paplay`
  pipe. The output filters now follow the NES's documented output stage (90 Hz and
  440 Hz high-pass, 14 kHz low-pass) at the actual output rate.
- Scheduling: default priority 45 (below kernel IRQ threads, configurable with
  `LOCKSTEP_RT_PRIO`), `mlockall(MCL_ONFAULT)`, pinning to `isolcpus=` cores, and
  PREEMPT_RT detection via `/sys/kernel/realtime`.
- `LOCKSTEP_STATS=1` live statistics. The ROM browser shows the same information.
- F11 fullscreen.

### Performance
- About 20 % faster core: tabulated and change-gated APU mixing, length-counter reloads
  only after writes, no modulo on memory accesses, cached mapper hook flags, and RGBA
  conversion through a lookup table.
- ROM data shared via `Arc` (cheap save states and run-ahead snapshots).

### Fixes
- Frame hand-off API made sound: consumers get a closure-scoped borrow (`with_latest`),
  so a frame can't be read while the producer recycles its buffer.
- Audio flush no longer discards samples pushed after it, and no longer allocates in
  the real-time callback. Zero-length audio callbacks no longer panic.
- Unloading a ROM wakes the UI immediately. Soft reset clears all DMA state. MC-ACC
  always uses the rev B IRQ condition.

### Project
- Library crate `lockstep_core`, binaries `lockstep` and `lockstep-headless`.
- Every public item documented (`#![warn(missing_docs)]`). Added README, `docs/`
  (architecture, real-time, accuracy, mappers, testing) and this changelog.
- `--help` / `--version`. ROM directory taken from the argument, `LOCKSTEP_ROM_DIR`, or
  nearby `roms`/`NES` folders (no hard-coded paths).
- Built-in sampling profiler in `lockstep-headless` (`LOCKSTEP_PROFILE=1`).
- `tools/contact-sheets.sh` for visual regression review.

## 0.2.0 — cycle-accurate core

A rewrite of the emulation core, modelled on Mesen's timing.

### Fixed
- **Segfault on start-up**: the emulation thread overflowed its 2 MB stack in debug
  builds. Machine state is now heap-allocated, and the thread gets a 16 MB stack.
- **Kirby's Adventure garbled screen** and status-bar jitter, along with the general
  class of mid-frame timing bugs, fixed by the new core.

### Changed
- CPU: every bus cycle emulated, including dummy reads and writes. Interrupt polling on
  the penultimate cycle, with branch, CLI/SEI/PLP and BRK-hijack behaviour. OAM and DMC
  DMA performed with real cycle stealing.
- PPU: dot-by-dot rendering with the real fetch pipeline, cycle-exact sprite evaluation
  (with the overflow bug), delayed register effects, odd-frame skip, VBL/NMI races, and
  open-bus decay.
- APU: exact frame counter, length-counter timing, and DMC sample fetch as DMA.
- Mapper framework rewritten around `CartBase` bank slots. Added MMC5, MMC6, MC-ACC,
  TxSROM, TQROM, NES-QJ, NES-EVENT, Namco 108, MMC4, Color Dreams, Camerica and
  NINA-03/06. Bus conflicts on discrete boards. Unsupported mappers are reported instead
  of silently falling back to NROM.
- ROM loading: NES 2.0 headers, a CRC table correcting known-bad headers, and a CRC table
  for headerless dumps (51 in the test library).
- Overclock reworked into idle pre-VBlank scanlines with the APU paused. Now off by default.
- ROM browser: hold-to-scroll with acceleration; the cut-off top status bar was replaced by
  brief auto-fitting notifications; load errors are now displayed.
- New `lockstep-headless` (then `nes-headless`) tool: test-ROM runner, screenshots, and
  parallel batch runs.

### Verified
- blargg CPU, PPU, APU, sprite-hit, sprite-overflow, VBL/NMI and MMC3 test suites pass.
- All 661 ROMs of the development library boot and render correctly.

## 0.1.0 — prototype

Initial multi-ROM emulator with a scanline-based renderer, instruction-level CPU timing,
the core NES mappers, a macroquad front end with ROM browser, and a SCHED_FIFO worker
thread with triple-buffered frame hand-off.
