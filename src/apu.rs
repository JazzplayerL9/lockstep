//! Ricoh 2A03 APU, clocked once per CPU cycle: frame counter with exact $4017 write
//! delays, length counters with the reload/halt timing quirks, envelopes, sweeps, and a
//! DMC whose sample fetches are performed as real DMA by the CPU core.

pub const SAMPLE_RATE: u32 = 44100;
const CPU_CLOCK_HZ: f64 = 1_789_773.0;

static LENGTH_TABLE: [u8; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96, 22, 192, 24, 72, 26, 16, 28, 32, 30,
];
static DUTY_TABLE: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1],
    [0, 0, 0, 0, 0, 0, 1, 1],
    [0, 0, 0, 0, 1, 1, 1, 1],
    [1, 1, 1, 1, 1, 1, 0, 0],
];
static TRIANGLE_TABLE: [u8; 32] = [
    15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];
static NOISE_PERIODS: [u16; 16] = [4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068];
static DMC_PERIODS: [u16; 16] = [428, 380, 340, 320, 286, 254, 226, 214, 190, 160, 142, 128, 106, 84, 72, 54];
static STEP_CYCLES: [[u32; 6]; 2] = [[7457, 14913, 22371, 29828, 29829, 29830], [7457, 14913, 22371, 29829, 37281, 37282]];
// 0 = none, 1 = quarter, 2 = half
static STEP_TYPE: [[u8; 6]; 2] = [[1, 2, 1, 0, 2, 0], [1, 2, 1, 0, 2, 0]];

#[derive(Clone, Default)]
struct LengthCounter {
    enabled: bool,
    halt: bool,
    new_halt: bool,
    counter: u8,
    reload_value: u8,
    previous_value: u8,
}

impl LengthCounter {
    fn init(&mut self, halt: bool) {
        self.new_halt = halt;
    }
    fn load(&mut self, idx: u8) {
        if self.enabled {
            self.reload_value = LENGTH_TABLE[idx as usize];
            self.previous_value = self.counter;
        }
    }
    fn reload(&mut self) {
        if self.reload_value != 0 {
            if self.counter == self.previous_value {
                self.counter = self.reload_value;
            }
            self.reload_value = 0;
        }
        self.halt = self.new_halt;
    }
    fn tick(&mut self) {
        if self.counter > 0 && !self.halt {
            self.counter -= 1;
        }
    }
    fn set_enabled(&mut self, e: bool) {
        if !e {
            self.counter = 0;
        }
        self.enabled = e;
    }
}

#[derive(Clone, Default)]
struct Envelope {
    constant: bool,
    volume: u8,
    start: bool,
    divider: i8,
    counter: u8,
    lc: LengthCounter,
}

impl Envelope {
    fn init(&mut self, v: u8) {
        self.lc.init(v & 0x20 != 0);
        self.constant = v & 0x10 != 0;
        self.volume = v & 0x0F;
    }
    fn volume(&self) -> u8 {
        if self.lc.counter > 0 {
            if self.constant { self.volume } else { self.counter }
        } else {
            0
        }
    }
    fn tick(&mut self) {
        if !self.start {
            self.divider -= 1;
            if self.divider < 0 {
                self.divider = self.volume as i8;
                if self.counter > 0 {
                    self.counter -= 1;
                } else if self.lc.halt {
                    self.counter = 15;
                }
            }
        } else {
            self.start = false;
            self.counter = 15;
            self.divider = self.volume as i8;
        }
    }
}

#[derive(Clone, Default)]
struct Square {
    env: Envelope,
    channel1: bool,
    timer: u16,
    period: u16,
    real_period: u16,
    duty: u8,
    duty_pos: u8,
    sweep_enabled: bool,
    sweep_period: u8,
    sweep_negate: bool,
    sweep_shift: u8,
    reload_sweep: bool,
    sweep_divider: u8,
    sweep_target: u32,
}

impl Square {
    fn update_target(&mut self) {
        let shift = (self.real_period >> self.sweep_shift) as u32;
        let rp = self.real_period as u32;
        self.sweep_target = if self.sweep_negate {
            rp.wrapping_sub(shift).wrapping_sub(self.channel1 as u32)
        } else {
            rp + shift
        };
    }
    fn set_period(&mut self, p: u16) {
        self.real_period = p;
        self.period = p * 2 + 1;
        self.update_target();
    }
    fn muted(&self) -> bool {
        self.real_period < 8 || (!self.sweep_negate && self.sweep_target > 0x7FF)
    }
    fn write(&mut self, reg: u16, v: u8) {
        match reg & 3 {
            0 => {
                self.env.init(v);
                self.duty = v >> 6;
            }
            1 => {
                self.sweep_enabled = v & 0x80 != 0;
                self.sweep_negate = v & 0x08 != 0;
                self.sweep_period = ((v & 0x70) >> 4) + 1;
                self.sweep_shift = v & 7;
                self.update_target();
                self.reload_sweep = true;
            }
            2 => self.set_period((self.real_period & 0x0700) | v as u16),
            _ => {
                self.env.lc.load(v >> 3);
                self.set_period((self.real_period & 0xFF) | (((v & 7) as u16) << 8));
                self.duty_pos = 0;
                self.env.start = true;
            }
        }
    }
    fn tick_sweep(&mut self) {
        self.sweep_divider = self.sweep_divider.wrapping_sub(1);
        if self.sweep_divider == 0 {
            if self.sweep_shift > 0 && self.sweep_enabled && self.real_period >= 8 && self.sweep_target <= 0x7FF {
                self.set_period(self.sweep_target as u16);
            }
            self.sweep_divider = self.sweep_period;
        }
        if self.reload_sweep {
            self.sweep_divider = self.sweep_period;
            self.reload_sweep = false;
        }
    }
    #[inline]
    fn clock(&mut self) {
        if self.timer == 0 {
            self.timer = self.period;
            self.duty_pos = self.duty_pos.wrapping_sub(1) & 7;
        } else {
            self.timer -= 1;
        }
    }
    #[inline]
    fn output(&self) -> u8 {
        if self.muted() { 0 } else { DUTY_TABLE[self.duty as usize][self.duty_pos as usize] * self.env.volume() }
    }
}

#[derive(Clone, Default)]
struct Triangle {
    lc: LengthCounter,
    timer: u16,
    period: u16,
    pos: u8,
    linear_counter: u8,
    linear_reload_value: u8,
    linear_control: bool,
    linear_reload: bool,
    out: u8,
}

impl Triangle {
    fn write(&mut self, reg: u16, v: u8) {
        match reg & 3 {
            0 => {
                self.linear_control = v & 0x80 != 0;
                self.linear_reload_value = v & 0x7F;
                self.lc.init(self.linear_control);
            }
            2 => self.period = (self.period & 0x0700) | v as u16,
            3 => {
                self.lc.load(v >> 3);
                self.period = (self.period & 0xFF) | (((v & 7) as u16) << 8);
                self.linear_reload = true;
            }
            _ => {}
        }
    }
    fn tick_linear(&mut self) {
        if self.linear_reload {
            self.linear_counter = self.linear_reload_value;
        } else if self.linear_counter > 0 {
            self.linear_counter -= 1;
        }
        if !self.linear_control {
            self.linear_reload = false;
        }
    }
    #[inline]
    fn clock(&mut self) {
        if self.timer == 0 {
            self.timer = self.period;
            if self.lc.counter > 0 && self.linear_counter > 0 {
                self.pos = (self.pos + 1) & 31;
                // Ultrasonic periods are filtered out (avoids popping), as most emulators do.
                if self.period >= 2 {
                    self.out = TRIANGLE_TABLE[self.pos as usize];
                }
            }
        } else {
            self.timer -= 1;
        }
    }
}

#[derive(Clone)]
struct Noise {
    env: Envelope,
    timer: u16,
    period: u16,
    shift: u16,
    mode: bool,
}

impl Noise {
    fn write(&mut self, reg: u16, v: u8) {
        match reg & 3 {
            0 => self.env.init(v),
            2 => {
                self.period = NOISE_PERIODS[(v & 0x0F) as usize] - 1;
                self.mode = v & 0x80 != 0;
            }
            3 => {
                self.env.lc.load(v >> 3);
                self.env.start = true;
            }
            _ => {}
        }
    }
    #[inline]
    fn clock(&mut self) {
        if self.timer == 0 {
            self.timer = self.period;
            let tap = if self.mode { 6 } else { 1 };
            let fb = (self.shift & 1) ^ ((self.shift >> tap) & 1);
            self.shift = (self.shift >> 1) | (fb << 14);
        } else {
            self.timer -= 1;
        }
    }
    #[inline]
    fn output(&self) -> u8 {
        if self.shift & 1 != 0 { 0 } else { self.env.volume() }
    }
}

#[derive(Clone)]
pub struct Dmc {
    timer: u16,
    period: u16,
    irq_enabled: bool,
    loop_flag: bool,
    output_level: u8,
    sample_addr: u16,
    sample_length: u16,
    current_addr: u16,
    bytes_remaining: u16,
    read_buffer: u8,
    buffer_empty: bool,
    shift: u8,
    bits_remaining: u8,
    silence: bool,
    transfer_start_delay: u8,
    disable_delay: u8,
    pub irq_flag: bool,
}

impl Dmc {
    fn new() -> Self {
        Self {
            timer: DMC_PERIODS[0] - 1,
            period: DMC_PERIODS[0] - 1,
            irq_enabled: false,
            loop_flag: false,
            output_level: 0,
            sample_addr: 0xC000,
            sample_length: 1,
            current_addr: 0,
            bytes_remaining: 0,
            read_buffer: 0,
            buffer_empty: true,
            shift: 0,
            bits_remaining: 8,
            silence: true,
            transfer_start_delay: 0,
            disable_delay: 0,
            irq_flag: false,
        }
    }
    fn init_sample(&mut self) {
        self.current_addr = self.sample_addr;
        self.bytes_remaining = self.sample_length;
    }
}

/// DMA requests raised by the APU for the CPU to service.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum DmcDmaRequest {
    #[default]
    None,
    Start,
    Stop,
}

#[derive(Clone)]
pub struct Apu {
    sq1: Square,
    sq2: Square,
    tri: Triangle,
    noise: Noise,
    pub dmc: Dmc,
    // Frame counter.
    fc_previous: u32,
    fc_step: usize,
    fc_mode: usize,
    fc_inhibit_irq: bool,
    fc_block_tick: u8,
    fc_new_value: i16,
    fc_write_delay: i8,
    pub frame_irq_flag: bool,
    pub frame_irq_line: bool,
    /// Pending DMC DMA start/stop for the CPU.
    pub dma_request: DmcDmaRequest,
    // Output.
    sample_accum: f64,
    sample_sum: f32,
    sample_count: u32,
    hp1_prev_in: f32,
    hp1_prev_out: f32,
    hp2_prev_in: f32,
    hp2_prev_out: f32,
    lp_prev: f32,
    pub audio_samples: Vec<i16>,
    /// Expansion audio contribution for the current cycle (set by the system).
    pub expansion: f32,
}

impl Default for Apu {
    fn default() -> Self {
        Self::new()
    }
}

impl Apu {
    pub fn new() -> Self {
        let sq1 = Square { channel1: true, ..Default::default() };
        let mut a = Self {
            sq1,
            sq2: Square::default(),
            tri: Triangle::default(),
            noise: Noise { env: Envelope::default(), timer: 0, period: NOISE_PERIODS[0] - 1, shift: 1, mode: false },
            dmc: Dmc::new(),
            fc_previous: 0,
            fc_step: 0,
            fc_mode: 0,
            fc_inhibit_irq: false,
            fc_block_tick: 0,
            fc_new_value: 0,
            fc_write_delay: 3,
            frame_irq_flag: false,
            frame_irq_line: false,
            dma_request: DmcDmaRequest::None,
            sample_accum: 0.0,
            sample_sum: 0.0,
            sample_count: 0,
            hp1_prev_in: 0.0,
            hp1_prev_out: 0.0,
            hp2_prev_in: 0.0,
            hp2_prev_out: 0.0,
            lp_prev: 0.0,
            audio_samples: Vec::with_capacity(2048),
            expansion: 0.0,
        };
        a.sq1.update_target();
        a
    }

    /// Soft reset: silences channels; the frame counter mode is kept (acts like a $4017 rewrite).
    pub fn reset(&mut self) {
        let mode = self.fc_mode;
        let samples = std::mem::take(&mut self.audio_samples);
        *self = Self::new();
        self.audio_samples = samples;
        self.fc_new_value = if mode == 1 { 0x80 } else { 0 };
    }

    #[inline]
    pub fn irq_line(&self) -> bool {
        self.frame_irq_line || self.dmc.irq_flag
    }

    fn frame_tick(&mut self, kind: u8) {
        self.sq1.env.tick();
        self.sq2.env.tick();
        self.tri.tick_linear();
        self.noise.env.tick();
        if kind == 2 {
            self.sq1.env.lc.tick();
            self.sq2.env.lc.tick();
            self.tri.lc.tick();
            self.noise.env.lc.tick();
            self.sq1.tick_sweep();
            self.sq2.tick_sweep();
        }
    }

    fn frame_counter_clock(&mut self) {
        if self.fc_previous + 1 >= STEP_CYCLES[self.fc_mode][self.fc_step] {
            if self.fc_mode == 0 && self.fc_step >= 3 {
                self.frame_irq_flag = true;
                if !self.fc_inhibit_irq {
                    self.frame_irq_line = true;
                } else if self.fc_step == 5 {
                    self.frame_irq_flag = false;
                }
            }
            let kind = STEP_TYPE[self.fc_mode][self.fc_step];
            if kind != 0 && self.fc_block_tick == 0 {
                self.frame_tick(kind);
                self.fc_block_tick = 2;
            }
            self.fc_step += 1;
            if self.fc_step == 6 {
                self.fc_step = 0;
                self.fc_previous = 0;
            } else {
                self.fc_previous += 1;
            }
        } else {
            self.fc_previous += 1;
        }

        if self.fc_new_value >= 0 {
            self.fc_write_delay -= 1;
            if self.fc_write_delay == 0 {
                self.fc_mode = if self.fc_new_value & 0x80 != 0 { 1 } else { 0 };
                self.fc_write_delay = -1;
                self.fc_step = 0;
                self.fc_previous = 0;
                self.fc_new_value = -1;
                if self.fc_mode == 1 && self.fc_block_tick == 0 {
                    self.frame_tick(2);
                    self.fc_block_tick = 2;
                }
            }
        }
        if self.fc_block_tick > 0 {
            self.fc_block_tick -= 1;
        }
    }

    fn dmc_start_transfer(&mut self) {
        if self.dmc.buffer_empty && self.dmc.bytes_remaining > 0 {
            self.dma_request = DmcDmaRequest::Start;
        }
    }

    #[inline]
    pub fn dmc_read_address(&self) -> u16 {
        self.dmc.current_addr
    }

    /// Completes a DMC DMA fetch.
    pub fn set_dmc_read_buffer(&mut self, value: u8) {
        let d = &mut self.dmc;
        if d.bytes_remaining > 0 {
            d.read_buffer = value;
            d.buffer_empty = false;
            d.current_addr = d.current_addr.wrapping_add(1);
            if d.current_addr == 0 {
                d.current_addr = 0x8000;
            }
            d.bytes_remaining -= 1;
            if d.bytes_remaining == 0 {
                if d.loop_flag {
                    d.init_sample();
                } else if d.irq_enabled {
                    d.irq_flag = true;
                }
            }
        }
    }

    fn dmc_process_clock(&mut self) {
        if self.dmc.disable_delay > 0 {
            self.dmc.disable_delay -= 1;
            if self.dmc.disable_delay == 0 {
                self.dmc.bytes_remaining = 0;
                self.dma_request = DmcDmaRequest::Stop;
            }
        }
        if self.dmc.transfer_start_delay > 0 {
            self.dmc.transfer_start_delay -= 1;
            if self.dmc.transfer_start_delay == 0 {
                self.dmc_start_transfer();
            }
        }
    }

    fn dmc_clock(&mut self) {
        if self.dmc.timer != 0 {
            self.dmc.timer -= 1;
            return;
        }
        self.dmc.timer = self.dmc.period;
        let d = &mut self.dmc;
        if !d.silence {
            if d.shift & 1 != 0 {
                if d.output_level <= 125 {
                    d.output_level += 2;
                }
            } else if d.output_level >= 2 {
                d.output_level -= 2;
            }
            d.shift >>= 1;
        }
        d.bits_remaining -= 1;
        if d.bits_remaining == 0 {
            d.bits_remaining = 8;
            if d.buffer_empty {
                d.silence = true;
            } else {
                d.silence = false;
                d.shift = d.read_buffer;
                d.buffer_empty = true;
                if d.transfer_start_delay == 0 {
                    self.dmc_start_transfer();
                }
            }
        }
    }

    /// Advance one CPU cycle.
    #[inline]
    pub fn clock(&mut self) {
        self.dmc_process_clock();
        self.frame_counter_clock();
        self.sq1.env.lc.reload();
        self.sq2.env.lc.reload();
        self.noise.env.lc.reload();
        self.tri.lc.reload();
        self.sq1.clock();
        self.sq2.clock();
        self.noise.clock();
        self.tri.clock();
        self.dmc_clock();
        self.mix_cycle();
    }

    #[inline]
    fn mix_cycle(&mut self) {
        let p = (self.sq1.output() + self.sq2.output()) as f32;
        let pulse = if p > 0.0 { 95.88 / (8128.0 / p + 100.0) } else { 0.0 };
        let t = self.tri.out as f32;
        let n = self.noise.output() as f32;
        let d = self.dmc.output_level as f32;
        let tnd_in = t / 8227.0 + n / 12241.0 + d / 22638.0;
        let tnd = if tnd_in > 0.0 { 159.79 / (1.0 / tnd_in + 100.0) } else { 0.0 };
        self.sample_sum += pulse + tnd + self.expansion;
        self.sample_count += 1;

        self.sample_accum += SAMPLE_RATE as f64;
        if self.sample_accum >= CPU_CLOCK_HZ {
            self.sample_accum -= CPU_CLOCK_HZ;
            let raw = self.sample_sum / self.sample_count as f32;
            self.sample_sum = 0.0;
            self.sample_count = 0;
            // NES output chain: two high-pass filters (~90 Hz, ~440 Hz) and a gentle low-pass.
            let hp1 = 0.987 * (self.hp1_prev_out + raw - self.hp1_prev_in);
            self.hp1_prev_in = raw;
            self.hp1_prev_out = hp1;
            let hp2 = 0.996 * (self.hp2_prev_out + hp1 - self.hp2_prev_in);
            self.hp2_prev_in = hp1;
            self.hp2_prev_out = hp2;
            let lp = self.lp_prev + 0.815 * (hp2 - self.lp_prev);
            self.lp_prev = lp;
            let s = (lp * 40000.0).clamp(-32767.0, 32767.0) as i16;
            if self.audio_samples.len() < 8192 {
                self.audio_samples.push(s);
            }
        }
    }

    pub fn read_status(&mut self) -> u8 {
        let mut s = 0u8;
        if self.sq1.env.lc.counter > 0 {
            s |= 0x01;
        }
        if self.sq2.env.lc.counter > 0 {
            s |= 0x02;
        }
        if self.tri.lc.counter > 0 {
            s |= 0x04;
        }
        if self.noise.env.lc.counter > 0 {
            s |= 0x08;
        }
        if self.dmc.bytes_remaining > 0 {
            s |= 0x10;
        }
        if self.frame_irq_flag {
            s |= 0x40;
        }
        if self.dmc.irq_flag {
            s |= 0x80;
        }
        self.frame_irq_flag = false;
        self.frame_irq_line = false;
        s
    }

    pub fn write_register(&mut self, addr: u16, val: u8, cpu_cycle: u64) {
        match addr {
            0x4000..=0x4003 => self.sq1.write(addr, val),
            0x4004..=0x4007 => self.sq2.write(addr, val),
            0x4008..=0x400B => self.tri.write(addr, val),
            0x400C..=0x400F => self.noise.write(addr, val),
            0x4010 => {
                self.dmc.irq_enabled = val & 0x80 != 0;
                self.dmc.loop_flag = val & 0x40 != 0;
                self.dmc.period = DMC_PERIODS[(val & 0x0F) as usize] - 1;
                if !self.dmc.irq_enabled {
                    self.dmc.irq_flag = false;
                }
            }
            0x4011 => self.dmc.output_level = val & 0x7F,
            0x4012 => self.dmc.sample_addr = 0xC000 | ((val as u16) << 6),
            0x4013 => self.dmc.sample_length = ((val as u16) << 4) | 1,
            0x4015 => {
                self.dmc.irq_flag = false;
                self.sq1.env.lc.set_enabled(val & 0x01 != 0);
                self.sq2.env.lc.set_enabled(val & 0x02 != 0);
                self.tri.lc.set_enabled(val & 0x04 != 0);
                self.noise.env.lc.set_enabled(val & 0x08 != 0);
                if val & 0x10 == 0 {
                    if self.dmc.disable_delay == 0 {
                        self.dmc.disable_delay = if cpu_cycle & 1 == 0 { 2 } else { 3 };
                    }
                } else if self.dmc.bytes_remaining == 0 {
                    self.dmc.init_sample();
                    self.dmc.transfer_start_delay = if cpu_cycle & 1 == 0 { 2 } else { 3 };
                }
            }
            0x4017 => {
                self.fc_new_value = val as i16;
                self.fc_write_delay = if cpu_cycle & 1 == 1 { 4 } else { 3 };
                self.fc_inhibit_irq = val & 0x40 != 0;
                if self.fc_inhibit_irq {
                    self.frame_irq_line = false;
                    self.frame_irq_flag = false;
                }
            }
            _ => {}
        }
    }
}

pub struct AudioStream {
    sender: Option<std::sync::mpsc::SyncSender<Vec<u8>>>,
}

impl Default for AudioStream {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioStream {
    pub fn new() -> Self {
        use std::io::Write;
        use std::process::{Command, Stdio};
        use std::sync::mpsc::sync_channel;

        let child = Command::new("paplay")
            .args([
                "--raw",
                "--format=s16le",
                "--rate=44100",
                "--channels=1",
                "--latency-msec=40",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        if let Ok(mut proc) = child
            && let Some(mut stdin) = proc.stdin.take() {
                let (tx, rx) = sync_channel::<Vec<u8>>(4);
                std::thread::spawn(move || {
                    while let Ok(buf) = rx.recv() {
                        if stdin.write_all(&buf).is_err() {
                            break;
                        }
                    }
                    let _ = proc.kill();
                });
                return Self { sender: Some(tx) };
            }
        Self { sender: None }
    }

    pub fn push_samples(&self, samples: &mut Vec<i16>) {
        if let Some(tx) = &self.sender {
            let mut bytes = Vec::with_capacity(samples.len() * 2);
            for &s in samples.iter() {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            let _ = tx.try_send(bytes);
        }
        samples.clear();
    }
}
