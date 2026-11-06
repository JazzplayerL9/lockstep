# Real-time operation

Lockstep's goal is that a button press reaches the screen and speakers as quickly and
as consistently as possible, without dropped or doubled frames and without audio
glitches. This document explains how, and how to tune a Linux system for it.

- [Threads and priorities](#threads-and-priorities)
- [Permissions](#permissions)
- [Frame pacing: display sync vs. free run](#frame-pacing-display-sync-vs-free-run)
- [Just-in-time frame scheduling](#just-in-time-frame-scheduling)
- [Run-ahead](#run-ahead)
- [Input path](#input-path)
- [Audio and dynamic rate control](#audio-and-dynamic-rate-control)
- [Latency budget](#latency-budget)
- [Statistics](#statistics)
- [Tuning](#tuning)
- [Troubleshooting](#troubleshooting)

---

## Threads and priorities

| Thread | Scheduling | Role |
|---|---|---|
| UI (`main`) | normal | window, keyboard, ROM browser, drawing; measures display refresh |
| `lockstep-emu` | **SCHED_FIFO 45** (configurable) | owns the `System`; plans, emulates and publishes frames |
| audio callback (cpal) | **SCHED_FIFO 46** | pulls samples from the ring buffer into the sound device |

**Why 45, i.e. below 50?** On Linux the kernel's threaded interrupt handlers (USB
keyboards, sound cards, GPUs) run at SCHED_FIFO 50. A higher-priority emulation
thread could delay your key presses and the sound card's interrupts while it is busy for
its ~3–7 ms per frame. Priority 45 still preempts every normal process. Override it with
`LOCKSTEP_RT_PRIO` (clamped to your `RLIMIT_RTPRIO`). The audio thread always runs one
step above the emulation thread, because an audio underrun is more noticeable than a
frame finishing a little late.

When real-time mode is on (default when permitted; toggle with F3), the emulation thread
also:

- calls `mlockall(MCL_CURRENT | MCL_FUTURE | MCL_ONFAULT)`. Pages are locked once
  touched, so emulator memory can never be swapped out. `MCL_ONFAULT` avoids pinning
  every mapping in the process up front (for example GPU driver mappings);
- sets its **timer slack to 1 ns** (default is 50 µs), so sleeps end on time;
- **pins itself to one CPU**: the last CPU listed in `/sys/devices/system/cpu/isolated`
  (see `isolcpus=` below), otherwise the highest-numbered online CPU.

With F3 off, the thread returns to SCHED_OTHER, 50 µs slack, and all CPUs.

At start-up Lockstep prints what it got:

```
[lockstep] kernel=7.2.14-zen1-1-zen (PREEMPT_RT=false) | mode=RT | SCHED_FIFO=Some(45) | isolated=[] | audio 48000 Hz
```

`PREEMPT_RT=true` means `/sys/kernel/realtime` reads `1`. Lockstep works well on stock
low-latency/zen kernels too. A PREEMPT_RT kernel mainly tightens the worst case.

## Permissions

SCHED_FIFO needs `RLIMIT_RTPRIO > 0`, and locking memory needs a sufficient
`RLIMIT_MEMLOCK`. Most distributions grant both to the `audio` (or `realtime`) group:

```sh
# /etc/security/limits.d/99-realtime.conf
@audio   -  rtprio   95
@audio   -  memlock  unlimited
```

```sh
sudo usermod -aG audio "$USER"   # then log out and back in
ulimit -r                        # should print 95
```

Without permission Lockstep falls back to normal scheduling at nice -20 if allowed,
and the browser shows `SCHED_OTHER`.

## Frame pacing: display sync vs. free run

An NTSC NES produces **60.0988** frames per second. Typical monitors refresh at
60.000 Hz, or 59.94 Hz, or something nearby. If the emulator paced itself with its own
clock, it would drift against the display and have to show some frame twice, or skip one,
every ~10 seconds. That shows up as a visible hitch in scrolling games.

Lockstep instead uses two modes:

| Mode | When | Behaviour |
|---|---|---|
| **Display sync** | measured refresh is 59.4–60.8 Hz *and* stable *and* the UI swapped a buffer within the last 100 ms | exactly one emulated frame per display refresh; never drops or repeats |
| **Free run** | anything else (144 Hz monitors, VRR, minimised window, no vsync) | absolute-deadline pacing at exactly 60.0988 Hz with `CLOCK_MONOTONIC`; catches up if more than 5 frames late |

In display sync the game runs 0.1–1.2 % slower or faster than a real NES, matching the
monitor. That's imperceptible, and the audio system compensates so sound never drifts
(see below).

**Measuring the display.** `DisplayClock` (UI thread) timestamps every return from the
buffer swap. It keeps the last 64 intervals. It considers the cadence vsync-throttled if
the spread between the 12.5th and 87.5th percentiles is under 10 % of the median. It
estimates the period as the mean of the middle half, smoothed by a 2 % exponential moving
average that resets on a jump larger than 2 %.

## Just-in-time frame scheduling

A frame that is ready early just waits, and that wait is added latency. So in display
sync the emulation thread starts each frame **as late as it safely can**:

```
target   = next display refresh that can still be met (never the same one twice)
start    = target − present_margin − cost − cost/8 − 0.5 ms
```

- `cost` is a running estimate of the worst-case time to produce a frame (emulation +
  run-ahead + publishing). It jumps up immediately to any slower frame and decays slowly
  (1/64 per frame), so it tracks the recent worst case.
- `present_margin` is the time the UI and compositor need between receiving a frame and
  the refresh. It starts at 4 ms and **adapts**: if a refresh is missed after a
  just-in-time frame (the swap interval exceeds 1.5 periods), it grows by 1 ms (max
  12 ms). After 600 consecutive good frames it shrinks by 0.25 ms (min 2 ms).

The thread sleeps with `clock_nanosleep(TIMER_ABSTIME)` until 80 µs before `start`, then
spins for the remainder, for µs-level precision (`MonotonicTimer::sleep_until_ns`).

On the UI side, after each swap the UI thread **waits for the fresh frame**
(`wait_for_jit_frame`). It is woken by `unpark` the moment the frame is published, or
gives up 1.5 ms after the planned publish time and shows the previous one. Then it
draws and swaps immediately. The frame is therefore emulated about *margin + cost* before
the refresh it appears on, instead of up to a whole frame earlier.

## Run-ahead

Most NES games read the controller during VBlank and show the result one or two frames
later. Run-ahead hides that built-in delay. Each frame:

1. emulate the real frame (its audio is kept);
2. clone the whole machine (~10–70 µs; ROM is shared, not copied);
3. emulate *n* more frames with the same input and **show that image**;
4. discard the speculative state and restore the clone.

The game's reaction therefore appears *n* frames sooner. Because emulation is
deterministic, the shown frame is exactly what the game would show *n* frames later
(enforced by a test). Cost: *n* extra frames of CPU time per frame, about 3 ms each.
That's why the just-in-time scheduler accounts for it in `cost`.

| F7 setting | Hides | Typical frame cost |
|---|---|---|
| Off | — | ~3 ms |
| 1 (default) | 1 frame (16.7 ms) | ~6 ms |
| 2 | 2 frames (33 ms) | ~9 ms |

Don't use more run-ahead than a game's actual lag. Beyond that, the extra frames start
predicting the future and motion can look "jumpy" right after inputs change. 1 frame is
safe for nearly every game.

## Input path

1. The UI thread polls the keyboard once per refresh, right after the swap, and stores
   the held buttons in `buttons_held` (an atomic). New presses are also OR'd into
   `buttons_latched`, so a tap shorter than a frame is never lost.
2. The emulated controller reads `buttons_held` **at the moment the game writes the
   `$4016` strobe**, not at frame start (`Joypad::effective_p1`). With just-in-time
   scheduling, that read happens only a few ms before the frame is shown.
3. Opposing directions (left+right, up+down) are masked, as a real D-pad can't produce
   them.

Reading `/dev/input` directly from a dedicated thread would shave a few more
milliseconds. It isn't done because it would also capture keys typed into other windows.

## Audio and dynamic rate control

`audio.rs`. Audio goes straight to the default ALSA/PipeWire device through `cpal`:

- **Device**: the device's native rate (usually 48 kHz). A ~5 ms period is requested,
  falling back to the device default.
- **Hand-off**: a lock-free single-producer/single-consumer ring buffer (16384 samples).
  The emulation thread pushes one frame's samples (~800) per frame. The callback thread
  pops them; on underrun it fades towards silence instead of clicking, and counts the
  event.
- **Dynamic rate control (DRC)**: emulation and the sound card run on different clocks,
  and display sync shifts emulation speed by up to ~1 %. Instead of dropping or
  inserting samples, the APU's output sample rate is adjusted continuously. A **PI
  controller** watches the buffer's *low-water mark* (its level just before each push):

  ```
  fill     ← fill + 0.15·(low_water − fill)                (exponential smoothing)
  err      = clamp((target − fill) / target, ±1)
  integral = clamp(integral + 0.00004·err, ±1.5 %)         (frozen for 1 s after start/flush)
  ratio    = 1 + clamp(clamp(0.004·err, ±0.5 %) + integral, ±1.5 %)
  output_rate = device_rate · ratio
  ```

  The integral term learns the constant clock offset. With a 60.00 Hz display it settles
  near +1500…+2500 ppm, the 60.0988/60 speed difference. The capped proportional term
  handles jitter without overshooting during start-up. The resulting pitch change is under
  0.03 semitones and inaudible.
- **Adaptive target**: the low-water target starts at 10 ms. Some sound servers pull
  audio in bursts much larger than the device period; PipeWire's default 1024-frame
  quantum, for example, drains ~21 ms at a time. So every underrun raises the target by
  2 ms (max 40 ms), and each 30 s without one lowers it by 0.5 ms (min 8 ms). On a
  low-quantum setup it stays near 10 ms; on a 1024-quantum PipeWire it settles around
  20–25 ms after a few early underruns.
- **Latency**: the buffer oscillates between the target (just before a push) and the
  target + ~17 ms (just after), plus the device period.
- The callback thread promotes itself to SCHED_FIFO on its first callback.
- The buffer is flushed when a ROM is loaded or unloaded and when a state is loaded.

## Latency budget

Approximate, for a game that reacts one frame after reading input, with display sync,
run-ahead 1, on a 60 Hz panel:

| Stage | Time |
|---|---|
| Key press → UI poll (after next swap) | 0–16.7 ms (avg ~8) |
| Poll → game reads `$4016` (just-in-time) | ~4–10 ms |
| Game's built-in reaction (1 frame) | hidden by run-ahead |
| Frame published → refresh it's shown on | ≈ `present_margin` (~4 ms) |
| Compositor / scan-out | display dependent (0–1 refresh) |

Compared with the original prototype (fixed 60.10 Hz timer, emulate-then-sleep, no
run-ahead), this removes roughly 25–45 ms of average latency and all periodic hitches.

## Statistics

The ROM browser (Esc) shows live statistics. `LOCKSTEP_STATS=1` prints them every 2 s:

```
[lockstep] 59.99 fps | SYNC display 59.982 Hz margin 3.8 ms | cost 6.27 ms | jitter max 93 us | audio low-water 9.2/10 ms period 240 drc +1901 ppm underruns 0 overflows 0 rt=true
```

| Field | Meaning |
|---|---|
| `fps` | real frames emulated per second |
| `SYNC` / `FREE` | pacing mode |
| `display … Hz` | measured refresh (0 = unknown) |
| `margin` | current present margin |
| `cost` | worst-case frame production time estimate |
| `jitter max` | worst lateness of a planned frame start in this interval |
| `audio low-water` | buffer level just before a push / current adaptive target |
| `period` | largest audio callback size seen, in frames |
| `drc` | current rate correction, parts per million |
| `underruns` / `overflows` | cumulative audio buffer faults |
| `rt` | audio thread obtained SCHED_FIFO |

## Tuning

In rough order of benefit:

1. **Grant real-time permissions** ([above](#permissions)).
2. **Run the display at 60 Hz** so display sync can engage. 120 Hz and 144 Hz modes fall
   back to free run. Disable VRR/FreeSync for the Lockstep window if your compositor
   applies it to windowed apps.
3. **Fullscreen (F11).** Many compositors then scan out the window directly, saving up
   to one refresh of compositing latency.
4. **CPU frequency.** Frame cost doubles when the core clocks down between frames. Use
   the `performance` governor while playing:
   `sudo cpupower frequency-set -g performance`
5. **Reserve a core.** Add to the kernel command line, using your highest core number:
   `isolcpus=7 nohz_full=7 rcu_nocbs=7 threadirqs`. Lockstep detects the isolated CPU and
   pins its emulation thread there, where nothing else runs.
6. **PipeWire quantum.** PipeWire's default quantum (often 1024 frames ≈ 21 ms) sets a
   floor on audio latency; Lockstep's adaptive target settles just above it. For lower
   latency, force a smaller quantum while playing:
   `pw-metadata -n settings 0 clock.force-quantum 256`.
7. **Run-ahead.** Use 1 for most games, 2 for games known to have 2 frames of lag, 0 to
   save CPU.

## Troubleshooting

| Symptom | Likely cause / fix |
|---|---|
| Browser shows `SCHED_OTHER` | no `rtprio` permission; see [Permissions](#permissions) |
| `Free-run 60.10 Hz (display 144.0 Hz)` | display not ~60 Hz; switch the mode, or accept free run (one repeated frame per ~2.4 s at 144 Hz is unavoidable without sync) |
| `Free-run` with no display rate | swaps aren't vsync-throttled (driver/compositor vsync off) or the window is hidden |
| A few underruns right after start, then none | normal: the adaptive target is learning your sound server's burst size |
| Audio underruns keep climbing | CPU frequency scaling or another RT process; try the `performance` governor or less run-ahead. A *smaller* PipeWire quantum (`pw-metadata -n settings 0 clock.force-quantum 256`) also lowers the achievable latency |
| Occasional single hitch, `margin` growing | compositor needs more time; the margin adapts upward automatically, or try fullscreen |
| `audio disabled: …` at start-up | no default output device; check `pactl info` / `aplay -l` |
| Game misbehaves only with F4 | the overclock breaks that game's timing assumptions; turn it off |
