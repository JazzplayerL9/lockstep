// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones

//! Real-time emulation engine: SCHED_FIFO scheduling, CLOCK_MONOTONIC pacing,
//! display-synchronised just-in-time frame scheduling, run-ahead, wait-free triple
//! buffering, dynamic-rate-controlled audio, and the ROM load/unload controller.
//!
//! Frame timing modes:
//! * **Display sync** (refresh 59.4-60.8 Hz): one emulated frame per display
//!   refresh, started just late enough to finish right before the UI must present it.
//!   No frames are ever dropped or repeated; audio absorbs the 0.1-0.3% speed difference.
//! * **Free run** (other refresh rates, window hidden): exact NTSC 60.0988 Hz pacing.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle, Thread};

use crate::audio::{AudioOut, AudioStats};
use crate::cartridge::Cartridge;
use crate::cpu::System;
use crate::ppu::{SCREEN_HEIGHT, SCREEN_WIDTH};

/// Exact NTSC NES frame duration in nanoseconds (~60.098814 Hz).
pub const NTSC_FRAME_NS: u64 = 16_639_263;
/// Size of one RGBA frame.
pub const FRAMEBUFFER_BYTES: usize = SCREEN_WIDTH * SCREEN_HEIGHT * 4;

/// Display refresh periods eligible for display-synchronised emulation.
/// 59.4-60.8 Hz: keeps game speed within ~1.2% of a real NES.
const SYNC_MIN_PERIOD_NS: u64 = 16_447_000;
/// Longest display refresh period (~59.4 Hz) eligible for display-synchronised emulation, ns.
const SYNC_MAX_PERIOD_NS: u64 = 16_835_000;
/// If the UI hasn't presented for this long (minimised/occluded), free-run instead.
const VSYNC_STALE_NS: u64 = 100_000_000;
/// Default SCHED_FIFO priority: below the kernel's threaded IRQ handlers (50), so
/// keyboard/USB and audio interrupts are never held off by emulation.
pub const DEFAULT_RT_PRIO: i32 = 45;

/// Flag in [`TripleFramebuffer::shared_state`]: the shared buffer holds a frame the
/// consumer has not yet taken.
const READY_BIT: u8 = 0x80;
/// Mask selecting the buffer index (0-2) in [`TripleFramebuffer::shared_state`].
const INDEX_MASK: u8 = 0x03;

/// Commands sent from the UI thread to the emulation worker thread.
pub enum EmulatorCommand {
    /// Power on with a new cartridge (saves the previous game's battery RAM first).
    LoadRom(Box<Cartridge>),
    /// Remove the cartridge and blank the screen.
    UnloadRom,
    /// Press the console's reset button.
    Reset,
    /// Snapshot the whole machine into the (single, in-memory) state slot.
    SaveState,
    /// Restore the snapshot taken by `SaveState`.
    LoadState,
    /// Enable/disable the extra idle scanlines before VBlank.
    SetOverclock(bool),
    /// Switch the emulation thread between SCHED_FIFO and normal scheduling.
    SetRtMode(bool),
    /// Frames of run-ahead (0-3) used to hide games' built-in input lag.
    SetRunAhead(u8),
}

/// What the host kernel and resource limits allow, probed once at start-up.
#[derive(Debug, Clone)]
pub struct RtCapabilities {
    /// `uname -r`.
    pub kernel_release: String,
    /// `uname -v` (contains `PREEMPT_RT` on real-time kernels).
    pub kernel_version: String,
    /// Running on a PREEMPT_RT kernel (`/sys/kernel/realtime` = 1).
    pub preempt_rt_kernel: bool,
    /// `RLIMIT_RTPRIO`: highest SCHED_FIFO priority this user may request (0 = none, unless root).
    pub max_rtprio: i32,
    /// CPUs reserved via `isolcpus=` (empty if none).
    pub isolated_cpus: Vec<usize>,
    /// SCHED_FIFO priority the emulation thread will request.
    pub requested_prio: i32,
}

/// Parses a Linux CPU list such as `0-3,6,8-9` (the format of
/// `/sys/devices/system/cpu/isolated`) into individual CPU numbers. Malformed parts are skipped.
fn parse_cpu_list(s: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for part in s.trim().split(',').filter(|p| !p.is_empty()) {
        if let Some((a, b)) = part.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>()) {
                out.extend(a..=b);
            }
        } else if let Ok(v) = part.parse() {
            out.push(v);
        }
    }
    out
}

/// Probes the kernel (PREEMPT_RT, `RLIMIT_RTPRIO`, isolated CPUs).
pub fn probe_rt_capabilities() -> RtCapabilities {
    let mut caps = RtCapabilities {
        kernel_release: String::new(),
        kernel_version: String::new(),
        preempt_rt_kernel: false,
        max_rtprio: 0,
        isolated_cpus: Vec::new(),
        requested_prio: DEFAULT_RT_PRIO,
    };

    #[cfg(target_os = "linux")]
    unsafe {
        let mut uts: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut uts) == 0 {
            caps.kernel_release = std::ffi::CStr::from_ptr(uts.release.as_ptr()).to_string_lossy().into_owned();
            caps.kernel_version = std::ffi::CStr::from_ptr(uts.version.as_ptr()).to_string_lossy().into_owned();
        }
        let mut rlim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        if libc::getrlimit(libc::RLIMIT_RTPRIO, &mut rlim) == 0 {
            caps.max_rtprio = (rlim.rlim_cur as i32).clamp(0, 99);
        }
    }
    // /sys/kernel/realtime exists (= 1) only on PREEMPT_RT kernels.
    caps.preempt_rt_kernel = std::fs::read_to_string("/sys/kernel/realtime").map(|s| s.trim() == "1").unwrap_or(false)
        || caps.kernel_version.contains("PREEMPT_RT");
    caps.isolated_cpus = std::fs::read_to_string("/sys/devices/system/cpu/isolated").map(|s| parse_cpu_list(&s)).unwrap_or_default();
    if let Some(p) = std::env::var("LOCKSTEP_RT_PRIO").ok().and_then(|v| v.parse::<i32>().ok()) {
        caps.requested_prio = p.clamp(1, 99);
    }
    if caps.max_rtprio > 0 {
        caps.requested_prio = caps.requested_prio.min(caps.max_rtprio);
    }
    caps
}

/// Applies or relaxes SCHED_FIFO scheduling, memory locking, timer slack and CPU pinning
/// for the calling thread. Returns the SCHED_FIFO priority obtained, if any.
pub fn apply_thread_scheduling(enable_rt: bool, caps: &RtCapabilities) -> Option<i32> {
    #[cfg(target_os = "linux")]
    unsafe {
        let num_cpus = libc::sysconf(libc::_SC_NPROCESSORS_ONLN).max(1) as usize;
        if enable_rt {
            // MCL_ONFAULT: lock pages as they are touched rather than pinning every
            // mapping in the process (GPU driver mappings etc.) up front.
            let _ = libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE | libc::MCL_ONFAULT);
            let _ = libc::prctl(libc::PR_SET_TIMERSLACK, 1, 0, 0, 0);

            // Prefer a CPU isolated with isolcpus=; otherwise the last online CPU.
            let target = caps.isolated_cpus.last().copied().filter(|&c| c < num_cpus).or(if num_cpus >= 2 { Some(num_cpus - 1) } else { None });
            if let Some(cpu) = target {
                let mut set: libc::cpu_set_t = std::mem::zeroed();
                libc::CPU_ZERO(&mut set);
                libc::CPU_SET(cpu, &mut set);
                let _ = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
            }

            for prio in [caps.requested_prio, 20, 1] {
                let param = libc::sched_param { sched_priority: prio };
                if libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) == 0 {
                    return Some(prio);
                }
            }
            let _ = libc::setpriority(libc::PRIO_PROCESS, 0, -20);
            None
        } else {
            let param = libc::sched_param { sched_priority: 0 };
            let _ = libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_OTHER, &param);
            let _ = libc::prctl(libc::PR_SET_TIMERSLACK, 50_000, 0, 0, 0);
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_ZERO(&mut set);
            for cpu in 0..num_cpus {
                libc::CPU_SET(cpu, &mut set);
            }
            let _ = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
            None
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (enable_rt, caps);
        None
    }
}

/// `CLOCK_MONOTONIC` time source with precise absolute-deadline sleeping.
pub struct MonotonicTimer;

impl MonotonicTimer {
    /// Current monotonic time in nanoseconds.
    #[inline]
    pub fn now_ns() -> u64 {
        #[cfg(target_os = "linux")]
        unsafe {
            let mut ts = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
            (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
        }
        #[cfg(not(target_os = "linux"))]
        {
            0
        }
    }

    /// Sleeps until the absolute time `target_ns`. With `rt_spin_guard`, wakes ~80 µs early and
    /// busy-waits the remainder for microsecond precision. Returns lateness in ns (negative = early).
    #[inline]
    pub fn sleep_until_ns(target_ns: u64, rt_spin_guard: bool) -> i64 {
        #[cfg(target_os = "linux")]
        unsafe {
            let spin_guard_ns = if rt_spin_guard { 80_000 } else { 0 };
            let sleep_target = target_ns.saturating_sub(spin_guard_ns);
            let now = Self::now_ns();
            if sleep_target > now {
                let ts = libc::timespec {
                    tv_sec: (sleep_target / 1_000_000_000) as libc::time_t,
                    tv_nsec: (sleep_target % 1_000_000_000) as libc::c_long,
                };
                loop {
                    let rc = libc::clock_nanosleep(
                        libc::CLOCK_MONOTONIC,
                        libc::TIMER_ABSTIME,
                        &ts,
                        std::ptr::null_mut(),
                    );
                    if rc != libc::EINTR {
                        break;
                    }
                }
            }
            if rt_spin_guard {
                while Self::now_ns() < target_ns {
                    std::hint::spin_loop();
                }
            }
            let actual = Self::now_ns();
            (actual as i128 - target_ns as i128) as i64
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (target_ns, rt_spin_guard);
            0
        }
    }
}

/// Triple buffer for 256x240 RGBA frames between one producer (the emulation thread) and
/// one consumer (the UI). The producer and consumer never wait for each other: each
/// hand-off is a single atomic swap.
pub struct TripleFramebuffer {
    /// The three frame buffers. At any instant each is owned by exactly one role: the
    /// producer's write buffer (`write_idx`), the consumer's read buffer (`read_idx`), or the
    /// shared "middle" buffer named by `shared_state`.
    buffers: [UnsafeCell<Box<[u8; FRAMEBUFFER_BYTES]>>; 3],
    /// Index (0-2, [`INDEX_MASK`]) of the shared middle buffer, plus [`READY_BIT`] when it holds
    /// a frame published since the consumer last took one.
    ///
    /// Protocol: the producer fills its write buffer, then atomically swaps
    /// `write_idx | READY_BIT` into this field and adopts the previous middle index as its new
    /// write buffer. The consumer checks [`READY_BIT`] and, if set, swaps its read index in
    /// (ready bit clear) and adopts the previous middle buffer, which is the newest frame. Each
    /// side performs a single unconditional `swap`, with no CAS retry loop or lock, so both are
    /// wait-free; and because each swap exchanges one index for another, the three indices
    /// always remain a permutation of {0, 1, 2}, so the two sides never touch the same buffer.
    shared_state: AtomicU8,
    /// Producer-private: index of the buffer the next [`TripleFramebuffer::publish_frame`]
    /// writes into. Only ever accessed by the single producer thread.
    write_idx: UnsafeCell<u8>,
    /// Consumer-private: index of the buffer last handed out by
    /// [`TripleFramebuffer::with_latest`]. Only ever accessed by the single consumer thread.
    read_idx: UnsafeCell<u8>,
    /// Serialises producers. With the normal single producer it is never contended; it
    /// exists so that two threads calling `publish_frame` can't race on `write_idx`
    /// (keeping the type sound). It never blocks the consumer.
    producer_lock: Mutex<()>,
    /// Serialises consumers, for the same reason. It never blocks the producer.
    consumer_lock: Mutex<()>,
}

// SAFETY: the buffers and the two private indices are only reached through
// `publish_frame` (under `producer_lock`) and `with_latest` (under `consumer_lock`). The
// atomic swap protocol hands each buffer to at most one side at a time, and consumer
// borrows cannot outlive `with_latest`'s closure.
unsafe impl Sync for TripleFramebuffer {}
// SAFETY: the contents are plain bytes and indices with no thread affinity.
unsafe impl Send for TripleFramebuffer {}

impl Default for TripleFramebuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl TripleFramebuffer {
    /// Three zeroed buffers; nothing published yet.
    pub fn new() -> Self {
        Self {
            buffers: [
                UnsafeCell::new(
                    vec![0u8; FRAMEBUFFER_BYTES]
                        .into_boxed_slice()
                        .try_into()
                        .unwrap(),
                ),
                UnsafeCell::new(
                    vec![0u8; FRAMEBUFFER_BYTES]
                        .into_boxed_slice()
                        .try_into()
                        .unwrap(),
                ),
                UnsafeCell::new(
                    vec![0u8; FRAMEBUFFER_BYTES]
                        .into_boxed_slice()
                        .try_into()
                        .unwrap(),
                ),
            ],
            shared_state: AtomicU8::new(1),
            write_idx: UnsafeCell::new(0),
            read_idx: UnsafeCell::new(2),
            producer_lock: Mutex::new(()),
            consumer_lock: Mutex::new(()),
        }
    }

    /// Producer: copies a finished frame in and makes it the latest. Never blocks.
    #[inline]
    pub fn publish_frame(&self, src: &[u8; FRAMEBUFFER_BYTES]) {
        let _guard = self.producer_lock.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the producer lock gives exclusive access to `write_idx` and to the write
        // buffer it names, which the swap protocol guarantees no consumer is reading.
        unsafe {
            let w = *self.write_idx.get();
            let dst = &mut *self.buffers[w as usize].get();
            dst.copy_from_slice(src);
            let prev_shared = self.shared_state.swap(w | READY_BIT, Ordering::AcqRel);
            *self.write_idx.get() = prev_shared & INDEX_MASK;
        }
    }

    /// Consumer: if a frame was published since the last call, runs `f` on the newest one
    /// and returns its result; otherwise returns `None`. Never waits for the producer.
    ///
    /// The frame is only borrowed for the duration of `f`, so it can't be held while the
    /// producer recycles the buffer.
    #[inline]
    pub fn with_latest<R>(&self, f: impl FnOnce(&[u8; FRAMEBUFFER_BYTES]) -> R) -> Option<R> {
        let _guard = self.consumer_lock.lock().unwrap_or_else(|e| e.into_inner());
        let current = self.shared_state.load(Ordering::Acquire);
        if (current & READY_BIT) == 0 {
            return None;
        }
        // SAFETY: the consumer lock gives exclusive access to `read_idx`; after the swap
        // the buffer at `new_r` belongs to the consumer alone until its next swap, which
        // can't happen while `_guard` (and therefore the borrow passed to `f`) is alive.
        unsafe {
            let r = *self.read_idx.get();
            let prev_shared = self.shared_state.swap(r, Ordering::AcqRel);
            let new_r = prev_shared & INDEX_MASK;
            *self.read_idx.get() = new_r;
            Some(f(&*self.buffers[new_r as usize].get()))
        }
    }
}

/// State shared between the UI thread and the emulation worker. Everything is atomics or
/// wait-free structures, so neither thread ever blocks on the other.
pub struct RtSharedState {
    /// Host real-time capabilities.
    pub caps: RtCapabilities,
    /// Finished frames, worker → UI.
    pub framebuffer: TripleFramebuffer,
    /// Held buttons, read by the emulated controller at strobe time.
    pub buttons_held: Arc<AtomicU8>,
    /// Buttons pressed since the last emulated frame (so sub-frame taps aren't lost).
    pub buttons_latched: AtomicU8,
    /// Cleared to stop the worker.
    pub running: AtomicBool,
    /// A cartridge is inserted and running.
    pub rom_loaded: AtomicBool,
    /// The worker is (asked to be) running with SCHED_FIFO.
    pub rt_mode_enabled: AtomicBool,
    /// SCHED_FIFO priority actually obtained (0 = normal scheduling).
    pub sched_fifo_prio: AtomicI32,
    /// Overclock scanlines enabled.
    pub overclock_enabled: AtomicBool,
    /// Run-ahead frames in use (0 = off).
    pub run_ahead: AtomicU8,
    /// Real (non-speculative) frames emulated.
    pub frames_executed: AtomicU64,
    /// Lateness of the most recent frame start, ns.
    pub last_jitter_ns: AtomicI64,
    /// Worst absolute frame-start lateness since the last reset, ns.
    pub max_abs_jitter_ns: AtomicU64,
    /// Incremented each time a frame is published.
    pub frame_seq: AtomicU64,
    // Display synchronisation (written by the UI thread).
    /// When the UI thread last returned from a buffer swap (≈ a display refresh).
    pub vsync_ns: AtomicU64,
    /// Measured display refresh period, ns (0 = unknown / not vsync-throttled).
    pub display_period_ns: AtomicU64,
    /// How long before the next refresh a frame must be ready (adapted by [`DisplayClock`]).
    pub present_margin_ns: AtomicU64,
    /// Worker is currently scheduling frames against the display refresh.
    pub display_sync_active: AtomicBool,
    /// Estimated worst-case time to produce a frame (emulation + run-ahead + publish).
    pub frame_cost_ns: AtomicU64,
    /// Audio output statistics, once the device is open.
    pub audio_stats: OnceLock<Arc<AudioStats>>,
    /// Handle of the UI thread (set by [`RtSharedState::register_ui_thread`]), unparked when a
    /// frame is published so [`RtSharedState::wait_for_jit_frame`] wakes promptly.
    ui_thread: Mutex<Option<Thread>>,
}

impl RtSharedState {
    /// Probes capabilities; RT mode defaults on when SCHED_FIFO is permitted.
    pub fn new() -> Self {
        let caps = probe_rt_capabilities();
        let default_rt = caps.preempt_rt_kernel || caps.max_rtprio > 0;
        Self {
            caps,
            framebuffer: TripleFramebuffer::new(),
            buttons_held: Arc::new(AtomicU8::new(0)),
            buttons_latched: AtomicU8::new(0),
            running: AtomicBool::new(true),
            rom_loaded: AtomicBool::new(false),
            rt_mode_enabled: AtomicBool::new(default_rt),
            sched_fifo_prio: AtomicI32::new(0),
            overclock_enabled: AtomicBool::new(false),
            run_ahead: AtomicU8::new(1),
            frames_executed: AtomicU64::new(0),
            last_jitter_ns: AtomicI64::new(0),
            max_abs_jitter_ns: AtomicU64::new(0),
            frame_seq: AtomicU64::new(0),
            vsync_ns: AtomicU64::new(0),
            display_period_ns: AtomicU64::new(0),
            present_margin_ns: AtomicU64::new(4_000_000),
            display_sync_active: AtomicBool::new(false),
            frame_cost_ns: AtomicU64::new(6_000_000),
            audio_stats: OnceLock::new(),
            ui_thread: Mutex::new(None),
        }
    }

    /// UI thread: publishes the controller state. `newly_pressed_mask` latches presses so taps
    /// shorter than a frame still register.
    #[inline]
    pub fn update_input(&self, held_mask: u8, newly_pressed_mask: u8) {
        self.buttons_held.store(held_mask, Ordering::Release);
        if newly_pressed_mask != 0 {
            self.buttons_latched.fetch_or(newly_pressed_mask, Ordering::AcqRel);
        }
    }

    /// Registers the calling thread as the UI thread (woken when a frame is published).
    pub fn register_ui_thread(&self) {
        *self.ui_thread.lock().unwrap() = Some(thread::current());
    }

    /// UI side: after a refresh, wait (bounded) for the frame the worker is producing
    /// just-in-time for the next one. Returns true if a fresh frame arrived.
    pub fn wait_for_jit_frame(&self, seq_before: u64) -> bool {
        if !self.display_sync_active.load(Ordering::Acquire) || !self.rom_loaded.load(Ordering::Acquire) {
            return false;
        }
        let vs = self.vsync_ns.load(Ordering::Acquire);
        let period = self.display_period_ns.load(Ordering::Acquire);
        let margin = self.present_margin_ns.load(Ordering::Acquire);
        // Grace period past the planned publish time before giving up on this refresh.
        let deadline = vs + period - margin + 1_500_000;
        loop {
            if self.frame_seq.load(Ordering::Acquire) != seq_before {
                return true;
            }
            let now = MonotonicTimer::now_ns();
            if now >= deadline {
                return false;
            }
            thread::park_timeout(std::time::Duration::from_nanos((deadline - now).min(2_000_000)));
        }
    }

    /// Worker side, after publishing a frame: bumps `frame_seq` and unparks the registered UI
    /// thread (if any).
    fn notify_frame(&self) {
        self.frame_seq.fetch_add(1, Ordering::AcqRel);
        if let Some(t) = self.ui_thread.lock().unwrap().as_ref() {
            t.unpark();
        }
    }
}

impl Default for RtSharedState {
    fn default() -> Self {
        Self::new()
    }
}

/// UI-thread helper that measures the display refresh from present timestamps and
/// adapts the just-in-time presentation margin.
pub struct DisplayClock {
    /// Timestamp of the previous [`DisplayClock::tick`], ns (`CLOCK_MONOTONIC`; 0 = none yet).
    last: u64,
    /// Sliding window of the last (up to 64) present-to-present intervals, ns. A period
    /// estimate is produced once at least 16 are collected.
    intervals: Vec<u64>,
    /// Consecutive just-in-time presents that did not miss a refresh. After 600 of them the
    /// presentation margin is lowered by 0.25 ms; reset whenever a refresh is missed.
    good_frames: u32,
    /// Smoothed refresh period estimate, ns (0.0 = none yet). Mean of the middle half of the
    /// sorted window, low-pass filtered with a 2% step; re-seeded directly when the new mean
    /// differs by more than 2%. Published as `display_period_ns` only while the cadence is stable.
    smoothed: f64,
}

impl Default for DisplayClock {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayClock {
    /// No measurements yet.
    pub fn new() -> Self {
        Self { last: 0, intervals: Vec::with_capacity(64), good_frames: 0, smoothed: 0.0 }
    }

    /// Call right after each buffer swap returns.
    pub fn tick(&mut self, shared: &RtSharedState, jit_waited: bool) {
        let now = MonotonicTimer::now_ns();
        if self.last != 0 {
            let dt = now - self.last;
            if self.intervals.len() == 64 {
                self.intervals.remove(0);
            }
            self.intervals.push(dt);
            if self.intervals.len() >= 16 {
                let mut sorted = self.intervals.clone();
                sorted.sort_unstable();
                let med = sorted[sorted.len() / 2];
                let lo = sorted[sorted.len() / 8];
                let hi = sorted[sorted.len() * 7 / 8];
                // Only trust a stable, vsync-throttled cadence. The refresh rate itself is
                // constant, so average the middle half and smooth heavily over time.
                let stable = hi - lo < med / 10;
                let mid = &sorted[sorted.len() / 4..sorted.len() * 3 / 4];
                let mean = mid.iter().sum::<u64>() as f64 / mid.len() as f64;
                self.smoothed = if self.smoothed == 0.0 || (mean - self.smoothed).abs() > self.smoothed * 0.02 {
                    mean
                } else {
                    self.smoothed + (mean - self.smoothed) * 0.02
                };
                let period = if stable { self.smoothed as u64 } else { 0 };
                shared.display_period_ns.store(period, Ordering::Release);

                // Adapt the presentation margin: a missed refresh after a JIT wait means
                // the frame reached the compositor too late.
                if jit_waited && period > 0 {
                    let margin = shared.present_margin_ns.load(Ordering::Acquire);
                    if dt > period * 3 / 2 {
                        shared.present_margin_ns.store((margin + 1_000_000).min(12_000_000), Ordering::Release);
                        self.good_frames = 0;
                    } else {
                        self.good_frames += 1;
                        if self.good_frames >= 600 {
                            self.good_frames = 0;
                            shared.present_margin_ns.store(margin.saturating_sub(250_000).max(2_000_000), Ordering::Release);
                        }
                    }
                }
            }
        }
        self.last = now;
        shared.vsync_ns.store(now, Ordering::Release);
    }
}

/// Starts the emulation thread. Returns the command channel and the thread handle; set
/// `shared.running` to false to stop it.
pub fn spawn_emulator_worker(shared: Arc<RtSharedState>) -> (Sender<EmulatorCommand>, JoinHandle<()>) {
    let (cmd_tx, cmd_rx): (Sender<EmulatorCommand>, Receiver<EmulatorCommand>) = channel();

    // Emulator state is heap-allocated; the generous stack is just headroom.
    let handle = thread::Builder::new()
        .name("lockstep-emu".to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || worker_main(shared, cmd_rx))
        .expect("failed to spawn the lockstep-emu thread");

    (cmd_tx, handle)
}

/// Body of the emulation thread. Sets up audio (one SCHED_FIFO priority step above
/// emulation) and thread scheduling, then loops until `shared.running` is cleared:
/// drains [`EmulatorCommand`]s; picks the next frame start (in display-sync mode, the
/// latest start that still finishes `present_margin_ns` before the earliest refresh not
/// already targeted, using the decaying worst-case `cost_est`; otherwise fixed
/// [`NTSC_FRAME_NS`] steps, resynchronised if more than 5 frames behind); sleeps until then;
/// runs one frame (with run-ahead), pushes audio, publishes the frame and notifies the UI.
/// Battery RAM is saved every 600 frames, on ROM change/unload and on exit.
fn worker_main(shared: Arc<RtSharedState>, cmd_rx: Receiver<EmulatorCommand>) {
    let mut rt_enabled = shared.rt_mode_enabled.load(Ordering::Acquire);
    // Audio callback runs one priority step above emulation: an audio underrun is
    // more audible than a late video frame.
    let audio_prio = if rt_enabled { (shared.caps.requested_prio + 1).min(99) } else { 0 };
    let mut audio = AudioOut::new(audio_prio);
    let _ = shared.audio_stats.set(audio.stats.clone());

    let prio = apply_thread_scheduling(rt_enabled, &shared.caps);
    shared.sched_fifo_prio.store(prio.unwrap_or(0), Ordering::Release);
    eprintln!(
        "[lockstep] kernel={} (PREEMPT_RT={}) | mode={} | SCHED_FIFO={:?} | isolated={:?} | audio {} Hz",
        shared.caps.kernel_release,
        shared.caps.preempt_rt_kernel,
        if rt_enabled { "RT" } else { "Standard" },
        prio,
        shared.caps.isolated_cpus,
        audio.sample_rate
    );

    let mut sys: Option<Box<System>> = None;
    let mut saved: Option<Box<System>> = None;
    let mut overclock = shared.overclock_enabled.load(Ordering::Acquire);
    let mut output_rate = audio.sample_rate;
    let mut next_deadline = MonotonicTimer::now_ns() + NTSC_FRAME_NS;
    let mut last_target_vblank = 0u64;
    let mut cost_est = shared.frame_cost_ns.load(Ordering::Relaxed);
    let mut display_fb = vec![0u8; FRAMEBUFFER_BYTES];
    let stats_enabled = std::env::var_os("LOCKSTEP_STATS").is_some();
    let mut stats_last = MonotonicTimer::now_ns();
    let mut stats_frames = 0u64;
    let mut stats_max_jitter = 0u64;

    while shared.running.load(Ordering::Acquire) {
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                EmulatorCommand::LoadRom(cart) => {
                    if let Some(prev) = &sys {
                        prev.save_sram();
                    }
                    saved = None;
                    match System::new(*cart) {
                        Ok(s) => {
                            let mut s = Box::new(s);
                            s.set_overclock(overclock);
                            s.joypad.live_p1 = Some(shared.buttons_held.clone());
                            sys = Some(s);
                            shared.rom_loaded.store(true, Ordering::Release);
                        }
                        Err(e) => {
                            eprintln!("[lockstep] {}", e);
                            sys = None;
                            shared.rom_loaded.store(false, Ordering::Release);
                        }
                    }
                    audio.flush();
                    shared.max_abs_jitter_ns.store(0, Ordering::Release);
                    next_deadline = MonotonicTimer::now_ns() + NTSC_FRAME_NS;
                }
                EmulatorCommand::UnloadRom => {
                    if let Some(prev) = &sys {
                        prev.save_sram();
                    }
                    sys = None;
                    saved = None;
                    shared.rom_loaded.store(false, Ordering::Release);
                    let blank = vec![0u8; FRAMEBUFFER_BYTES];
                    shared.framebuffer.publish_frame((&blank[..]).try_into().unwrap());
                    shared.notify_frame();
                    audio.flush();
                }
                EmulatorCommand::Reset => {
                    if let Some(s) = &mut sys {
                        s.reset();
                    }
                }
                EmulatorCommand::SaveState => {
                    if let Some(s) = &sys {
                        saved = Some(s.clone());
                    }
                }
                EmulatorCommand::LoadState => {
                    if let Some(snap) = &saved {
                        let mut restored = snap.clone();
                        restored.set_overclock(overclock);
                        sys = Some(restored);
                        audio.flush();
                    }
                }
                EmulatorCommand::SetOverclock(enabled) => {
                    overclock = enabled;
                    shared.overclock_enabled.store(enabled, Ordering::Release);
                    if let Some(s) = &mut sys {
                        s.set_overclock(enabled);
                    }
                }
                EmulatorCommand::SetRunAhead(n) => shared.run_ahead.store(n.min(3), Ordering::Release),
                EmulatorCommand::SetRtMode(enable) => {
                    rt_enabled = enable;
                    shared.rt_mode_enabled.store(enable, Ordering::Release);
                    let p = apply_thread_scheduling(rt_enabled, &shared.caps);
                    shared.sched_fifo_prio.store(p.unwrap_or(0), Ordering::Release);
                    shared.max_abs_jitter_ns.store(0, Ordering::Release);
                    next_deadline = MonotonicTimer::now_ns() + NTSC_FRAME_NS;
                }
            }
        }

        // ---- Decide when to start the next frame.
        let now = MonotonicTimer::now_ns();
        let period = shared.display_period_ns.load(Ordering::Acquire);
        let vsync = shared.vsync_ns.load(Ordering::Acquire);
        let sync = sys.is_some()
            && (SYNC_MIN_PERIOD_NS..=SYNC_MAX_PERIOD_NS).contains(&period)
            && now.saturating_sub(vsync) < VSYNC_STALE_NS;
        shared.display_sync_active.store(sync, Ordering::Release);

        let planned_start = if sync {
            let margin = shared.present_margin_ns.load(Ordering::Acquire);
            let lead = margin + cost_est + cost_est / 8 + 500_000;
            // Earliest upcoming refresh we can still make, never serving one twice.
            let mut target = vsync + period;
            while target < now + lead || target < last_target_vblank + period / 2 {
                target += period;
            }
            last_target_vblank = target;
            next_deadline = target.saturating_sub(lead);
            next_deadline
        } else {
            next_deadline
        };

        let jitter = MonotonicTimer::sleep_until_ns(planned_start, rt_enabled);
        shared.last_jitter_ns.store(jitter, Ordering::Relaxed);
        shared.max_abs_jitter_ns.fetch_max(jitter.unsigned_abs(), Ordering::Relaxed);

        // ---- Produce the frame.
        let t0 = MonotonicTimer::now_ns();
        if let Some(s) = &mut sys {
            s.joypad.buttons_p1 = shared.buttons_latched.swap(0, Ordering::AcqRel);
            s.apu.output_rate = output_rate;
            let ahead = shared.run_ahead.load(Ordering::Acquire);
            s.run_frame_ahead(ahead, &mut display_fb);
            output_rate = audio.push(&mut s.apu.audio_samples);
            shared.framebuffer.publish_frame((&display_fb[..]).try_into().unwrap());
            shared.notify_frame();

            let n = shared.frames_executed.fetch_add(1, Ordering::Relaxed);
            if n % 600 == 599 {
                // Persist battery RAM every ~10 s so a crash doesn't lose saves.
                s.save_sram();
            }
            // Cost estimate: jump up immediately, decay slowly (tracks the worst case).
            let cost = MonotonicTimer::now_ns() - t0;
            cost_est = if cost > cost_est { cost } else { cost_est - (cost_est - cost) / 64 };
            shared.frame_cost_ns.store(cost_est, Ordering::Relaxed);
        } else {
            let _ = shared.buttons_latched.swap(0, Ordering::AcqRel);
        }

        if stats_enabled && sys.is_some() {
            stats_frames += 1;
            stats_max_jitter = stats_max_jitter.max(jitter.unsigned_abs());
            let now = MonotonicTimer::now_ns();
            if now - stats_last >= 2_000_000_000 {
                let a = &audio.stats;
                eprintln!(
                    "[lockstep] {:.2} fps | {} display {:.3} Hz margin {:.1} ms | cost {:.2} ms | jitter max {} us | audio low-water {:.1}/{:.0} ms period {} drc {:+} ppm underruns {} overflows {} rt={}",
                    stats_frames as f64 * 1e9 / (now - stats_last) as f64,
                    if sync { "SYNC" } else { "FREE" },
                    if period > 0 { 1e9 / period as f64 } else { 0.0 },
                    shared.present_margin_ns.load(Ordering::Relaxed) as f64 / 1e6,
                    cost_est as f64 / 1e6,
                    stats_max_jitter / 1000,
                    a.low_water_us.load(Ordering::Relaxed) as f64 / 1000.0,
                    a.target_us.load(Ordering::Relaxed) as f64 / 1000.0,
                    a.device_period_frames.load(Ordering::Relaxed),
                    a.drc_ppm.load(Ordering::Relaxed) as i64,
                    a.underruns.load(Ordering::Relaxed),
                    a.overflows.load(Ordering::Relaxed),
                    a.rt_promoted.load(Ordering::Relaxed),
                );
                stats_last = now;
                stats_frames = 0;
                stats_max_jitter = 0;
            }
        }

        if !sync {
            next_deadline += NTSC_FRAME_NS;
            let now = MonotonicTimer::now_ns();
            if now > next_deadline + NTSC_FRAME_NS * 5 {
                next_deadline = now + NTSC_FRAME_NS;
            }
        }
    }

    if let Some(s) = &sys {
        s.save_sram();
    }
}
