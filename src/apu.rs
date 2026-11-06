//! Ricoh 2A03 Audio Processing Unit (APU) 5-channel synthesizer and real-time audio sink.

use crate::mapper::Mapper;

pub const SAMPLE_RATE: u32 = 44100;
const CPU_CLOCK_HZ: f64 = 1_789_773.0;

static LENGTH_TABLE: [u8; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96, 22,
    192, 24, 72, 26, 16, 28, 32, 30,
];

static DUTY_TABLE: [[u8; 8]; 4] = [
    [0, 1, 0, 0, 0, 0, 0, 0],
    [0, 1, 1, 0, 0, 0, 0, 0],
    [0, 1, 1, 1, 1, 0, 0, 0],
    [1, 0, 0, 1, 1, 1, 1, 1],
];

static TRIANGLE_TABLE: [u8; 32] = [
    15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12,
    13, 14, 15,
];

static NOISE_PERIOD_TABLE: [u16; 16] = [
    4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068,
];

static DMC_RATE_TABLE: [u16; 16] = [
    428, 380, 340, 320, 286, 254, 226, 214, 190, 160, 142, 128, 106, 84, 72, 54,
];

#[derive(Clone, Default)]
pub struct PulseChannel {
    pub enabled: bool,
    pub is_channel_1: bool,
    pub duty: u8,
    pub length_halt: bool,
    pub constant_volume: bool,
    pub volume: u8,
    pub env_start: bool,
    pub env_divider: u8,
    pub env_decay: u8,
    pub sweep_enabled: bool,
    pub sweep_period: u8,
    pub sweep_negate: bool,
    pub sweep_shift: u8,
    pub sweep_reload: bool,
    pub sweep_divider: u8,
    pub timer_period: u16,
    pub timer_val: u16,
    pub seq_pos: u8,
    pub length_counter: u8,
}

impl PulseChannel {
    fn step_timer(&mut self) {
        if self.timer_val == 0 {
            self.timer_val = self.timer_period;
            self.seq_pos = (self.seq_pos + 1) & 7;
        } else {
            self.timer_val -= 1;
        }
    }

    fn step_envelope(&mut self) {
        if self.env_start {
            self.env_start = false;
            self.env_decay = 15;
            self.env_divider = self.volume;
        } else if self.env_divider == 0 {
            self.env_divider = self.volume;
            if self.env_decay > 0 {
                self.env_decay -= 1;
            } else if self.length_halt {
                self.env_decay = 15;
            }
        } else {
            self.env_divider -= 1;
        }
    }

    fn step_length(&mut self) {
        if !self.length_halt && self.length_counter > 0 {
            self.length_counter -= 1;
        }
    }

    fn step_sweep(&mut self) {
        let change = self.timer_period >> self.sweep_shift;
        let target = if self.sweep_negate {
            if self.is_channel_1 {
                self.timer_period.wrapping_sub(change).wrapping_sub(1)
            } else {
                self.timer_period.wrapping_sub(change)
            }
        } else {
            self.timer_period.wrapping_add(change)
        };
        let muting = self.timer_period < 8 || target > 0x7FF;

        if self.sweep_divider == 0 && self.sweep_enabled && !muting && self.sweep_shift > 0 {
            self.timer_period = target;
        }
        if self.sweep_divider == 0 || self.sweep_reload {
            self.sweep_divider = self.sweep_period;
            self.sweep_reload = false;
        } else {
            self.sweep_divider -= 1;
        }
    }

    fn output(&self) -> u8 {
        if !self.enabled
            || self.length_counter == 0
            || self.timer_period < 8
            || self.timer_period > 0x7FF
            || DUTY_TABLE[self.duty as usize][self.seq_pos as usize] == 0
        {
            return 0;
        }
        if self.constant_volume {
            self.volume
        } else {
            self.env_decay
        }
    }
}

#[derive(Clone, Default)]
pub struct TriangleChannel {
    pub enabled: bool,
    pub control_flag: bool,
    pub linear_reload_val: u8,
    pub linear_counter: u8,
    pub linear_reload: bool,
    pub timer_period: u16,
    pub timer_val: u16,
    pub seq_pos: u8,
    pub length_counter: u8,
}

impl TriangleChannel {
    fn step_timer(&mut self) {
        if self.timer_val == 0 {
            self.timer_val = self.timer_period;
            if self.length_counter > 0 && self.linear_counter > 0 {
                self.seq_pos = (self.seq_pos + 1) & 31;
            }
        } else {
            self.timer_val -= 1;
        }
    }

    fn step_linear(&mut self) {
        if self.linear_reload {
            self.linear_counter = self.linear_reload_val;
        } else if self.linear_counter > 0 {
            self.linear_counter -= 1;
        }
        if !self.control_flag {
            self.linear_reload = false;
        }
    }

    fn step_length(&mut self) {
        if !self.control_flag && self.length_counter > 0 {
            self.length_counter -= 1;
        }
    }

    fn output(&self) -> u8 {
        if !self.enabled || self.timer_period < 2 {
            return 7;
        }
        TRIANGLE_TABLE[self.seq_pos as usize]
    }
}

#[derive(Clone)]
pub struct NoiseChannel {
    pub enabled: bool,
    pub length_halt: bool,
    pub constant_volume: bool,
    pub volume: u8,
    pub env_start: bool,
    pub env_divider: u8,
    pub env_decay: u8,
    pub mode: bool,
    pub timer_period: u16,
    pub timer_val: u16,
    pub shift_reg: u16,
    pub length_counter: u8,
}

impl Default for NoiseChannel {
    fn default() -> Self {
        Self {
            enabled: false,
            length_halt: false,
            constant_volume: false,
            volume: 0,
            env_start: false,
            env_divider: 0,
            env_decay: 0,
            mode: false,
            timer_period: NOISE_PERIOD_TABLE[0],
            timer_val: 0,
            shift_reg: 1,
            length_counter: 0,
        }
    }
}

impl NoiseChannel {
    fn step_timer(&mut self) {
        if self.timer_val == 0 {
            self.timer_val = self.timer_period;
            let tap = if self.mode {
                (self.shift_reg >> 6) & 1
            } else {
                (self.shift_reg >> 1) & 1
            };
            let feedback = (self.shift_reg & 1) ^ tap;
            self.shift_reg = (self.shift_reg >> 1) | (feedback << 14);
        } else {
            self.timer_val -= 1;
        }
    }

    fn step_envelope(&mut self) {
        if self.env_start {
            self.env_start = false;
            self.env_decay = 15;
            self.env_divider = self.volume;
        } else if self.env_divider == 0 {
            self.env_divider = self.volume;
            if self.env_decay > 0 {
                self.env_decay -= 1;
            } else if self.length_halt {
                self.env_decay = 15;
            }
        } else {
            self.env_divider -= 1;
        }
    }

    fn step_length(&mut self) {
        if !self.length_halt && self.length_counter > 0 {
            self.length_counter -= 1;
        }
    }

    fn output(&self) -> u8 {
        if !self.enabled || self.length_counter == 0 || (self.shift_reg & 1) != 0 {
            return 0;
        }
        if self.constant_volume {
            self.volume
        } else {
            self.env_decay
        }
    }
}

#[derive(Clone)]
pub struct DmcChannel {
    pub enabled: bool,
    pub irq_enabled: bool,
    pub loop_flag: bool,
    pub timer_period: u16,
    pub timer_val: u16,
    pub output_level: u8,
    pub sample_addr: u16,
    pub sample_length: u16,
    pub current_addr: u16,
    pub bytes_remaining: u16,
    pub shift_reg: u8,
    pub bits_remaining: u8,
    pub silence: bool,
    /// DMC IRQ flag ($4015 bit 7): set when a non-looping sample finishes with IRQ enabled.
    pub irq_flag: bool,
    /// CPU cycles stolen by DMC sample-fetch DMA, consumed by the CPU core.
    pub stall_cycles: u32,
}

impl Default for DmcChannel {
    fn default() -> Self {
        Self {
            enabled: false,
            irq_enabled: false,
            loop_flag: false,
            timer_period: DMC_RATE_TABLE[0],
            timer_val: DMC_RATE_TABLE[0],
            output_level: 0,
            sample_addr: 0xC000,
            sample_length: 1,
            current_addr: 0xC000,
            bytes_remaining: 0,
            shift_reg: 0,
            bits_remaining: 8,
            silence: true,
            irq_flag: false,
            stall_cycles: 0,
        }
    }
}

impl DmcChannel {
    fn step_timer(&mut self, mapper: &Mapper) {
        if self.timer_val > 0 {
            self.timer_val -= 1;
            return;
        }
        self.timer_val = self.timer_period;

        if !self.silence {
            if (self.shift_reg & 1) != 0 {
                if self.output_level <= 125 {
                    self.output_level += 2;
                }
            } else if self.output_level >= 2 {
                self.output_level -= 2;
            }
            self.shift_reg >>= 1;
        }

        if self.bits_remaining > 0 {
            self.bits_remaining -= 1;
        }

        if self.bits_remaining == 0 {
            self.bits_remaining = 8;
            if self.bytes_remaining > 0 {
                self.shift_reg = mapper.read_prg(self.current_addr);
                // DMA halts the 6502 for ~4 cycles per sample byte fetch.
                self.stall_cycles += 4;
                self.current_addr = if self.current_addr == 0xFFFF {
                    0x8000
                } else {
                    self.current_addr + 1
                };
                self.bytes_remaining -= 1;
                self.silence = false;
                if self.bytes_remaining == 0 {
                    if self.loop_flag {
                        self.current_addr = self.sample_addr;
                        self.bytes_remaining = self.sample_length;
                    } else if self.irq_enabled {
                        self.irq_flag = true;
                    }
                }
            } else {
                self.silence = true;
            }
        }
    }
}

#[derive(Clone)]
pub struct Apu {
    pub pulse1: PulseChannel,
    pub pulse2: PulseChannel,
    pub triangle: TriangleChannel,
    pub noise: NoiseChannel,
    pub dmc: DmcChannel,
    pub frame_counter_mode: u8,
    pub frame_step_cycles: u32,
    /// $4017 bit 6: inhibit the frame-counter IRQ.
    pub frame_irq_inhibit: bool,
    /// Frame-counter IRQ flag ($4015 bit 6), raised at the end of each 4-step sequence.
    pub frame_irq: bool,
    pub cpu_cycle_parity: bool,
    pub sample_accum: f64,
    pub hp_prev_in: f32,
    pub hp_prev_out: f32,
    pub audio_samples: Vec<i16>,
}

impl Default for Apu {
    fn default() -> Self {
        Self::new()
    }
}

impl Apu {
    pub fn new() -> Self {
        let mut p1 = PulseChannel::default();
        p1.is_channel_1 = true;
        Self {
            pulse1: p1,
            pulse2: PulseChannel::default(),
            triangle: TriangleChannel::default(),
            noise: NoiseChannel::default(),
            dmc: DmcChannel::default(),
            frame_counter_mode: 0,
            frame_step_cycles: 0,
            frame_irq_inhibit: false,
            frame_irq: false,
            cpu_cycle_parity: false,
            sample_accum: 0.0,
            hp_prev_in: 0.0,
            hp_prev_out: 0.0,
            audio_samples: Vec::with_capacity(1024),
        }
    }

    pub fn write_reg(&mut self, addr: u16, val: u8) {
        match addr {
            0x4000 => {
                self.pulse1.duty = (val >> 6) & 3;
                self.pulse1.length_halt = (val & 0x20) != 0;
                self.pulse1.constant_volume = (val & 0x10) != 0;
                self.pulse1.volume = val & 0x0F;
            }
            0x4001 => {
                self.pulse1.sweep_enabled = (val & 0x80) != 0;
                self.pulse1.sweep_period = (val >> 4) & 7;
                self.pulse1.sweep_negate = (val & 0x08) != 0;
                self.pulse1.sweep_shift = val & 7;
                self.pulse1.sweep_reload = true;
            }
            0x4002 => {
                self.pulse1.timer_period = (self.pulse1.timer_period & 0x0700) | (val as u16);
            }
            0x4003 => {
                self.pulse1.timer_period =
                    (self.pulse1.timer_period & 0x00FF) | (((val & 7) as u16) << 8);
                if self.pulse1.enabled {
                    self.pulse1.length_counter = LENGTH_TABLE[(val >> 3) as usize];
                }
                self.pulse1.seq_pos = 0;
                self.pulse1.env_start = true;
            }
            0x4004 => {
                self.pulse2.duty = (val >> 6) & 3;
                self.pulse2.length_halt = (val & 0x20) != 0;
                self.pulse2.constant_volume = (val & 0x10) != 0;
                self.pulse2.volume = val & 0x0F;
            }
            0x4005 => {
                self.pulse2.sweep_enabled = (val & 0x80) != 0;
                self.pulse2.sweep_period = (val >> 4) & 7;
                self.pulse2.sweep_negate = (val & 0x08) != 0;
                self.pulse2.sweep_shift = val & 7;
                self.pulse2.sweep_reload = true;
            }
            0x4006 => {
                self.pulse2.timer_period = (self.pulse2.timer_period & 0x0700) | (val as u16);
            }
            0x4007 => {
                self.pulse2.timer_period =
                    (self.pulse2.timer_period & 0x00FF) | (((val & 7) as u16) << 8);
                if self.pulse2.enabled {
                    self.pulse2.length_counter = LENGTH_TABLE[(val >> 3) as usize];
                }
                self.pulse2.seq_pos = 0;
                self.pulse2.env_start = true;
            }
            0x4008 => {
                self.triangle.control_flag = (val & 0x80) != 0;
                self.triangle.linear_reload_val = val & 0x7F;
            }
            0x400A => {
                self.triangle.timer_period = (self.triangle.timer_period & 0x0700) | (val as u16);
            }
            0x400B => {
                self.triangle.timer_period =
                    (self.triangle.timer_period & 0x00FF) | (((val & 7) as u16) << 8);
                if self.triangle.enabled {
                    self.triangle.length_counter = LENGTH_TABLE[(val >> 3) as usize];
                }
                self.triangle.linear_reload = true;
            }
            0x400C => {
                self.noise.length_halt = (val & 0x20) != 0;
                self.noise.constant_volume = (val & 0x10) != 0;
                self.noise.volume = val & 0x0F;
            }
            0x400E => {
                self.noise.mode = (val & 0x80) != 0;
                self.noise.timer_period = NOISE_PERIOD_TABLE[(val & 0x0F) as usize];
            }
            0x400F => {
                if self.noise.enabled {
                    self.noise.length_counter = LENGTH_TABLE[(val >> 3) as usize];
                }
                self.noise.env_start = true;
            }
            0x4010 => {
                self.dmc.irq_enabled = (val & 0x80) != 0;
                if !self.dmc.irq_enabled {
                    self.dmc.irq_flag = false;
                }
                self.dmc.loop_flag = (val & 0x40) != 0;
                self.dmc.timer_period = DMC_RATE_TABLE[(val & 0x0F) as usize];
            }
            0x4011 => {
                self.dmc.output_level = val & 0x7F;
            }
            0x4012 => {
                self.dmc.sample_addr = 0xC000 + ((val as u16) << 6);
            }
            0x4013 => {
                self.dmc.sample_length = ((val as u16) << 4) + 1;
            }
            0x4015 => {
                self.pulse1.enabled = (val & 0x01) != 0;
                if !self.pulse1.enabled {
                    self.pulse1.length_counter = 0;
                }
                self.pulse2.enabled = (val & 0x02) != 0;
                if !self.pulse2.enabled {
                    self.pulse2.length_counter = 0;
                }
                self.triangle.enabled = (val & 0x04) != 0;
                if !self.triangle.enabled {
                    self.triangle.length_counter = 0;
                }
                self.noise.enabled = (val & 0x08) != 0;
                if !self.noise.enabled {
                    self.noise.length_counter = 0;
                }
                self.dmc.irq_flag = false;
                self.dmc.enabled = (val & 0x10) != 0;
                if !self.dmc.enabled {
                    self.dmc.bytes_remaining = 0;
                } else if self.dmc.bytes_remaining == 0 {
                    self.dmc.current_addr = self.dmc.sample_addr;
                    self.dmc.bytes_remaining = self.dmc.sample_length;
                }
            }
            0x4017 => {
                self.frame_counter_mode = (val >> 7) & 1;
                self.frame_irq_inhibit = (val & 0x40) != 0;
                if self.frame_irq_inhibit {
                    self.frame_irq = false;
                }
                self.frame_step_cycles = 0;
                if self.frame_counter_mode == 1 {
                    self.clock_quarter_frame();
                    self.clock_half_frame();
                }
            }
            _ => {}
        }
    }

    /// True while either APU interrupt source is asserting the CPU IRQ line.
    #[inline]
    pub fn irq_pending(&self) -> bool {
        self.frame_irq || self.dmc.irq_flag
    }

    /// $4015 read. Reading acknowledges (clears) the frame IRQ but not the DMC IRQ.
    pub fn read_status(&mut self) -> u8 {
        let mut status = 0u8;
        if self.frame_irq {
            status |= 0x40;
        }
        if self.dmc.irq_flag {
            status |= 0x80;
        }
        self.frame_irq = false;
        if self.pulse1.length_counter > 0 {
            status |= 0x01;
        }
        if self.pulse2.length_counter > 0 {
            status |= 0x02;
        }
        if self.triangle.length_counter > 0 {
            status |= 0x04;
        }
        if self.noise.length_counter > 0 {
            status |= 0x08;
        }
        if self.dmc.bytes_remaining > 0 {
            status |= 0x10;
        }
        status
    }

    fn clock_quarter_frame(&mut self) {
        self.pulse1.step_envelope();
        self.pulse2.step_envelope();
        self.triangle.step_linear();
        self.noise.step_envelope();
    }

    fn clock_half_frame(&mut self) {
        self.pulse1.step_length();
        self.pulse1.step_sweep();
        self.pulse2.step_length();
        self.pulse2.step_sweep();
        self.triangle.step_length();
        self.noise.step_length();
    }

    pub fn step(&mut self, cpu_cycles: u32, mapper: &Mapper) {
        for _ in 0..cpu_cycles {
            self.triangle.step_timer();
            self.dmc.step_timer(mapper);

            self.cpu_cycle_parity = !self.cpu_cycle_parity;
            if self.cpu_cycle_parity {
                self.pulse1.step_timer();
                self.pulse2.step_timer();
                self.noise.step_timer();
            }

            self.frame_step_cycles += 1;
            if self.frame_counter_mode == 0 {
                match self.frame_step_cycles {
                    7457 => self.clock_quarter_frame(),
                    14913 => {
                        self.clock_quarter_frame();
                        self.clock_half_frame();
                    }
                    22371 => self.clock_quarter_frame(),
                    29829 => {
                        self.clock_quarter_frame();
                        self.clock_half_frame();
                        if !self.frame_irq_inhibit {
                            self.frame_irq = true;
                        }
                        self.frame_step_cycles = 0;
                    }
                    _ => {}
                }
            } else {
                match self.frame_step_cycles {
                    7457 => self.clock_quarter_frame(),
                    14913 => {
                        self.clock_quarter_frame();
                        self.clock_half_frame();
                    }
                    22371 => self.clock_quarter_frame(),
                    37281 => {
                        self.clock_quarter_frame();
                        self.clock_half_frame();
                        self.frame_step_cycles = 0;
                    }
                    _ => {}
                }
            }

            self.sample_accum += SAMPLE_RATE as f64;
            if self.sample_accum >= CPU_CLOCK_HZ {
                self.sample_accum -= CPU_CLOCK_HZ;
                let raw = self.mix_output();
                let filtered = raw - self.hp_prev_in + 0.996 * self.hp_prev_out;
                self.hp_prev_in = raw;
                self.hp_prev_out = filtered;
                let sample = (filtered * 22000.0).clamp(-32767.0, 32767.0) as i16;
                self.audio_samples.push(sample);
            }
        }
    }

    fn mix_output(&self) -> f32 {
        let p1 = self.pulse1.output() as f32;
        let p2 = self.pulse2.output() as f32;
        let pulse_out = if p1 + p2 > 0.0 {
            95.88 / ((8128.0 / (p1 + p2)) + 100.0)
        } else {
            0.0
        };

        let t = self.triangle.output() as f32;
        let n = self.noise.output() as f32;
        let d = self.dmc.output_level as f32;
        let tnd_denom = (t / 8227.0) + (n / 12241.0) + (d / 22638.0);
        let tnd_out = if tnd_denom > 0.0 {
            159.79 / ((1.0 / tnd_denom) + 100.0)
        } else {
            0.0
        };
        pulse_out + tnd_out
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
