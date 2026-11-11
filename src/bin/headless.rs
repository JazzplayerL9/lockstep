// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones

//! `lockstep-headless`: runs the Lockstep core without a window, for testing.
//!
//! ```text
//! lockstep-headless test  <rom>...                          run blargg-style test ROMs ($6000 protocol)
//! lockstep-headless shot  <rom> <frames> <out.png> [script] run N frames, save a screenshot, print speed
//! lockstep-headless batch <dir> <frames> <outdir> [threads] boot every ROM in parallel; PNGs + summary.tsv
//! ```
//!
//! Input scripts: `start` (default), `game`, `walk`, `hold`, `none`.
//! `LOCKSTEP_PROFILE=1` adds a sampling profile to `shot`.
//! See docs/TESTING.md for details.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use lockstep_core::cartridge::Cartridge;
use lockstep_core::cpu::System;
use lockstep_core::input::*;
use lockstep_core::png;

/// Controller state for `frame` under a named input script (`game`, `walk`, `hold`,
/// `none`, or the default `start`). See docs/TESTING.md for what each does.
fn scripted_input(script: &str, frame: u32) -> u8 {
    match script {
        "none" => 0,
        "walk" => {
            // Single Start press, then run right and jump periodically.
            if (120..124).contains(&frame) {
                BTN_START
            } else if frame > 300 {
                BTN_RIGHT | BTN_B | if frame % 50 < 18 { BTN_A } else { 0 }
            } else {
                0
            }
        }
        "game" => {
            // Tap Start a few times to get through title/menus, then play: walk right, jump.
            if frame < 900 {
                if frame % 150 < 4 && frame >= 150 { BTN_START } else if frame % 150 == 75 { BTN_A } else { 0 }
            } else {
                BTN_RIGHT | if frame % 40 < 10 { BTN_A } else { 0 } | if frame % 90 < 20 { BTN_B } else { 0 }
            }
        }
        "hold" => {
            if frame % 120 < 4 { BTN_START } else { BTN_RIGHT | if frame % 30 < 15 { BTN_B } else { 0 } }
        }
        _ => {
            // Tap Start every 2 s for the first ~20 s, then also tap A and walk right.
            let f = frame % 120;
            if f < 4 && frame < 1500 {
                BTN_START
            } else if (60..64).contains(&f) {
                BTN_A
            } else if frame > 900 && f > 70 {
                BTN_RIGHT
            } else {
                0
            }
        }
    }
}

// ---- Minimal SIGPROF sampling profiler (`LOCKSTEP_PROFILE=1`), symbolised with addr2line.
/// Minimal SIGPROF sampling profiler (`LOCKSTEP_PROFILE=1`). Samples the interrupted
/// instruction pointer every 200 µs of CPU time; `report` symbolises them with `addr2line`.
#[cfg(target_os = "linux")]
mod prof {
    use std::sync::atomic::{AtomicUsize, Ordering};
    /// Maximum number of samples kept.
    const N: usize = 1 << 16;
    /// Sampled instruction pointers. Written only by the signal handler, read after the timer stops.
    static mut SAMPLES: [usize; N] = [0; N];
    /// Number of samples taken (may exceed `N`; extra samples are dropped).
    static COUNT: AtomicUsize = AtomicUsize::new(0);

    /// SIGPROF handler: records RIP from the interrupted context. Async-signal-safe (no allocation, no locks).
    extern "C" fn handler(_: libc::c_int, _: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
        unsafe {
            let uc = ctx as *mut libc::ucontext_t;
            let ip = (*uc).uc_mcontext.gregs[libc::REG_RIP as usize] as usize;
            let i = COUNT.fetch_add(1, Ordering::Relaxed);
            if i < N {
                (*std::ptr::addr_of_mut!(SAMPLES))[i] = ip;
            }
        }
    }

    /// Installs the handler and starts a 200 µs `ITIMER_PROF` timer.
    pub fn start() {
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handler as *const () as usize;
            sa.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
            libc::sigaction(libc::SIGPROF, &sa, std::ptr::null_mut());
            let tv = libc::timeval { tv_sec: 0, tv_usec: 200 };
            let it = libc::itimerval { it_interval: tv, it_value: tv };
            libc::setitimer(libc::ITIMER_PROF, &it, std::ptr::null_mut());
        }
    }

    /// Stops the timer, converts samples to offsets in the executable (subtracting the load
    /// base from `/proc/self/maps`), symbolises them with `addr2line` and prints the top 40.
    pub fn report() {
        unsafe {
            let it: libc::itimerval = std::mem::zeroed();
            libc::setitimer(libc::ITIMER_PROF, &it, std::ptr::null_mut());
        }
        let n = COUNT.load(Ordering::Relaxed).min(N);
        let samples: Vec<usize> = unsafe { (&(*std::ptr::addr_of!(SAMPLES)))[..n].to_vec() };
        let exe = std::fs::read_link("/proc/self/exe").unwrap();
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        let base = maps
            .lines()
            .find(|l| l.ends_with(exe.to_str().unwrap()))
            .and_then(|l| usize::from_str_radix(l.split('-').next().unwrap(), 16).ok())
            .unwrap_or(0);
        let addrs: Vec<String> = samples.iter().map(|a| format!("{:x}", a.wrapping_sub(base))).collect();
        let out = std::process::Command::new("addr2line")
            .arg("-f").arg("-C").arg("-e").arg(&exe)
            .args(&addrs)
            .output()
            .expect("addr2line");
        let text = String::from_utf8_lossy(&out.stdout);
        let mut counts = std::collections::HashMap::<String, usize>::new();
        for pair in text.lines().collect::<Vec<_>>().chunks(2) {
            let func = pair[0].split("::{{closure}}").next().unwrap().to_string();
            let line = pair.get(1).map(|l| l.rsplit('/').next().unwrap_or(l)).unwrap_or("");
            *counts.entry(format!("{func}  [{line}]")).or_default() += 1;
        }
        let mut v: Vec<_> = counts.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        eprintln!("--- profile: {n} samples ---");
        for (k, c) in v.iter().take(40) {
            eprintln!("{:5.1}%  {}", *c as f64 * 100.0 / n as f64, k);
        }
    }
}

/// Stub profiler for non-Linux hosts where `SIGPROF` / `/proc/self/maps` are not available.
#[cfg(not(target_os = "linux"))]
mod prof {
    /// Prints a notice that `SIGPROF` profiling is Linux-only.
    pub fn start() {
        eprintln!("[lockstep-headless] LOCKSTEP_PROFILE is only supported on Linux");
    }
    /// No-op on non-Linux hosts.
    pub fn report() {}
}

/// Reads CPU memory for result inspection (RAM directly, everything else through the mapper).
fn peek(sys: &mut System, addr: u16) -> u8 {
    if addr < 0x2000 { sys.ram[(addr & 0x7FF) as usize] } else { sys.mapper.cpu_read(addr).unwrap_or(0) }
}

/// Writes the last completed frame as a PNG.
fn save_png(sys: &System, path: &Path) {
    let data = png::encode_rgba(256, 240, &sys.ppu.framebuffer);
    let _ = std::fs::write(path, data);
}

/// Runs a test ROM using blargg's `$6000` protocol: signature `DE B0 61` at `$6001`, status
/// `$80` = running, `$81` = reset requested, anything else = finished with that result code
/// (0 = pass) and the text output at `$6004`. Returns (passed, message).
fn run_test_rom(path: &str) -> (bool, String) {
    let cart = match Cartridge::from_file(path) {
        Ok(c) => c,
        Err(e) => return (false, format!("load error: {e}")),
    };
    let mut sys = match System::new(cart) {
        Ok(s) => s,
        Err(e) => return (false, e),
    };
    let mut started = false;
    let mut reset_at: Option<u32> = None;
    for frame in 0..60 * 60 {
        sys.run_frame();
        if let Some(r) = reset_at
            && frame >= r
        {
            sys.reset();
            reset_at = None;
        }
        let sig = (peek(&mut sys, 0x6001), peek(&mut sys, 0x6002), peek(&mut sys, 0x6003));
        if sig != (0xDE, 0xB0, 0x61) {
            continue;
        }
        let status = peek(&mut sys, 0x6000);
        match status {
            0x80 => started = true,
            0x81 => {
                if reset_at.is_none() {
                    reset_at = Some(frame + 10);
                }
            }
            _ if started || frame > 2 => {
                let mut text = String::new();
                let mut a = 0x6004u16;
                while a < 0x7F00 {
                    let c = peek(&mut sys, a);
                    if c == 0 {
                        break;
                    }
                    text.push(c as char);
                    a += 1;
                }
                let text = text.trim().replace('\n', " | ");
                return (status == 0, format!("status={status:#04X} {text}"));
            }
            _ => {}
        }
    }
    let shot = std::env::temp_dir().join(format!(
        "nestest_{}.png",
        Path::new(path).file_stem().unwrap().to_string_lossy()
    ));
    save_png(&sys, &shot);
    (false, format!("no $6000 result (screenshot {})", shot.display()))
}

/// One row of `summary.tsv`.
struct BatchResult {
    /// ROM file name without extension.
    name: String,
    /// `ok`, `BLANK`, `JAM@<frame> pc=<addr>`, `ERR <message>` or `PANIC`.
    status: String,
    /// Board name.
    mapper: String,
    /// Distinct colours in the final frame (1 = blank screen).
    colors: usize,
    /// Wall-clock time spent on this ROM.
    ms: u128,
}

/// Boots one ROM for `frames` frames with the `game` script, saves its screenshot and classifies the outcome.
fn batch_one(path: &Path, frames: u32, outdir: &Path) -> BatchResult {
    let name = path.file_stem().unwrap().to_string_lossy().to_string();
    let t0 = Instant::now();
    let res = std::panic::catch_unwind(|| -> Result<(String, String, usize), String> {
        let cart = Cartridge::from_file(path)?;
        let mut sys = System::new(cart)?;
        let mapper = sys.mapper_name().to_string();
        let mut jammed_at = None;
        for f in 0..frames {
            sys.joypad.buttons_p1 = scripted_input("game", f);
            sys.run_frame();
            if sys.jammed && jammed_at.is_none() {
                jammed_at = Some(f);
            }
        }
        save_png(&sys, &outdir.join(format!("{name}.png")));
        let mut colors: Vec<[u8; 3]> = sys.ppu.framebuffer.chunks_exact(4).map(|p| [p[0], p[1], p[2]]).collect();
        colors.sort();
        colors.dedup();
        let status = match jammed_at {
            Some(f) => format!("JAM@{f} pc={:04X}", sys.pc),
            None if colors.len() <= 1 => "BLANK".to_string(),
            None => "ok".to_string(),
        };
        Ok((status, mapper, colors.len()))
    });
    let ms = t0.elapsed().as_millis();
    match res {
        Ok(Ok((status, mapper, colors))) => BatchResult { name, status, mapper, colors, ms },
        Ok(Err(e)) => BatchResult { name, status: format!("ERR {e}"), mapper: String::new(), colors: 0, ms },
        Err(_) => BatchResult { name, status: "PANIC".into(), mapper: String::new(), colors: 0, ms },
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("test") => {
            let mut pass = 0;
            let roms = &args[2..];
            for r in roms {
                let (ok, msg) = run_test_rom(r);
                if ok {
                    pass += 1;
                }
                println!("{} {:50} {}", if ok { "PASS" } else { "FAIL" }, Path::new(r).file_name().unwrap().to_string_lossy(), msg);
            }
            println!("{pass}/{} passed", roms.len());
        }
        Some("shot") => {
            let rom = &args[2];
            let frames: u32 = args[3].parse().unwrap();
            let out = &args[4];
            let script = args.get(5).map(String::as_str).unwrap_or("start");
            let mut sys = System::new(Cartridge::from_file(rom).unwrap()).unwrap();
            let profile = std::env::var_os("LOCKSTEP_PROFILE").is_some();
            if profile {
                prof::start();
            }
            let t0 = Instant::now();
            for f in 0..frames {
                sys.joypad.buttons_p1 = scripted_input(script, f);
                sys.run_frame();
            }
            let el = t0.elapsed();
            if profile {
                prof::report();
            }
            save_png(&sys, Path::new(out));
            println!(
                "{} frames in {:.2?} ({:.0} fps), mapper {}, jammed={}",
                frames,
                el,
                frames as f64 / el.as_secs_f64(),
                sys.mapper_name(),
                sys.jammed
            );
        }
        Some("batch") => {
            let dir = PathBuf::from(&args[2]);
            let frames: u32 = args[3].parse().unwrap();
            let outdir = PathBuf::from(&args[4]);
            let threads: usize = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(8);
            std::fs::create_dir_all(&outdir).unwrap();
            let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "zip" || x == "nes"))
                .collect();
            files.sort();
            let files = Arc::new(files);
            let next = Arc::new(AtomicUsize::new(0));
            let results = Arc::new(Mutex::new(Vec::new()));
            let mut hs = Vec::new();
            for _ in 0..threads {
                let (files, next, results, outdir) = (files.clone(), next.clone(), results.clone(), outdir.clone());
                hs.push(
                    std::thread::Builder::new()
                        .stack_size(16 << 20)
                        .spawn(move || {
                            loop {
                                let i = next.fetch_add(1, Ordering::SeqCst);
                                if i >= files.len() {
                                    break;
                                }
                                let r = batch_one(&files[i], frames, &outdir);
                                results.lock().unwrap().push(r);
                            }
                        })
                        .unwrap(),
                );
            }
            for h in hs {
                h.join().unwrap();
            }
            let mut results = std::mem::take(&mut *results.lock().unwrap());
            results.sort_by(|a, b| a.name.cmp(&b.name));
            let mut tsv = String::from("name\tstatus\tmapper\tcolors\tms\n");
            let mut bad = 0;
            for r in &results {
                if r.status != "ok" {
                    bad += 1;
                    println!("{:8} {} [{}]", r.status, r.name, r.mapper);
                }
                tsv.push_str(&format!("{}\t{}\t{}\t{}\t{}\n", r.name, r.status, r.mapper, r.colors, r.ms));
            }
            std::fs::write(outdir.join("summary.tsv"), tsv).unwrap();
            println!("{} ROMs, {} flagged", results.len(), bad);
        }
        _ => eprintln!("usage: lockstep-headless test <rom>... | shot <rom> <frames> <out.png> [script] | batch <dir> <frames> <outdir> [threads]"),
    }
}
