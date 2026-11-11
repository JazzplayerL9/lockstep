// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones

//! Low-latency audio output: a lock-free single-producer/single-consumer ring buffer
//! feeding a cpal (ALSA / PipeWire) stream, plus dynamic rate control (DRC).
//!
//! The emulator and the sound card run on different clocks, and when video is locked
//! to the display refresh the emulator runs ~0.1-0.3% off NES speed. Rather than
//! dropping or duplicating buffers (clicks), DRC nudges the APU's output sample rate by
//! up to ±0.5% so the device buffer hovers at its target level indefinitely.

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Initial target for the buffer's low point (measured just before each frame's samples
/// are queued). Total latency swings between the target and the target + one video frame.
/// The target adapts: see [`TARGET_STEP_UP_MS`].
const TARGET_LOW_WATER_MS: f64 = 10.0;
/// Lower bound of the adaptive low-water target.
const TARGET_MIN_MS: f64 = 8.0;
/// Upper bound of the adaptive low-water target.
const TARGET_MAX_MS: f64 = 40.0;
/// Target increase after an underrun. Sound servers that pull audio in large bursts
/// (e.g. PipeWire with a 1024-frame quantum) need more headroom than the device period
/// suggests; this finds the right amount automatically.
const TARGET_STEP_UP_MS: f64 = 2.0;
/// Target decrease after [`TARGET_RELAX_PUSHES`] consecutive underrun-free frames.
const TARGET_STEP_DOWN_MS: f64 = 0.5;
/// Underrun-free frames (~30 s) before the target is lowered again.
const TARGET_RELAX_PUSHES: u32 = 1800;
/// Frames after start-up or a flush during which the integral term is frozen, so the
/// start-up transient can't wind it up (anti-windup).
const DRC_SETTLE_PUSHES: u32 = 60;
/// Maximum rate adjustment applied by DRC (covers display-sync speed offsets up to ~1.2%).
const MAX_RATE_DELTA: f64 = 0.015;
/// Maximum contribution of the proportional term. The integral term carries the steady
/// clock offset; limiting P keeps start-up transients from overshooting into underruns.
const MAX_P_DELTA: f64 = 0.005;
/// Weight of each new low-water sample in the smoothed fill level (per video frame).
const DRC_SMOOTHING: f64 = 0.15;
/// Proportional / integral gains of the buffer-level controller (per video frame).
const DRC_KP: f64 = 0.004;
/// Integral gain of the buffer-level controller (per video frame); see `DRC_KP`.
const DRC_KI: f64 = 0.00004;

/// Fixed-capacity, lock-free single-producer/single-consumer ring buffer of mono samples.
///
/// `head` and `tail` are free-running counters (they wrap at `usize::MAX`, not at the
/// capacity) masked down to an index into `buf`; their wrapping difference is the fill
/// level. Only the producer stores `head` and only the consumer stores `tail`. Each side
/// writes or reads its slots first and then publishes its index with a Release store,
/// and loads the other side's index with Acquire, so a slot is never accessed by both
/// sides at once and each slot's write happens-before the other side reads it.
struct Ring {
    /// Sample storage (length is a power of two). `UnsafeCell` because slots are written
    /// through a shared reference.
    buf: Box<[UnsafeCell<f32>]>,
    /// `capacity - 1`, for wrapping a counter into an index into `buf`.
    mask: usize,
    /// Total samples ever pushed (free-running); stored only by the producer.
    head: AtomicUsize, // written by producer
    /// Total samples ever popped (free-running); stored only by the consumer.
    tail: AtomicUsize, // written by consumer
}

// SAFETY: single producer (emulation thread) / single consumer (audio callback); each
// slot is only accessed by one side at a time, as delimited by head/tail.
unsafe impl Sync for Ring {}
unsafe impl Send for Ring {}

impl Ring {
    /// Creates an empty, zeroed ring. `capacity_pow2` must be a power of two (not checked).
    fn new(capacity_pow2: usize) -> Self {
        assert!(capacity_pow2.is_power_of_two(), "ring capacity must be a power of two");
        let buf = (0..capacity_pow2).map(|_| UnsafeCell::new(0.0)).collect::<Vec<_>>().into_boxed_slice();
        Self { buf, mask: capacity_pow2 - 1, head: AtomicUsize::new(0), tail: AtomicUsize::new(0) }
    }

    /// Number of buffered samples. A snapshot: the other side may change it immediately.
    fn len(&self) -> usize {
        self.head.load(Ordering::Acquire).wrapping_sub(self.tail.load(Ordering::Acquire))
    }

    /// Producer side: copies as many of `samples` as fit and returns how many were written
    /// (the rest are dropped). Must only be called from the single producer thread; the slots
    /// from `head` up to `tail + capacity` belong to the producer until `head` is published.
    fn push(&self, samples: &[f32]) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let free = self.buf.len() - head.wrapping_sub(tail);
        let n = samples.len().min(free);
        for (i, &s) in samples[..n].iter().enumerate() {
            unsafe { *self.buf[(head + i) & self.mask].get() = s };
        }
        self.head.store(head.wrapping_add(n), Ordering::Release);
        n
    }

    /// Consumer side: discards every sample written before the producer's `head` reached
    /// `upto`, without touching samples pushed after that point. No allocation.
    fn discard_until(&self, upto: usize) {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        // Only move forward, and never past what has actually been written.
        let skip = upto.wrapping_sub(tail).min(head.wrapping_sub(tail));
        if upto.wrapping_sub(tail) <= self.buf.len() {
            self.tail.store(tail.wrapping_add(skip), Ordering::Release);
        }
    }

    /// Consumer side: moves up to `out.len()` samples into `out` and returns how many were
    /// read. Must only be called from the single consumer thread; the slots from `tail` up to
    /// `head` belong to the consumer until `tail` is published.
    fn pop(&self, out: &mut [f32]) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let n = out.len().min(head.wrapping_sub(tail));
        for (i, o) in out[..n].iter_mut().enumerate() {
            *o = unsafe { *self.buf[(tail + i) & self.mask].get() };
        }
        self.tail.store(tail.wrapping_add(n), Ordering::Release);
        n
    }
}

/// Statistics readable from any thread.
pub struct AudioStats {
    /// Device callbacks that found the buffer short (each is an audible glitch).
    pub underruns: AtomicU64,
    /// Pushes that didn't fit in the ring buffer (samples dropped).
    pub overflows: AtomicU64,
    /// Buffered audio in microseconds right after the last push (high-water mark).
    pub buffered_us: AtomicU64,
    /// Buffered audio just before the last push (low-water mark).
    pub low_water_us: AtomicU64,
    /// Current DRC ratio in parts-per-million offset (+ = producing faster).
    pub drc_ppm: AtomicU64,
    /// Whether the audio callback thread obtained SCHED_FIFO priority.
    pub rt_promoted: AtomicBool,
    /// Largest number of frames the device has requested in one callback (its period).
    pub device_period_frames: AtomicUsize,
    /// Current adaptive low-water target, microseconds.
    pub target_us: AtomicU64,
    /// Pending flush: the producer's `head` position at the time of `AudioOut::flush`, or
    /// `usize::MAX` for none. The audio callback discards samples up to that point only, so
    /// audio pushed after the flush survives (only the consumer may advance `tail`).
    flush_to: AtomicUsize,
}

/// Consumer side, owned by the audio callback.
struct Feeder {
    /// Shared ring buffer (consumer end).
    ring: Arc<Ring>,
    /// Shared statistics; also carries the flush request.
    stats: Arc<AudioStats>,
    /// Reusable buffer holding one callback's worth of mono samples.
    scratch: Vec<f32>,
    /// Last sample delivered, used to fade out smoothly on underrun and to tell real
    /// underruns from idle silence.
    last: f32,
    /// Whether promoting the callback thread has been attempted. Done once, from the first
    /// callback, because cpal creates the thread.
    promoted: bool,
    /// SCHED_FIFO priority to request for the callback thread; 0 or less disables promotion.
    rt_prio: i32,
}

impl Feeder {
    /// Produces `n_frames` mono samples for one device callback. Promotes the thread on the
    /// first call, services a pending flush, then pops from the ring. On underrun the gap is
    /// filled with an exponentially decaying continuation of the last sample; it is only
    /// counted as an underrun if audio was actually playing (some samples arrived, or the last
    /// sample wasn't near silence).
    fn fill(&mut self, n_frames: usize) -> &[f32] {
        if !self.promoted {
            self.promoted = true;
            if self.rt_prio > 0 && promote_current_thread(self.rt_prio) {
                self.stats.rt_promoted.store(true, Ordering::Relaxed);
            }
        }
        let flush_to = self.stats.flush_to.swap(usize::MAX, Ordering::AcqRel);
        if flush_to != usize::MAX {
            self.ring.discard_until(flush_to);
        }
        if n_frames == 0 {
            return &[];
        }
        self.stats.device_period_frames.fetch_max(n_frames, Ordering::Relaxed);
        // `scratch` is pre-allocated, so this only allocates if the device asks for an
        // unusually large period.
        self.scratch.resize(n_frames, 0.0);
        let got = self.ring.pop(&mut self.scratch);
        if got < n_frames {
            if got > 0 || self.last.abs() > 1e-4 {
                self.stats.underruns.fetch_add(1, Ordering::Relaxed);
            }
            // Fade toward silence instead of clicking.
            let mut v = if got > 0 { self.scratch[got - 1] } else { self.last };
            for s in &mut self.scratch[got..] {
                v *= 0.995;
                *s = v;
            }
        }
        self.last = self.scratch[n_frames - 1];
        &self.scratch[..]
    }
}

impl Default for AudioStats {
    fn default() -> Self {
        Self {
            underruns: AtomicU64::new(0),
            overflows: AtomicU64::new(0),
            buffered_us: AtomicU64::new(0),
            low_water_us: AtomicU64::new(0),
            drc_ppm: AtomicU64::new(0),
            rt_promoted: AtomicBool::new(false),
            device_period_frames: AtomicUsize::new(0),
            target_us: AtomicU64::new((TARGET_LOW_WATER_MS * 1000.0) as u64),
            flush_to: AtomicUsize::new(usize::MAX),
        }
    }
}

/// Producer-side handle owned by the emulation thread. Dropping it stops playback.
pub struct AudioOut {
    /// Shared ring buffer (producer end).
    ring: Arc<Ring>,
    /// The running cpal stream (dropping it stops playback), or `None` if no device could be
    /// opened, in which case pushed samples are discarded.
    _stream: Option<cpal::Stream>,
    /// The device's native sample rate (Hz).
    pub sample_rate: f64,
    /// Live statistics, shareable with the UI.
    pub stats: Arc<AudioStats>,
    /// Exponentially smoothed low-water fill level in samples (weight `DRC_SMOOTHING` per push): the
    /// measured value fed to the DRC controller.
    smoothed_fill: f64,
    /// Integral term of the DRC PI controller: accumulated error times `DRC_KI`, clamped to
    /// +/-`MAX_RATE_DELTA`.
    integral: f64,
    /// Whether the ring has been pre-filled to the target level since opening or the last flush.
    started: bool,
    /// Current adaptive low-water target, ms.
    target_ms: f64,
    /// Underrun count seen at the previous push (to detect new underruns).
    last_underruns: u64,
    /// Consecutive pushes without an underrun.
    clean_pushes: u32,
    /// Pushes since start-up or the last flush (for anti-windup).
    pushes_since_start: u32,
}

/// Promote the calling thread to SCHED_FIFO at `prio` on Linux, or `THREAD_PRIORITY_TIME_CRITICAL`
/// on Windows (best effort).
pub fn promote_current_thread(prio: i32) -> bool {
    #[cfg(target_os = "linux")]
    unsafe {
        let param = libc::sched_param { sched_priority: prio };
        libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) == 0
    }
    #[cfg(target_os = "windows")]
    unsafe {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            /// Returns a pseudo-handle for the calling thread.
            fn GetCurrentThread() -> *mut std::ffi::c_void;
            /// Sets the scheduling priority of `hThread`.
            fn SetThreadPriority(hThread: *mut std::ffi::c_void, nPriority: i32) -> i32;
        }
        let _ = prio;
        // THREAD_PRIORITY_TIME_CRITICAL = 15
        SetThreadPriority(GetCurrentThread(), 15) != 0
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = prio;
        false
    }
}

impl AudioOut {
    /// Opens the default output device. `rt_prio` > 0 promotes the callback thread to SCHED_FIFO.
    pub fn new(rt_prio: i32) -> Self {
        let ring = Arc::new(Ring::new(1 << 14));
        let stats = Arc::new(AudioStats::default());
        match Self::open(ring.clone(), stats.clone(), rt_prio) {
            Ok((stream, rate)) => Self { ring, _stream: Some(stream), sample_rate: rate, stats, smoothed_fill: 0.0, integral: 0.0, started: false, target_ms: TARGET_LOW_WATER_MS, last_underruns: 0, clean_pushes: 0, pushes_since_start: 0 },
            Err(e) => {
                eprintln!("[lockstep] audio disabled: {e}");
                Self { ring, _stream: None, sample_rate: crate::apu::DEFAULT_SAMPLE_RATE, stats, smoothed_fill: 0.0, integral: 0.0, started: false, target_ms: TARGET_LOW_WATER_MS, last_underruns: 0, clean_pushes: 0, pushes_since_start: 0 }
            }
        }
    }

    /// Opens the default output device with its default config and starts a stream that pulls
    /// from `ring` through a `Feeder`, copying the mono signal to every channel and converting
    /// to the device's sample format (f32, i16 or i32). Requests a ~5 ms fixed buffer first and
    /// falls back to the device default. Returns the stream and its sample rate in Hz.
    fn open(ring: Arc<Ring>, stats: Arc<AudioStats>, rt_prio: i32) -> Result<(cpal::Stream, f64), String> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no output device")?;
        let def = device.default_output_config().map_err(|e| e.to_string())?;
        let channels = def.channels() as usize;
        let rate = def.sample_rate();
        let fmt = def.sample_format();
        let mut cfg: cpal::StreamConfig = def.into();

        let build = |cfg: &cpal::StreamConfig| -> Result<cpal::Stream, String> {
            let mut feeder = Feeder { ring: ring.clone(), stats: stats.clone(), scratch: Vec::with_capacity(8192), last: 0.0, promoted: false, rt_prio };
            let err = |e: cpal::Error| eprintln!("[lockstep] audio: {e}");
            match fmt {
                cpal::SampleFormat::F32 => device.build_output_stream(
                    cfg.clone(),
                    move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        let src = feeder.fill(data.len() / channels);
                        for (frame, &s) in data.chunks_mut(channels).zip(src) {
                            frame.fill(s);
                        }
                    },
                    err,
                    None,
                ),
                cpal::SampleFormat::I16 => device.build_output_stream(
                    cfg.clone(),
                    move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                        let src = feeder.fill(data.len() / channels);
                        for (frame, &s) in data.chunks_mut(channels).zip(src) {
                            frame.fill((s * 32767.0) as i16);
                        }
                    },
                    err,
                    None,
                ),
                cpal::SampleFormat::I32 => device.build_output_stream(
                    cfg.clone(),
                    move |data: &mut [i32], _: &cpal::OutputCallbackInfo| {
                        let src = feeder.fill(data.len() / channels);
                        for (frame, &s) in data.chunks_mut(channels).zip(src) {
                            frame.fill((s as f64 * 2147483647.0) as i32);
                        }
                    },
                    err,
                    None,
                ),
                other => return Err(format!("unsupported sample format {other:?}")),
            }
            .map_err(|e| e.to_string())
        };

        // Ask for a small device period (~5 ms); fall back to the device default.
        cfg.buffer_size = cpal::BufferSize::Fixed((rate / 200).max(64));
        let stream = match build(&cfg) {
            Ok(s) => s,
            Err(_) => {
                cfg.buffer_size = cpal::BufferSize::Default;
                build(&cfg)?
            }
        };
        stream.play().map_err(|e| e.to_string())?;
        Ok((stream, rate as f64))
    }

    /// Queues samples and returns the output rate the APU should use next (DRC).
    pub fn push(&mut self, samples: &mut Vec<f32>) -> f64 {
        if self._stream.is_none() {
            samples.clear();
            return self.sample_rate;
        }
        // Adapt the target to how bursty the device's consumption actually is.
        let underruns = self.stats.underruns.load(Ordering::Relaxed);
        if underruns != self.last_underruns {
            self.last_underruns = underruns;
            self.target_ms = (self.target_ms + TARGET_STEP_UP_MS).min(TARGET_MAX_MS);
            self.clean_pushes = 0;
        } else {
            self.clean_pushes += 1;
            if self.clean_pushes >= TARGET_RELAX_PUSHES {
                self.clean_pushes = 0;
                self.target_ms = (self.target_ms - TARGET_STEP_DOWN_MS).max(TARGET_MIN_MS);
            }
        }
        self.stats.target_us.store((self.target_ms * 1000.0) as u64, Ordering::Relaxed);
        let target = self.sample_rate * self.target_ms / 1000.0;
        let low_water = self.ring.len() as f64;
        if !self.started {
            // Pre-fill to the target once so playback starts at steady state.
            let pad = vec![0.0f32; target as usize];
            self.ring.push(&pad);
            self.started = true;
            self.smoothed_fill = target;
            self.pushes_since_start = 0;
        } else {
            self.smoothed_fill += (low_water - self.smoothed_fill) * DRC_SMOOTHING;
        }
        let pushed = self.ring.push(samples);
        if pushed < samples.len() {
            self.stats.overflows.fetch_add(1, Ordering::Relaxed);
        }
        samples.clear();

        let fill = self.ring.len() as f64;
        // PI controller: the integral term absorbs a constant clock/speed offset so the
        // low-water mark settles exactly on target.
        let err = ((target - self.smoothed_fill) / target).clamp(-1.0, 1.0);
        self.pushes_since_start = self.pushes_since_start.saturating_add(1);
        if self.pushes_since_start > DRC_SETTLE_PUSHES {
            self.integral = (self.integral + err * DRC_KI).clamp(-MAX_RATE_DELTA, MAX_RATE_DELTA);
        }
        let p_term = (err * DRC_KP).clamp(-MAX_P_DELTA, MAX_P_DELTA);
        let ratio = 1.0 + (p_term + self.integral).clamp(-MAX_RATE_DELTA, MAX_RATE_DELTA);
        self.stats.buffered_us.store((fill / self.sample_rate * 1e6) as u64, Ordering::Relaxed);
        self.stats.low_water_us.store((low_water / self.sample_rate * 1e6) as u64, Ordering::Relaxed);
        self.stats.drc_ppm.store(((ratio - 1.0) * 1e6) as i64 as u64, Ordering::Relaxed);
        self.sample_rate * ratio
    }

    /// Drops queued audio (e.g. after loading a state or a ROM); performed by the consumer.
    pub fn flush(&mut self) {
        let head = self.ring.head.load(Ordering::Relaxed);
        self.stats.flush_to.store(head, Ordering::Release);
        self.started = false;
    }
}
