# Lockstep architecture

This document explains how Lockstep emulates the NES: the timing model that ties the
chips together, each component in turn, and how the emulator is driven in real time.
Read it alongside the source; every module and public item has rustdoc comments
(`cargo doc --open`).

- [The big picture](#the-big-picture)
- [The master clock](#the-master-clock)
- [CPU (2A03)](#cpu-2a03)
- [DMA](#dma)
- [PPU (2C02)](#ppu-2c02)
- [APU](#apu)
- [Cartridges and mappers](#cartridges-and-mappers)
- [ROM loading](#rom-loading)
- [Save states and run-ahead](#save-states-and-run-ahead)
- [Overclock](#overclock)
- [Threads and data flow](#threads-and-data-flow)
- [Design decisions](#design-decisions)

---

## The big picture

```
                 ┌───────────────────────── System (cpu.rs) ─────────────────────────┐
                 │                                                                   │
  joypad ──────► │  CPU core ──bus access──► start_cycle ──► PPU.run(master clock)   │
                 │     │                         │       └──► APU.clock()          │
                 │     │                         │       └──► mapper.cpu_clock()   │
                 │     ▼                         ▼                                   │
                 │  bus_read / bus_write ◄──► RAM, PPU regs, APU regs, joypad,       │
                 │                            cartridge (Box<dyn Mapper>)            │
                 │     │                                                             │
                 │     ▼                                                             │
                 │  end_cycle ──► PPU.run ──► NMI edge detect, IRQ line sample       │
                 └───────────────────────────────────────────────────────────────────┘
```

`System` owns everything. The **CPU is the scheduler**: it executes instructions, and
every bus access it performs is exactly one CPU cycle. Each cycle is split into a
*start* half and an *end* half. Around the actual memory access, the PPU is advanced to
the current master-clock time and the APU and cartridge are clocked once. Nothing ever
runs ahead of anything else by more than a fraction of a CPU cycle, which is what makes
timing-sensitive games work without per-game hacks.

## The master clock

The NTSC NES derives everything from a 21.477 MHz master clock:

| Unit | Master clocks per tick | Rate |
|---|---|---|
| CPU cycle | 12 | 1.789773 MHz |
| PPU dot | 4 | 5.369318 MHz |

So there are exactly three PPU dots per CPU cycle. Lockstep keeps a `master_clock`
counter in `System` and a separate one in the `Ppu`. The CPU splits each cycle
asymmetrically depending on direction, following Mesen's model of where in the cycle
the data bus is sampled:

| Access | `start_cycle` adds | `end_cycle` adds |
|---|---|---|
| Read | 5 | 7 |
| Write | 7 | 5 |

After each half, `Ppu::run(master_clock - 1)` executes PPU dots until the PPU's own clock
catches up (the `- 1` is a fixed CPU/PPU phase offset). The consequence is that a CPU
write to `$2001` lands on the PPU dot it would hit on hardware, and a `$2002` read sees
the VBlank flag exactly as the real race allows. Test ROMs such as
`ppu_vbl_nmi/02-vbl_set_time` and `05-nmi_timing` measure this to the dot.

At power-on the CPU runs 8 reset cycles before fetching its first opcode, as hardware does.

## CPU (2A03)

`cpu.rs`. The 6502 core without decimal mode, plus the 2A03's on-chip DMA units.

### Instruction execution

`step_instruction()`:

1. Fetch the opcode (one read cycle).
2. Look up its **addressing mode** in `ADDR_MODE[256]` and run `fetch_operand()`. This
   performs *all* the operand bus cycles, including the dummy ones:
   - implied/accumulator: a dummy read of the next byte;
   - `zp,X`/`zp,Y`/`(zp,X)`: a dummy read of the unindexed zero-page address;
   - `abs,X`/`abs,Y`/`(zp),Y` *reads*: a dummy read of the wrong-page address only when a
     page is crossed;
   - the same modes for *writes and read-modify-writes* (`AbsXW`, `AbsYW`, `IndYW`):
     the dummy read always happens.
3. `execute(op)` performs the operation's own cycles. Read-modify-write instructions
   (`INC`, `ASL`, the unofficial `SLO`/`DCP`/...) write the **unmodified value back**
   before writing the result, as the real CPU does. Some hardware depends on this
   double write, for example MMC1, which ignores the second write of a consecutive pair.
4. If an interrupt was latched (see below), run the 7-cycle interrupt sequence.

Cycle counts are never looked up in a table: an instruction takes exactly as many cycles
as it performs bus accesses. All 256 opcodes are implemented, including the unofficial
ones (`LAX`, `SAX`, `DCP`, `ISB`, `SLO`, `RLA`, `SRE`, `RRA`, `ANC`, `ALR`, `ARR`, `AXS`,
`LAS`, the `SH*`/`TAS` family with their DMA-dependent behaviour, the multi-byte `NOP`s,
and the `KIL`/`JAM` opcodes that freeze the CPU and set `System::jammed`).

### Interrupts

Real 6502 interrupt timing is subtle, and several games depend on it:

- **NMI** is edge-triggered. At the end of every cycle, `end_cycle` compares the PPU's
  NMI output (`ppu.nmi_line`) with its previous value, and a rising edge sets `need_nmi`.
- **IRQ** is level-triggered: `run_irq = (mapper.irq() || apu.irq_line()) && !I`.
- Both are **polled on the second-to-last cycle** of an instruction. This is modelled
  by keeping the previous cycle's values (`prev_need_nmi`, `prev_run_irq`); the
  instruction checks those after its last cycle.
- Consequences that fall out naturally: `CLI`/`SEI`/`PLP` change the I flag after the
  poll, so an IRQ is taken one instruction late (`cpu_interrupts_v2/1-cli_latency`).
- A **taken branch that doesn't cross a page** ignores interrupts on its last cycle, and
  a page-crossing branch polls twice. See `branch()` and `5-branch_delays_irq`.
- **Interrupt hijacking**: if an NMI arrives while `BRK` is pushing, `BRK` uses the NMI
  vector (`2-nmi_and_brk`).

### The bus

`bus_read` / `bus_write` decode the 16-bit address:

| Range | Device |
|---|---|
| `$0000-$1FFF` | 2 KB RAM, mirrored |
| `$2000-$3FFF` | PPU registers, mirrored every 8 bytes |
| `$4000-$4013`, `$4015`, `$4017` (write) | APU |
| `$4014` | OAM DMA |
| `$4016` | controller strobe (write) / port 1 (read) |
| `$4015` (read) | APU status; does not drive the external data bus |
| `$4017` (read) | controller port 2 |
| `$4018-$401F` | open bus (CPU test mode is not emulated) |
| `$4020-$FFFF` | cartridge (`Mapper::cpu_read` / `cpu_write`) |

**Open bus.** The last value seen on the data bus is remembered (`open_bus`) and
returned for unmapped reads. Controller reads only drive bit 0 (and bits 1–4 on other
devices), so bits 5–7 come from open bus. Some games, such as Paperboy, check this.

PPU register writes are also forwarded to `Mapper::ppu_register_write`, because MMC5
snoops `$2000` to know the sprite size.

## DMA

`process_pending_dma()` in `cpu.rs`, a port of Mesen's DMA engine.

The 2A03 has two DMA units that **halt the CPU on a read cycle** and then alternate
between *get* (read) and *put* (write) cycles according to the CPU cycle's parity:

- **OAM DMA** (`$4014` write): 256 reads from `$XX00-$XXFF`, each followed by a write to
  `$2004`. That's 513 or 514 cycles depending on alignment.
- **DMC DMA**: when the DMC's sample buffer empties, the APU raises a request
  (`DmcDmaRequest::Start`). The DMA halts the CPU, performs a dummy cycle, and reads one
  byte on a get cycle. That's normally 4 cycles, fewer when overlapping OAM DMA.

Requests only take effect at the CPU's next **read** cycle (`read()` checks
`need_halt`). During the halt the CPU's pending read is repeated, which is why a DMC DMA
landing on a `$2007` or `$4016` read can cause a double read on hardware.
`dmc_dma_during_read4/dma_2007_*` and `sprdma_and_dmc_dma` verify the interplay. Reads
performed by DMA cannot see the 2A03's internal registers at `$4015-$401A`.

Not emulated: the conflict where a DMA halt during a `$4016` read clocks the controller
shift register extra times (`dma_4016_read`).

## PPU (2C02)

`ppu.rs`, modelled on Mesen's NES PPU. The PPU renders **262 scanlines × 341 dots**
per frame, numbered -1 (pre-render) to 260, with dots 0–340. `exec()` advances one dot.

### Per-dot schedule (rendering enabled)

| Scanline | Dots | Work |
|---|---|---|
| 0–239 (visible) | 1–256 | draw one pixel; one step of sprite evaluation; shift BG registers; one step of the BG fetch pipeline |
| -1, 0–239 | 8, 16, …, 256 | coarse-X increment of `v` (and fine-Y increment at 256) |
| -1, 0–239 | 257 | copy horizontal scroll bits `t` → `v` |
| -1, 0–239 | 257–320 | sprite fetches: garbage nametable reads at 257/259, pattern reads at 261, 269, … (8 sprites); `OAMADDR` forced to 0 |
| -1 | 280–304 | copy vertical scroll bits `t` → `v` |
| -1, 0–239 | 321–336 | prefetch the first two tiles of the next line |
| -1, 0–239 | 337, 339 | dummy nametable reads (MMC5 uses these to detect scanlines) |
| -1 | 339 | on odd frames, skip dot 340 (the "odd-frame dot skip") |
| 240 | 0 | frame finished: convert to RGBA, set `frame_complete` |
| 241 | 1 | set VBlank flag; raise NMI if enabled |
| -1 | 1 | clear VBlank, sprite-0-hit and overflow flags |

The **background pipeline** (`load_tile_info`) performs the real fetch sequence on every
8-dot group: nametable byte (dot 1), attribute byte (3), pattern low (5), pattern high (7).
At dot 0 of the group the fetched tile is loaded into the low byte of the 16-bit shift
registers. Pixels come from bit `15 - fine_x` of the shifters.

Every fetch goes through `read_vram()`, which updates the PPU address bus and notifies
the mapper. That is how the MMC3 scanline counter sees A12 rise during sprite fetches,
and how MMC2/MMC4 see tiles `$FD`/`$FE`.

**Sprite evaluation** (`process_sprite_evaluation`) is the cycle-by-cycle hardware
algorithm. Dots 1–64 clear secondary OAM. From dot 65, odd dots read primary OAM and
even dots copy to secondary OAM, including the famous **overflow bug** (the diagonal
OAM scan after 8 sprites are found). This makes `sprite_overflow_tests` pass and
reproduces quirks of games that rely on the overflow flag.

**Sprite rendering.** The (up to) eight sprites found for the next line are fetched
during dots 257–320 into `sprites[]` (already flipped horizontally). When drawing,
the lowest-index opaque sprite wins. Its priority bit then decides between sprite and
background, so a behind-background sprite still masks higher-index sprites (the
"sprite priority quirk"). **Sprite-0 hit** is set when sprite 0 (found on dot 66) and an
opaque background pixel coincide, except at x = 255, in the clipped left column, or with
either layer disabled.

### Register timing

| Behaviour | Implementation |
|---|---|
| `$2002` read on the dot VBlank is set suppresses the flag *and* NMI | `prevent_vbl` |
| Enabling/disabling rendering takes effect one dot later | `rendering_enabled` vs `prev_rendering_enabled` |
| The second `$2006` write updates `v` 3 dots later; if that lands on a scroll increment, `v` is corrupted (AND glitch) | `update_vram_delay` |
| `$2007` reads return the internal buffer, which is refilled a few dots after the read | `read_sm` state machine |
| Two `$2007` reads on consecutive CPU cycles: the second is ignored | `ignore_vram_read` |
| `$2007` writes land a few dots later; during rendering the low address byte is written instead | `write_sm` |
| Palette reads bypass the buffer, upper bits from open bus | `read_register` |
| `$2004` writes during rendering only bump `OAMADDR`; reads return the evaluation latch | `write_register` / `read_register` |
| PPU open bus with per-bit decay (~3 frames) | `set_open_bus` |

### Output

Pixels are stored as 9-bit values (6-bit colour index + 3 emphasis bits from `$2001`,
masked by greyscale). At the end of the frame `send_frame` converts them to RGBA through
a 512-entry lookup table (`palette::rgba_table`), producing `ppu.framebuffer`
(256×240×4 bytes). All 240 lines are output; the GUI crops 8 pixels of overscan per
edge by default (F6).

## APU

`apu.rs`, timing modelled on Mesen's APU. Clocked once per CPU cycle by `start_cycle`,
in this order:

1. **DMC delayed actions**: a `$4015` enable or disable takes effect 2–3 cycles later,
   depending on parity.
2. **Frame counter**: the 4-step / 5-step sequencer with exact step cycles (7457, 14913,
   22371, 29828–29830 / 37281). It produces quarter-frame (envelope, linear counter) and
   half-frame (length counter, sweep) clocks and the frame IRQ. A `$4017` write takes
   effect 3 or 4 cycles later depending on parity, and 5-step mode clocks immediately.
3. **Length-counter reload**: a write to `$4003`/`$4007`/`$400B`/`$400F` loads the counter
   *after* the frame counter has had its chance to clock it that cycle, and is ignored if
   the clock just changed it (`len_reload_timing`).
4. **Channel timers**: pulse ×2 (period ×2+1), triangle, noise (LFSR), DMC (output unit +
   sample buffer, requesting DMA when empty).
5. **Mixing**: the standard non-linear mixer, tabulated (`pulse[31]`, `tnd[203]`) and
   only recomputed when a channel's output can have changed (a timer fired, a register
   was written, or a frame tick happened).

**Resampling.** Mixed values are accumulated every CPU cycle and averaged into one output
sample whenever `output_rate` / 1.789773 MHz worth of time has passed (a box filter).
They then go through the console's analogue output stage as documented on the nesdev
wiki: first-order high-pass filters at 90 Hz and 440 Hz and a first-order low-pass at
14 kHz. The coefficients are recomputed from the current output rate. `output_rate` is adjusted continuously
by the audio layer's dynamic rate control (see REALTIME.md).

Not emulated: cartridge expansion audio (MMC5 pulse/PCM, Sunsoft 5B, VRC6/7, FDS, N163).
North-American NES consoles don't route cartridge audio without modification.

## Cartridges and mappers

`mappers/mod.rs` defines the framework; each board lives in its own file. See
[MAPPERS.md](MAPPERS.md) for the list and a walkthrough of adding one.

### `CartBase`: the shared banking model

Every board embeds a `CartBase` holding the memories and three small lookup tables:

| Table | Granularity | Maps |
|---|---|---|
| `prg: [PrgSlot; 5]` | 8 KB | CPU `$6000-$FFFF` → PRG-ROM / PRG-RAM / nothing (+ writable flag) |
| `chr: [(ChrKind, usize); 8]` | 1 KB | PPU `$0000-$1FFF` → CHR-ROM / CHR-RAM |
| `nt: [usize; 4]` | 1 KB | the four nametables → CIRAM pages (this *is* mirroring) |

Bank-switching just rewrites slot entries (`set_prg_8k`, `set_chr_4k`, `set_mirroring`, …),
reducing bank numbers modulo the memory size at switch time. The read and write fast paths
are then a shift, an add and an index, with no division.

ROM is held in `Arc<[u8]>`, so cloning a mapper (save states, run-ahead) never copies it.

### The `Mapper` trait

```rust
pub trait Mapper: Send + MapperClone {
    fn base(&self) -> &CartBase;                 // required
    fn base_mut(&mut self) -> &mut CartBase;     // required
    fn name(&self) -> &'static str;              // required
    fn cpu_read(&mut self, addr: u16) -> Option<u8>;          // default: PRG slots
    fn cpu_write(&mut self, addr: u16, val: u8, cycle: u64);  // default: RAM, or write_register for $8000+
    fn write_register(&mut self, addr: u16, val: u8, cycle: u64) {}
    fn ppu_read(&mut self, addr: u16) -> u8;                  // default: CHR + nametables
    fn ppu_write(&mut self, addr: u16, val: u8);
    fn wants_vram_addr(&self) -> bool { false }               // opt in to...
    fn notify_vram_addr(&mut self, addr: u16, cpu_cycle: u64) {} // ...every PPU address-bus change
    fn wants_cpu_clock(&self) -> bool { false }               // opt in to...
    fn cpu_clock(&mut self) {}                                // ...a call every CPU cycle
    fn ppu_register_write(&mut self, addr: u16, val: u8) {}
    fn irq(&self) -> bool { false }
    fn has_expansion_audio(&self) -> bool { false }
    fn audio_sample(&self) -> f32 { 0.0 }
    fn battery_ram(&self) -> Vec<u8>;
}
```

The `wants_*` flags are read once at power-on and cached by `System`/`Ppu`, so boards
that don't need the hot-path hooks pay nothing for them.

### Notable board logic

- **MMC3 IRQ**: clocked by **rising edges of PPU A12** that follow at least 3 CPU cycles of
  A12 low (`notify_vram_addr`). Because the PPU performs the real fetch pattern, the
  counter clocks once per scanline in all sprite/background pattern-table
  configurations, including 8×16 sprites. Rev A behaviour and the Acclaim MC-ACC variant
  (falling edges, ÷8 prescaler) are selected by the CRC fixup table.
- **MMC5** has no direct scanline input. Like the real chip, it detects a new scanline
  by watching for **three consecutive reads of the same nametable address** (dots
  337/339 and the next line's dot 1), and leaves "in-frame" state when the PPU stops
  reading for 3 CPU cycles. Its separate BG/sprite CHR banks for 8×16 sprites, ExRAM
  attribute mode, fill mode and vertical split are all driven off this fetch tracking.
- **MMC1** ignores a write on the cycle right after another write, which matters for
  read-modify-write instructions. It also handles SUROM's 512 KB outer bank, SOROM/SXROM
  RAM banking, the fixed-32K SEROM submapper, and NES-EVENT's countdown timer.

## ROM loading

`cartridge.rs`:

1. **Container**: a `.zip` is parsed directly (central directory, stored or deflate),
   preferring a `.nes` entry.
2. **Header**: iNES 1.0, or NES 2.0 (12-bit mapper, submapper, exponent RAM sizes). Junk in
   bytes 12–15 of old iNES headers ("DiskDude!") causes the upper mapper nibble to be ignored.
3. **Headerless dumps**: if there is no `NES\x1A` magic, the whole file's CRC32 is
   looked up in `HEADERLESS_ROMS`, and a NES 2.0 header is synthesised.
4. **Fixups**: the CRC32 of PRG+CHR is looked up in `HEADER_FIXUPS`, which corrects
   mapper/submapper, battery, mirroring, MMC3 revision and RAM sizes for known-bad headers.
5. **RAM sizing**: 8 KB PRG-RAM by default; battery games save their RAM; MMC5 gets 32 KB
   save or 64 KB work RAM; MMC6 gets its internal 1 KB.
6. **Battery**: `<rom>.sav` is loaded if present.

## Save states and run-ahead

`System` derives `Clone`. Because every component is plain owned data, with ROM shared
through `Arc`, **a clone is a complete, independent machine**. It costs about 10–70 µs
(≈400 KB of RAM, VRAM, frame buffers and mapper state). This gives:

- **Save state / load state**: the worker stores one cloned `Box<System>`.
- **Run-ahead** (`System::run_frame_ahead`): run the real frame, clone, emulate *n* more
  frames with the same input, show that image, then restore the clone. Emulation is fully
  deterministic, so the displayed image is exactly what the game will show *n* frames
  later. A test (`run_ahead_shows_the_next_frame_early`) asserts this bit-for-bit.

## Overclock

With F4, `OVERCLOCK_SCANLINES` (100) idle scanlines are inserted between the end of
rendering and VBlank. The CPU keeps running during them while the PPU does nothing,
giving games ~37 % more CPU time per frame, which removes slowdown in many titles. The
APU is paused for those lines, so audio pitch and tempo are unchanged (`ppu.apu_enabled`).
The APU's even/odd cycle phase is re-synchronised afterwards. Timing-sensitive games may
misbehave, so it is off by default.

## Threads and data flow

```
 ┌────────── UI thread (main.rs, macroquad) ───────────┐        ┌──── emulation thread (rt.rs) ────┐
 │ poll keyboard ──► buttons_held (AtomicU8) ──────────┼───────►│ read at $4016 strobe             │
 │                ──► buttons_latched (taps)  ─────────┼───────►│ OR'd in at frame start           │
 │ DisplayClock: swap timestamps ──► vsync_ns,         │        │ plan frame start (display sync)  │
 │               display_period_ns, present_margin_ns ─┼───────►│ sleep_until (SCHED_FIFO)         │
 │                                                     │        │ run_frame_ahead                  │
 │ wait_for_jit_frame ◄── park/unpark ◄────────────────┼────────┤ publish_frame (triple buffer)    │
 │ with_latest    ◄── TripleFramebuffer ◄──────────────┼────────┤                                  │
 │ draw, swap (vsync) ─────────────────────────────────┘        │ audio.push ──► lock-free ring ───┼──► audio callback thread
 └─────────────────────────────────────────────────────┘        └──────────────────────────────────┘    (cpal, SCHED_FIFO)
        commands (load, reset, save state, ...) ── mpsc channel ──►
```

- No locks are on the frame path. Frames go through a wait-free triple buffer, input
  through atomics, audio through an SPSC ring buffer.
- The emulation thread owns the `System`. The UI only sends commands and reads results.

The timing logic of the emulation thread is described in [REALTIME.md](REALTIME.md).

## Design decisions

**Why catch-up per bus cycle instead of per instruction or per scanline?** The previous
prototype rendered whole scanlines and executed whole instructions. That made games with
mid-frame effects (Kirby's Adventure's status bar, Battletoads, MMC3 IRQ games, sprite-0
splits) glitch or hang, and it needed per-game timing hacks. Advancing the PPU at every
bus access is the simplest model that is correct by construction. It costs about
2.5–3 ms per frame on one core, a fraction of the 16.6 ms budget.

**Why trait objects for mappers?** There are many boards, each with its own state.
`Box<dyn Mapper>` keeps them independent and cloneable. The dynamic-dispatch cost is
contained by doing bank resolution in `CartBase` and caching the opt-in hook flags.

**Why `f32` mono audio, resampled inside the APU?** The NES is mono, and doing the
box-filter decimation per CPU cycle in the APU avoids a separate high-rate buffer. It
also makes dynamic rate control a single multiplication.
