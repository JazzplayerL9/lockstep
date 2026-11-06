//! Dynamic PREEMPT_RT / SCHED_FIFO vs. Standard SCHED_OTHER scheduler engine,
//! CLOCK_MONOTONIC timer, wait-free atomic triple-buffering, and live ROM load/unload controller.

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{self, JoinHandle};

use crate::apu::AudioStream;
use crate::cartridge::Cartridge;
use crate::cpu::System;
use crate::ppu::{SCREEN_HEIGHT, SCREEN_WIDTH};

/// Exact NTSC NES frame duration in nanoseconds (~60.098814 Hz).
pub const NTSC_FRAME_NS: u64 = 16_639_263;
pub const FRAMEBUFFER_BYTES: usize = SCREEN_WIDTH * SCREEN_HEIGHT * 4;

const READY_BIT: u8 = 0x80;
const INDEX_MASK: u8 = 0x03;

/// Commands sent from the UI thread to the emulation worker thread.
pub enum EmulatorCommand {
    LoadRom(Cartridge),
    UnloadRom,
    Reset,
    SaveState,
    LoadState,
    SetOverclock(bool),
    SetRtMode(bool),
}

#[derive(Debug, Clone)]
pub struct RtCapabilities {
    pub kernel_release: String,
    pub kernel_version: String,
    pub preempt_rt_kernel: bool,
    pub max_rtprio: i32,
}

/// Probes the host OS kernel (`uname` + `RLIMIT_RTPRIO`) to check if `PREEMPT_RT`
/// and `SCHED_FIFO` real-time scheduling are available.
pub fn probe_rt_capabilities() -> RtCapabilities {
    let mut caps = RtCapabilities {
        kernel_release: String::new(),
        kernel_version: String::new(),
        preempt_rt_kernel: false,
        max_rtprio: 0,
    };

    #[cfg(target_os = "linux")]
    unsafe {
        let mut uts: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut uts) == 0 {
            let release = std::ffi::CStr::from_ptr(uts.release.as_ptr())
                .to_string_lossy()
                .into_owned();
            let version = std::ffi::CStr::from_ptr(uts.version.as_ptr())
                .to_string_lossy()
                .into_owned();
            caps.preempt_rt_kernel =
                version.contains("PREEMPT_RT") || release.contains("-rt") || release.contains("rt");
            caps.kernel_release = release;
            caps.kernel_version = version;
        }

        let mut rlim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_RTPRIO, &mut rlim) == 0 {
            caps.max_rtprio = (rlim.rlim_cur as i32).clamp(0, 99);
        }
    }

    caps
}

/// Applies or relaxes `SCHED_FIFO` real-time scheduling, `mlockall`, timer slack, and CPU core pinning.
pub fn apply_thread_scheduling(enable_rt: bool, max_rtprio: i32) -> Option<i32> {
    #[cfg(target_os = "linux")]
    unsafe {
        if enable_rt {
            let _ = libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE);
            let _ = libc::prctl(libc::PR_SET_TIMERSLACK, 1, 0, 0, 0);

            let num_cpus = libc::sysconf(libc::_SC_NPROCESSORS_ONLN);
            if num_cpus >= 2 {
                let target_cpu = (num_cpus as usize) - 1;
                let mut cpuset: libc::cpu_set_t = std::mem::zeroed();
                libc::CPU_ZERO(&mut cpuset);
                libc::CPU_SET(target_cpu, &mut cpuset);
                let _ =
                    libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &cpuset);
            }

            let top_prio = if max_rtprio > 0 { max_rtprio } else { 80 };
            for prio in [top_prio, 50, 20, 1] {
                let param = libc::sched_param {
                    sched_priority: prio,
                };
                if libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) == 0
                {
                    return Some(prio);
                }
            }
            let _ = libc::setpriority(libc::PRIO_PROCESS, 0, -20);
            None
        } else {
            // Standard SCHED_OTHER mode: restore default scheduling policy and 50 µs timer slack
            let param = libc::sched_param { sched_priority: 0 };
            let _ = libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_OTHER, &param);
            let _ = libc::prctl(libc::PR_SET_TIMERSLACK, 50_000, 0, 0, 0);

            let num_cpus = libc::sysconf(libc::_SC_NPROCESSORS_ONLN);
            if num_cpus >= 1 {
                let mut cpuset: libc::cpu_set_t = std::mem::zeroed();
                libc::CPU_ZERO(&mut cpuset);
                for cpu in 0..(num_cpus as usize) {
                    libc::CPU_SET(cpu, &mut cpuset);
                }
                let _ =
                    libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &cpuset);
            }
            None
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (enable_rt, max_rtprio);
        None
    }
}

pub struct MonotonicTimer;

impl MonotonicTimer {
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

/// Wait-free atomic triple buffer for 256x240 RGBA frames.
pub struct TripleFramebuffer {
    buffers: [UnsafeCell<Box<[u8; FRAMEBUFFER_BYTES]>>; 3],
    shared_state: AtomicU8,
    write_idx: UnsafeCell<u8>,
    read_idx: UnsafeCell<u8>,
}

unsafe impl Sync for TripleFramebuffer {}
unsafe impl Send for TripleFramebuffer {}

impl Default for TripleFramebuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl TripleFramebuffer {
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
        }
    }

    #[inline]
    pub fn publish_frame(&self, src: &[u8; FRAMEBUFFER_BYTES]) {
        unsafe {
            let w = *self.write_idx.get();
            let dst = &mut *self.buffers[w as usize].get();
            dst.copy_from_slice(src);
            let prev_shared = self.shared_state.swap(w | READY_BIT, Ordering::AcqRel);
            *self.write_idx.get() = prev_shared & INDEX_MASK;
        }
    }

    #[inline]
    pub fn acquire_latest(&self) -> Option<&[u8; FRAMEBUFFER_BYTES]> {
        let current = self.shared_state.load(Ordering::Acquire);
        if (current & READY_BIT) == 0 {
            return None;
        }
        unsafe {
            let r = *self.read_idx.get();
            let prev_shared = self.shared_state.swap(r, Ordering::AcqRel);
            let new_r = prev_shared & INDEX_MASK;
            *self.read_idx.get() = new_r;
            Some(&*self.buffers[new_r as usize].get())
        }
    }
}

pub struct RtSharedState {
    pub caps: RtCapabilities,
    pub framebuffer: TripleFramebuffer,
    pub buttons_held: AtomicU8,
    pub buttons_latched: AtomicU8,
    pub running: AtomicBool,
    pub rom_loaded: AtomicBool,
    pub rt_mode_enabled: AtomicBool,
    pub sched_fifo_prio: AtomicI32,
    pub overclock_enabled: AtomicBool,
    pub frames_executed: AtomicU64,
    pub last_jitter_ns: AtomicI64,
    pub max_abs_jitter_ns: AtomicU64,
}

impl RtSharedState {
    pub fn new() -> Self {
        let caps = probe_rt_capabilities();
        let default_rt = caps.preempt_rt_kernel || caps.max_rtprio > 0;
        Self {
            caps,
            framebuffer: TripleFramebuffer::new(),
            buttons_held: AtomicU8::new(0),
            buttons_latched: AtomicU8::new(0),
            running: AtomicBool::new(true),
            rom_loaded: AtomicBool::new(false),
            rt_mode_enabled: AtomicBool::new(default_rt),
            sched_fifo_prio: AtomicI32::new(0),
            overclock_enabled: AtomicBool::new(true),
            frames_executed: AtomicU64::new(0),
            last_jitter_ns: AtomicI64::new(0),
            max_abs_jitter_ns: AtomicU64::new(0),
        }
    }

    #[inline]
    pub fn update_input(&self, held_mask: u8, newly_pressed_mask: u8) {
        self.buttons_held.store(held_mask, Ordering::Release);
        if newly_pressed_mask != 0 {
            self.buttons_latched
                .fetch_or(newly_pressed_mask, Ordering::AcqRel);
        }
    }
}

impl Default for RtSharedState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn spawn_emulator_worker(
    shared: Arc<RtSharedState>,
) -> (Sender<EmulatorCommand>, JoinHandle<()>) {
    let (cmd_tx, cmd_rx): (Sender<EmulatorCommand>, Receiver<EmulatorCommand>) = channel();

    let handle = thread::Builder::new()
        .name("nes-rt-worker".to_string())
        .spawn(move || {
            // Spawn audio helper thread BEFORE elevating this worker thread to SCHED_FIFO
            let audio_stream = AudioStream::new();

            let mut rt_enabled = shared.rt_mode_enabled.load(Ordering::Acquire);
            let prio = apply_thread_scheduling(rt_enabled, shared.caps.max_rtprio);
            shared
                .sched_fifo_prio
                .store(prio.unwrap_or(0), Ordering::Release);

            eprintln!(
                "[nes-rt-emu] kernel={} (PREEMPT_RT={}) | mode={} | SCHED_FIFO={:?}",
                shared.caps.kernel_release,
                shared.caps.preempt_rt_kernel,
                if rt_enabled { "RT" } else { "Standard" },
                prio
            );

            let mut active_system: Option<System> = None;
            let mut saved_snapshot: Option<System> = None;
            let mut overclock = shared.overclock_enabled.load(Ordering::Acquire);

            let mut next_deadline_ns = MonotonicTimer::now_ns() + NTSC_FRAME_NS;

            while shared.running.load(Ordering::Acquire) {
                while let Ok(cmd) = cmd_rx.try_recv() {
                    match cmd {
                        EmulatorCommand::LoadRom(cart) => {
                            if let Some(prev) = &active_system {
                                prev.save_sram();
                            }
                            let mut sys = System::from_cartridge(cart);
                            sys.overclock_enabled = overclock;
                            saved_snapshot = None;
                            active_system = Some(sys);
                            shared.rom_loaded.store(true, Ordering::Release);
                            shared.max_abs_jitter_ns.store(0, Ordering::Release);
                            next_deadline_ns = MonotonicTimer::now_ns() + NTSC_FRAME_NS;
                        }
                        EmulatorCommand::UnloadRom => {
                            if let Some(prev) = &active_system {
                                prev.save_sram();
                            }
                            active_system = None;
                            saved_snapshot = None;
                            shared.rom_loaded.store(false, Ordering::Release);
                            let blank = [0u8; FRAMEBUFFER_BYTES];
                            shared.framebuffer.publish_frame(&blank);
                        }
                        EmulatorCommand::Reset => {
                            if let Some(sys) = &mut active_system {
                                sys.reset();
                            }
                        }
                        EmulatorCommand::SaveState => {
                            if let Some(sys) = &active_system {
                                saved_snapshot = Some(sys.clone());
                            }
                        }
                        EmulatorCommand::LoadState => {
                            if let Some(snap) = &saved_snapshot {
                                let mut restored = snap.clone();
                                restored.overclock_enabled = overclock;
                                active_system = Some(restored);
                            }
                        }
                        EmulatorCommand::SetOverclock(enabled) => {
                            overclock = enabled;
                            shared.overclock_enabled.store(enabled, Ordering::Release);
                            if let Some(sys) = &mut active_system {
                                sys.overclock_enabled = enabled;
                            }
                        }
                        EmulatorCommand::SetRtMode(enable) => {
                            rt_enabled = enable;
                            shared.rt_mode_enabled.store(enable, Ordering::Release);
                            let new_prio =
                                apply_thread_scheduling(rt_enabled, shared.caps.max_rtprio);
                            shared
                                .sched_fifo_prio
                                .store(new_prio.unwrap_or(0), Ordering::Release);
                            shared.max_abs_jitter_ns.store(0, Ordering::Release);
                            next_deadline_ns = MonotonicTimer::now_ns() + NTSC_FRAME_NS;
                        }
                    }
                }

                if let Some(sys) = &mut active_system {
                    let held = shared.buttons_held.load(Ordering::Acquire);
                    let latched = shared.buttons_latched.swap(0, Ordering::AcqRel);
                    sys.joypad.buttons_p1 = held | latched;

                    sys.run_frame();

                    let fb_array: &[u8; FRAMEBUFFER_BYTES] =
                        (&sys.ppu.framebuffer[..]).try_into().unwrap();
                    shared.framebuffer.publish_frame(fb_array);
                    audio_stream.push_samples(&mut sys.apu.audio_samples);
                    shared.frames_executed.fetch_add(1, Ordering::Relaxed);
                } else {
                    let _ = shared.buttons_latched.swap(0, Ordering::AcqRel);
                }

                let jitter_ns = MonotonicTimer::sleep_until_ns(next_deadline_ns, rt_enabled);
                shared.last_jitter_ns.store(jitter_ns, Ordering::Relaxed);
                let abs_jitter = jitter_ns.unsigned_abs();
                shared
                    .max_abs_jitter_ns
                    .fetch_max(abs_jitter, Ordering::Relaxed);

                next_deadline_ns += NTSC_FRAME_NS;
                let now = MonotonicTimer::now_ns();
                if now > next_deadline_ns + NTSC_FRAME_NS * 5 {
                    next_deadline_ns = now + NTSC_FRAME_NS;
                }
            }

            if let Some(prev) = &active_system {
                prev.save_sram();
            }
        })
        .expect("Failed to spawn nes-rt-worker thread");

    (cmd_tx, handle)
}
