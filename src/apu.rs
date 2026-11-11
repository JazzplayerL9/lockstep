// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones
//
// Portions derived from Mesen (https://github.com/nesdev-org/MesenCE):
// the frame counter, length counter and DMC timing.
// Mesen is Copyright (C) 2014-2026 Sour and contributors, GPL-3.0-or-later.

//! Ricoh 2A03 APU, clocked once per CPU cycle: frame counter with exact $4017 write
//! delays, length counters with the reload/halt timing quirks, envelopes, sweeps, and a
//! DMC whose sample fetches are performed as real DMA by the CPU core.

/// Output sample rate used until the audio device reports its own (Hz).
pub const DEFAULT_SAMPLE_RATE: f64 = 48_000.0;
/// NTSC CPU clock rate (Hz); one APU `clock` call per CPU cycle.
const CPU_CLOCK_HZ: f64 = 1_789_773.0;

/// Length counter load values, indexed by the 5-bit index in bits 7-3 of
/// $4003/$4007/$400B/$400F.
static LENGTH_TABLE: [u8; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96, 22, 192, 24, 72, 26, 16, 28, 32, 30,
];
/// Pulse waveform sequences for the four duty settings ($4000/$4004 bits 7-6):
/// 12.5%, 25%, 50% and 25% negated. Indexed by `Square::duty_pos`, which counts down.
static DUTY_TABLE: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1],
    [0, 0, 0, 0, 0, 0, 1, 1],
    [0, 0, 0, 0, 1, 1, 1, 1],
    [1, 1, 1, 1, 1, 1, 0, 0],
];
/// 32-step triangle waveform: 15 down to 0, then 0 back up to 15.
static TRIANGLE_TABLE: [u8; 32] = [
    15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];
/// Noise timer periods in CPU cycles (NTSC), indexed by $400E bits 3-0.
static NOISE_PERIODS: [u16; 16] = [4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068];
/// DMC output-unit periods in CPU cycles (NTSC), indexed by $4010 bits 3-0.
static DMC_PERIODS: [u16; 16] = [428, 380, 340, 320, 286, 254, 226, 214, 190, 160, 142, 128, 106, 84, 72, 54];
/// Frame counter step boundaries in CPU cycles since the start of the sequence (from Mesen).
/// Row 0 is 4-step mode, row 1 is 5-step mode. In 4-step mode the last three entries are
/// the three consecutive cycles on which the frame IRQ flag is set. After the sixth step
/// the sequence restarts from 0.
static STEP_CYCLES: [[u32; 6]; 2] = [[7457, 14913, 22371, 29828, 29829, 29830], [7457, 14913, 22371, 29829, 37281, 37282]];
/// Frame counter clock fired at each step of [`STEP_CYCLES`]: quarter frame clocks the
/// envelopes and triangle linear counter; half frame additionally clocks the length
/// counters and sweep units.
// 0 = none, 1 = quarter, 2 = half
static STEP_TYPE: [[u8; 6]; 2] = [[1, 2, 1, 0, 2, 0], [1, 2, 1, 0, 2, 0]];

/// Non-linear 2A03 mixer, tabulated: pulse[p1+p2], tnd[3*tri + 2*noise + dmc].
struct MixTables {
    /// Pulse output for `p1 + p2` (0-30): `95.52 / (8128 / n + 100)`, 0 for n = 0.
    pulse: [f32; 31],
    /// Triangle/noise/DMC output for `3*tri + 2*noise + dmc` (0-202):
    /// `163.67 / (24329 / n + 100)`, 0 for n = 0.
    tnd: [f32; 203],
}

/// Returns the mixer lookup tables, computing them on first use (shared process-wide).
fn mix_tables() -> &'static MixTables {
    static T: std::sync::OnceLock<MixTables> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = MixTables { pulse: [0.0; 31], tnd: [0.0; 203] };
        for (i, v) in t.pulse.iter_mut().enumerate().skip(1) {
            *v = 95.52 / (8128.0 / i as f32 + 100.0);
        }
        for (i, v) in t.tnd.iter_mut().enumerate().skip(1) {
            *v = 163.67 / (24329.0 / i as f32 + 100.0);
        }
        t
    })
}

/// Length counter, shared by the pulse, triangle and noise channels.
///
/// Register writes are not applied immediately: `load` and `init` latch the reload value
/// and halt flag, and `reload` commits them on the next APU clock, after the frame counter
/// has run for that cycle. This reproduces two hardware quirks (as in Mesen): a reload
/// written on the cycle the counter is clocked by a half-frame tick is ignored if that tick
/// decremented the counter, and a halt-flag change on that cycle only takes effect after
/// the tick.
#[derive(Clone, Default)]
struct LengthCounter {
    /// Channel enable bit from $4015. While clear, the counter is held at 0 and loads are ignored.
    enabled: bool,
    /// Halt flag currently in effect: stops the counter from decrementing. Doubles as the
    /// envelope loop flag (pulse/noise) and the triangle's linear control flag.
    halt: bool,
    /// Halt flag from the last register write; committed to `halt` by `reload`.
    new_halt: bool,
    /// Current length count; the channel is silenced while it is 0.
    counter: u8,
    /// Pending reload value from [`LENGTH_TABLE`]; 0 means no reload is pending.
    reload_value: u8,
    /// `counter` at the time of the pending load, so `reload` can detect that a frame
    /// counter tick changed it in between.
    previous_value: u8,
}

impl LengthCounter {
    /// Latches a new halt flag ($4000/$4004/$400C bit 5, or $4008 bit 7); applied by the next `reload`.
    fn init(&mut self, halt: bool) {
        self.new_halt = halt;
    }
    /// Latches a reload from length index `idx` (bits 7-3 of the channel's fourth register),
    /// applied by the next `reload`. Ignored while the channel is disabled.
    fn load(&mut self, idx: u8) {
        if self.enabled {
            self.reload_value = LENGTH_TABLE[idx as usize];
            self.previous_value = self.counter;
        }
    }
    /// Commits latched writes; called on the APU cycle after any register write. The reload
    /// is dropped if the counter changed since the write (it was clocked in between). The
    /// latched halt flag is committed either way.
    fn reload(&mut self) {
        if self.reload_value != 0 {
            if self.counter == self.previous_value {
                self.counter = self.reload_value;
            }
            self.reload_value = 0;
        }
        self.halt = self.new_halt;
    }
    /// Half-frame clock: decrements the counter unless it is 0 or halted.
    fn tick(&mut self) {
        if self.counter > 0 && !self.halt {
            self.counter -= 1;
        }
    }
    /// Applies the channel's $4015 enable bit; disabling clears the counter immediately.
    fn set_enabled(&mut self, e: bool) {
        if !e {
            self.counter = 0;
        }
        self.enabled = e;
    }
}

/// Volume envelope for the pulse and noise channels.
///
/// Also owns the channel's length counter, since its halt flag doubles as the envelope
/// loop flag and `volume()` returns 0 once the length counter has expired.
#[derive(Clone, Default)]
struct Envelope {
    /// Constant-volume flag (bit 4 of $4000/$4004/$400C): output `volume` directly
    /// instead of the decay level.
    constant: bool,
    /// Low 4 bits of the register (0-15): the constant volume, or the divider period in decay mode.
    volume: u8,
    /// Start flag, set by a write to the channel's fourth register; restarts the decay on
    /// the next quarter-frame clock.
    start: bool,
    /// Divider counting down from `volume` once per quarter frame. Signed so it can reach -1,
    /// which triggers a decay step and a reload.
    divider: i8,
    /// Decay level (0-15), output when `constant` is clear.
    counter: u8,
    /// The channel's length counter.
    lc: LengthCounter,
}

impl Envelope {
    /// Handles a write to the channel's first register ($4000/$4004/$400C): halt/loop flag
    /// (latched in the length counter), constant-volume flag and volume/period.
    fn init(&mut self, v: u8) {
        self.lc.init(v & 0x20 != 0);
        self.constant = v & 0x10 != 0;
        self.volume = v & 0x0F;
    }
    /// Current envelope output (0-15), or 0 when the length counter is 0.
    fn volume(&self) -> u8 {
        if self.lc.counter > 0 {
            if self.constant { self.volume } else { self.counter }
        } else {
            0
        }
    }
    /// Quarter-frame clock. If the start flag is set, clears it and restarts the decay at 15.
    /// Otherwise clocks the divider; when it expires, the decay level is decremented, or
    /// wrapped from 0 back to 15 if looping (length counter halt flag set).
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

/// Pulse channel: pulse 1 at $4000-$4003, pulse 2 at $4004-$4007.
#[derive(Clone, Default)]
struct Square {
    /// Envelope, which also holds the length counter.
    env: Envelope,
    /// True for pulse 1, whose sweep negation is ones' complement (subtracts an extra 1).
    channel1: bool,
    /// Timer countdown in CPU cycles; at 0 it reloads from `period` and steps the sequencer.
    timer: u16,
    /// Timer reload in CPU cycles: `real_period * 2 + 1`, because the hardware pulse timer is
    /// clocked every other CPU cycle.
    period: u16,
    /// 11-bit period (0-$7FF) as written to the registers or updated by the sweep unit.
    real_period: u16,
    /// Duty cycle selector (0-3), bits 7-6 of $4000/$4004; row of [`DUTY_TABLE`].
    duty: u8,
    /// Sequencer position (0-7) in [`DUTY_TABLE`]. Counts down; reset to 0 by a write to
    /// $4003/$4007.
    duty_pos: u8,
    /// Sweep enable flag (bit 7 of $4001/$4005).
    sweep_enabled: bool,
    /// Sweep divider period in half frames: bits 6-4 plus 1 (1-8).
    sweep_period: u8,
    /// Sweep negate flag (bit 3): the target period is below the current period.
    sweep_negate: bool,
    /// Sweep shift count (bits 2-0, 0-7). A shift of 0 never updates the period.
    sweep_shift: u8,
    /// Set by a $4001/$4005 write; reloads the sweep divider on the next half-frame clock.
    reload_sweep: bool,
    /// Sweep divider countdown; the period is updated when it reaches 0.
    sweep_divider: u8,
    /// Target period computed by `update_target`. Wider than 11 bits so an overflow above
    /// $7FF (which mutes the channel) is visible; may wrap around when negating.
    sweep_target: u32,
}

impl Square {
    /// Recomputes `sweep_target` from the current period and sweep settings. Called whenever
    /// the period or the sweep register changes, since the target also drives muting.
    fn update_target(&mut self) {
        let shift = (self.real_period >> self.sweep_shift) as u32;
        let rp = self.real_period as u32;
        self.sweep_target = if self.sweep_negate {
            rp.wrapping_sub(shift).wrapping_sub(self.channel1 as u32)
        } else {
            rp + shift
        };
    }
    /// Sets the 11-bit period, derives the CPU-cycle timer reload and updates the sweep target.
    fn set_period(&mut self, p: u16) {
        self.real_period = p;
        self.period = p * 2 + 1;
        self.update_target();
    }
    /// Whether the sweep unit silences the channel: period below 8, or (when not negating)
    /// a target above $7FF. Applies even when the sweep is disabled.
    fn muted(&self) -> bool {
        self.real_period < 8 || (!self.sweep_negate && self.sweep_target > 0x7FF)
    }
    /// Handles a write to one of the channel's four registers (selected by `reg & 3`). The
    /// fourth register also loads the length counter, resets the sequencer and restarts the
    /// envelope.
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
    /// Half-frame clock of the sweep unit. When the divider reaches 0 and the sweep is enabled
    /// with a non-zero shift and the channel isn't muted, the target period is applied. A
    /// pending reload then resets the divider to `sweep_period`.
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
    /// Advances the timer by one CPU cycle. Returns true when it expired and stepped the
    /// sequencer (so the output may have changed).
    #[inline]
    fn clock(&mut self) -> bool {
        if self.timer == 0 {
            self.timer = self.period;
            self.duty_pos = self.duty_pos.wrapping_sub(1) & 7;
            true
        } else {
            self.timer -= 1;
            false
        }
    }
    /// Current output (0-15): duty sequence bit times envelope volume, or 0 when muted by the
    /// sweep unit.
    #[inline]
    fn output(&self) -> u8 {
        if self.muted() { 0 } else { DUTY_TABLE[self.duty as usize][self.duty_pos as usize] * self.env.volume() }
    }
}

/// Triangle channel ($4008-$400B).
#[derive(Clone, Default)]
struct Triangle {
    /// Length counter, halted by the linear control flag.
    lc: LengthCounter,
    /// Timer countdown in CPU cycles (the triangle timer runs at the full CPU rate).
    timer: u16,
    /// 11-bit timer period from $400A/$400B, in CPU cycles minus 1.
    period: u16,
    /// Sequencer position (0-31) in [`TRIANGLE_TABLE`].
    pos: u8,
    /// Linear counter; the sequencer only advances while it and the length counter are both non-zero.
    linear_counter: u8,
    /// Linear counter reload value ($4008 bits 6-0).
    linear_reload_value: u8,
    /// Control flag ($4008 bit 7): halts the length counter and keeps the linear reload flag set.
    linear_control: bool,
    /// Linear counter reload flag, set by a $400B write.
    linear_reload: bool,
    /// Current output level (0-15). Holds its last value while the sequencer is stopped or
    /// the period is ultrasonic (below 2).
    out: u8,
}

impl Triangle {
    /// Handles a write to $4008, $400A or $400B (selected by `reg & 3`); $4009 is unused.
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
    /// Quarter-frame clock of the linear counter: reloads it if the reload flag is set,
    /// otherwise decrements it. The reload flag is then cleared unless the control flag is set.
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
    /// Advances the timer by one CPU cycle, stepping the sequencer on expiry if both counters
    /// are non-zero. Returns true when the timer expired.
    #[inline]
    fn clock(&mut self) -> bool {
        if self.timer == 0 {
            self.timer = self.period;
            if self.lc.counter > 0 && self.linear_counter > 0 {
                self.pos = (self.pos + 1) & 31;
                // Ultrasonic periods are filtered out (avoids popping), as most emulators do.
                if self.period >= 2 {
                    self.out = TRIANGLE_TABLE[self.pos as usize];
                }
            }
            true
        } else {
            self.timer -= 1;
            false
        }
    }
}

/// Noise channel ($400C-$400F).
#[derive(Clone)]
struct Noise {
    /// Envelope, which also holds the length counter.
    env: Envelope,
    /// Timer countdown in CPU cycles.
    timer: u16,
    /// Timer reload: the [`NOISE_PERIODS`] entry minus 1.
    period: u16,
    /// 15-bit linear feedback shift register (1 at power-on); output is muted while bit 0 is set.
    shift: u16,
    /// Mode flag ($400E bit 7): feedback taken from bit 6 instead of bit 1, giving a short,
    /// metallic-sounding sequence.
    mode: bool,
}

impl Noise {
    /// Handles a write to $400C, $400E or $400F (selected by `reg & 3`); $400D is unused.
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
    /// Advances the timer by one CPU cycle. On expiry clocks the LFSR (feedback is bit 0 XOR
    /// bit 1, or bit 6 in short mode, shifted into bit 14) and returns true.
    #[inline]
    fn clock(&mut self) -> bool {
        if self.timer == 0 {
            self.timer = self.period;
            let tap = if self.mode { 6 } else { 1 };
            let fb = (self.shift & 1) ^ ((self.shift >> tap) & 1);
            self.shift = (self.shift >> 1) | (fb << 14);
            true
        } else {
            self.timer -= 1;
            false
        }
    }
    /// Current output (0-15): envelope volume, or 0 while LFSR bit 0 is set.
    #[inline]
    fn output(&self) -> u8 {
        if self.shift & 1 != 0 { 0 } else { self.env.volume() }
    }
}

/// Delta modulation channel ($4010-$4013).
///
/// The channel doesn't read memory itself: when its buffer needs a byte it raises
/// [`DmcDmaRequest::Start`], the CPU core performs the DMA read from
/// `Apu::dmc_read_address` and hands the byte back via `Apu::set_dmc_read_buffer`.
#[derive(Clone)]
struct Dmc {
    /// Output-unit timer countdown in CPU cycles.
    timer: u16,
    /// Timer reload: the [`DMC_PERIODS`] entry minus 1.
    period: u16,
    /// IRQ enable ($4010 bit 7). Clearing it also acknowledges a pending DMC IRQ.
    irq_enabled: bool,
    /// Loop flag ($4010 bit 6): restart the sample when it ends instead of raising an IRQ.
    loop_flag: bool,
    /// 7-bit output level (0-127); set directly by $4011 and moved by 2 per sample bit.
    output_level: u8,
    /// Sample start address: $C000 + $4012 * 64.
    sample_addr: u16,
    /// Sample length in bytes: $4013 * 16 + 1.
    sample_length: u16,
    /// Address of the next byte to fetch; wraps from $FFFF to $8000.
    current_addr: u16,
    /// Bytes still to be fetched in the current sample. Non-zero means the DMC is active
    /// (reported in $4015 bit 4).
    bytes_remaining: u16,
    /// Sample buffer: the last fetched byte, waiting to be moved into the shift register.
    read_buffer: u8,
    /// Whether `read_buffer` is empty (and a DMA fetch is wanted).
    buffer_empty: bool,
    /// Output shift register; bit 0 decides the next +2/-2 step of `output_level`.
    shift: u8,
    /// Bits left in the current 8-bit output cycle (8 down to 1).
    bits_remaining: u8,
    /// Silence flag: set when an output cycle began with an empty buffer, in which case the
    /// output level is left unchanged for that cycle.
    silence: bool,
    /// CPU cycles until the first DMA fetch is requested after a $4015 write enables the DMC
    /// (2 or 3 depending on write-cycle parity, from Mesen). While non-zero, the output unit
    /// doesn't request fetches itself; 0 when idle.
    transfer_start_delay: u8,
    /// CPU cycles until a $4015 disable takes effect (2 or 3 depending on write-cycle parity,
    /// from Mesen). On expiry `bytes_remaining` is cleared and [`DmcDmaRequest::Stop`] cancels
    /// a fetch that hasn't started; 0 when idle.
    disable_delay: u8,
    /// DMC IRQ flag ($4015 bit 7); asserts the APU IRQ line while set. Cleared by any $4015
    /// write or by clearing the IRQ enable in $4010.
    irq_flag: bool,
}

impl Dmc {
    /// Power-on state: rate index 0 (slowest), sample address $C000, length 1, empty buffer,
    /// silenced output.
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
    /// Restarts the sample: reloads the current address and byte count from the $4012/$4013 values.
    fn init_sample(&mut self) {
        self.current_addr = self.sample_addr;
        self.bytes_remaining = self.sample_length;
    }
}

/// DMA requests raised by the APU for the CPU to service.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum DmcDmaRequest {
    /// No request pending.
    #[default]
    None,
    /// The DMC's sample buffer emptied: steal CPU cycles to fetch the next byte.
    Start,
    /// A $4015 write disabled the DMC: cancel a fetch that hasn't started yet.
    Stop,
}

/// The 2A03 audio unit: two pulse channels, triangle, noise, DMC and the frame counter.
///
/// Clocked once per CPU cycle by [`crate::cpu::System`]. Produces mono `f32` samples at
/// [`Apu::output_rate`] into [`Apu::audio_samples`], which the front end drains each frame.
#[repr(C)]
#[derive(Clone)]
pub struct Apu {
    /// Pulse 1 ($4000-$4003).
    sq1: Square,
    /// Pulse 2 ($4004-$4007).
    sq2: Square,
    /// Triangle ($4008-$400B).
    tri: Triangle,
    /// Noise ($400C-$400F).
    noise: Noise,
    /// DMC ($4010-$4013).
    dmc: Dmc,
    // Frame counter.
    /// CPU cycles elapsed in the current frame counter sequence.
    fc_previous: u32,
    /// Index (0-5) of the next step in [`STEP_CYCLES`].
    fc_step: usize,
    /// Sequencer mode, row of [`STEP_CYCLES`]: 0 = 4-step, 1 = 5-step ($4017 bit 7).
    fc_mode: usize,
    /// IRQ inhibit flag ($4017 bit 6).
    fc_inhibit_irq: bool,
    /// Remaining CPU cycles during which further frame ticks are suppressed (set to 2 after each
    /// tick), so a $4017 write selecting 5-step mode can't clock the units twice in a row
    /// together with a regular step.
    fc_block_tick: u8,
    /// Value of a pending $4017 write, or -1 if none; applied when `fc_write_delay` expires.
    fc_new_value: i16,
    /// CPU cycles until the pending $4017 write takes effect (3 or 4 depending on write-cycle
    /// parity); -1 when no write is pending.
    fc_write_delay: i8,
    /// Frame interrupt flag, as read from $4015 bit 6.
    frame_irq_flag: bool,
    /// Whether the frame counter is asserting the CPU IRQ line. Kept separate from
    /// `frame_irq_flag` because with IRQs inhibited the flag can be set without asserting the line.
    frame_irq_line: bool,
    /// Pending DMC DMA start/stop for the CPU.
    pub dma_request: DmcDmaRequest,
    // Output.
    /// Resampling phase accumulator: `output_rate` is added each CPU cycle and a sample is
    /// emitted each time it reaches [`CPU_CLOCK_HZ`].
    sample_accum: f64,
    /// Sum of the mixed output over the CPU cycles since the last emitted sample (box-filter
    /// decimation).
    sample_sum: f32,
    /// Number of CPU cycles summed in `sample_sum`.
    sample_count: u32,
    /// Output rate the filter coefficients below were computed for (0 = not yet).
    filter_rate: f64,
    /// Coefficient of the 90 Hz first-order high-pass (`RC / (RC + dt)`).
    hp1_coef: f32,
    /// Coefficient of the 440 Hz first-order high-pass.
    hp2_coef: f32,
    /// Coefficient of the 14 kHz first-order low-pass (`dt / (RC + dt)`).
    lp_coef: f32,
    /// Previous input of the first (90 Hz) high-pass filter.
    hp1_prev_in: f32,
    /// Previous output of the first high-pass filter.
    hp1_prev_out: f32,
    /// Previous input of the second high-pass filter.
    hp2_prev_in: f32,
    /// Previous output of the second high-pass filter.
    hp2_prev_out: f32,
    /// Previous output of the low-pass filter.
    lp_prev: f32,
    /// Packed channel levels (`pulse | tnd << 5`) that `mix_value` was computed from; lets
    /// `update_mix` skip the table lookup when nothing changed. `u32::MAX` forces a recompute.
    mix_key: u32,
    /// Mixed output of the five channels for the current cycle (expansion audio excluded).
    mix_value: f32,
    /// Forces `update_mix` on the next cycle even if no channel timer fired. Set by register
    /// writes, frame counter ticks and length counter reloads, which can change channel output
    /// without a timer event.
    mix_dirty: bool,
    /// Set by every register write so the next `clock` commits the deferred length counter
    /// reloads and halt flags (see `LengthCounter`).
    lc_pending: bool,
    /// Output samples (mono, roughly -1..1) produced since the front end last drained them.
    pub audio_samples: Vec<f32>,
    /// Output sample rate. The front end nudges this by fractions of a percent
    /// (dynamic rate control) to keep the audio device's buffer level steady.
    pub output_rate: f64,
    /// Expansion audio contribution for the current cycle (set by the system).
    pub expansion: f32,
}

impl Default for Apu {
    fn default() -> Self {
        Self::new()
    }
}

impl Apu {
    /// Power-on state.
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
            filter_rate: 0.0,
            hp1_coef: 0.0,
            hp2_coef: 0.0,
            lp_coef: 0.0,
            hp1_prev_in: 0.0,
            hp1_prev_out: 0.0,
            hp2_prev_in: 0.0,
            hp2_prev_out: 0.0,
            lp_prev: 0.0,
            mix_key: u32::MAX,
            mix_value: 0.0,
            mix_dirty: true,
            lc_pending: false,
            audio_samples: Vec::with_capacity(2048),
            output_rate: DEFAULT_SAMPLE_RATE,
            expansion: 0.0,
        };
        a.sq1.update_target();
        a
    }

    /// Soft reset: silences channels; the frame counter mode is kept (acts like a $4017 rewrite).
    pub fn reset(&mut self) {
        let mode = self.fc_mode;
        let samples = std::mem::take(&mut self.audio_samples);
        let rate = self.output_rate;
        *self = Self::new();
        self.audio_samples = samples;
        self.output_rate = rate;
        self.fc_new_value = if mode == 1 { 0x80 } else { 0 };
    }

    /// State of the APU's IRQ output (frame counter or DMC), as seen by the CPU.
    #[inline]
    pub fn irq_line(&self) -> bool {
        self.frame_irq_line || self.dmc.irq_flag
    }

    /// Applies a frame counter tick. Every tick (`kind` 1 or 2) clocks the envelopes and the
    /// triangle linear counter; a half-frame tick (`kind == 2`) also clocks the length counters
    /// and sweep units.
    fn frame_tick(&mut self, kind: u8) {
        self.mix_dirty = true;
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

    /// Advances the frame counter by one CPU cycle (timing from Mesen).
    ///
    /// When the current step's boundary is reached: in 4-step mode the last three steps set the
    /// frame IRQ (only the flag, cleared again on the last step, when inhibited), the step's
    /// quarter/half-frame tick fires unless `fc_block_tick` suppresses it, and the sequence
    /// wraps after step 5. Then a pending $4017 write is counted down; when it lands the
    /// sequence restarts, and selecting 5-step mode immediately produces a half-frame tick.
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

    /// Requests a DMC DMA fetch if the sample buffer is empty and bytes remain.
    fn dmc_start_transfer(&mut self) {
        if self.dmc.buffer_empty && self.dmc.bytes_remaining > 0 {
            self.dma_request = DmcDmaRequest::Start;
        }
    }

    /// CPU address the DMC wants to fetch its next sample byte from.
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

    /// Counts down the DMC's $4015 disable and transfer-start delays, applying each on expiry.
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

    /// Advances the DMC output unit by one CPU cycle. On timer expiry, applies one shift
    /// register bit to the output level (unless silenced); at the end of an 8-bit output cycle,
    /// reloads the shift register from the sample buffer and requests the next fetch (unless
    /// a transfer-start delay is pending). Returns true on timer expiry.
    fn dmc_clock(&mut self) -> bool {
        if self.dmc.timer != 0 {
            self.dmc.timer -= 1;
            return false;
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
        true
    }

    /// Advance one CPU cycle.
    #[inline]
    pub fn clock(&mut self) {
        self.dmc_process_clock();
        self.frame_counter_clock();
        // Length-counter reloads only matter right after a register write.
        if self.lc_pending {
            self.lc_pending = false;
            self.sq1.env.lc.reload();
            self.sq2.env.lc.reload();
            self.noise.env.lc.reload();
            self.tri.lc.reload();
            self.mix_dirty = true;
        }
        // Non-short-circuit `|`: every timer must run each cycle.
        let fired = self.sq1.clock() | self.sq2.clock() | self.noise.clock() | self.tri.clock() | self.dmc_clock();
        if fired || self.mix_dirty {
            self.mix_dirty = false;
            self.update_mix();
        }
        self.mix_cycle();
    }

    /// Recomputes the mixed output; only needed when a channel's output may have changed.
    #[inline]
    fn update_mix(&mut self) {
        let p = (self.sq1.output() + self.sq2.output()) as u32;
        let tnd = 3 * self.tri.out as u32 + 2 * self.noise.output() as u32 + self.dmc.output_level as u32;
        let key = p | (tnd << 5);
        if key != self.mix_key {
            let t = mix_tables();
            self.mix_key = key;
            self.mix_value = t.pulse[p as usize] + t.tnd[tnd as usize];
        }
    }

    /// Accumulates this cycle's output and, when the resampler is due, emits one sample: the
    /// average since the last sample (a box-filter decimator) passed through the NES output
    /// stage (90 Hz and 440 Hz high-pass, 14 kHz low-pass), with 1.2x gain, clamped to -1..1.
    /// Samples are dropped once 8192 are queued.
    #[inline]
    fn mix_cycle(&mut self) {
        self.sample_sum += self.mix_value + self.expansion;
        self.sample_count += 1;

        self.sample_accum += self.output_rate;
        if self.sample_accum >= CPU_CLOCK_HZ {
            self.sample_accum -= CPU_CLOCK_HZ;
            let raw = self.sample_sum / self.sample_count as f32;
            self.sample_sum = 0.0;
            self.sample_count = 0;
            // The NES's analogue output stage (nesdev wiki, "APU Mixer"): first-order
            // high-pass filters at 90 Hz and 440 Hz and a first-order low-pass at 14 kHz.
            // Coefficients follow the (DRC-adjusted) output rate.
            if self.output_rate != self.filter_rate {
                self.filter_rate = self.output_rate;
                let dt = 1.0 / self.output_rate;
                let rc = |hz: f64| 1.0 / (2.0 * std::f64::consts::PI * hz);
                self.hp1_coef = (rc(90.0) / (rc(90.0) + dt)) as f32;
                self.hp2_coef = (rc(440.0) / (rc(440.0) + dt)) as f32;
                self.lp_coef = (dt / (rc(14_000.0) + dt)) as f32;
            }
            let hp1 = self.hp1_coef * (self.hp1_prev_out + raw - self.hp1_prev_in);
            self.hp1_prev_in = raw;
            self.hp1_prev_out = hp1;
            let hp2 = self.hp2_coef * (self.hp2_prev_out + hp1 - self.hp2_prev_in);
            self.hp2_prev_in = hp1;
            self.hp2_prev_out = hp2;
            let lp = self.lp_prev + self.lp_coef * (hp2 - self.lp_prev);
            self.lp_prev = lp;
            let s = (lp * 1.2).clamp(-1.0, 1.0);
            if self.audio_samples.len() < 8192 {
                self.audio_samples.push(s);
            }
        }
    }

    /// Reads $4015 (channel length-counter status and IRQ flags). Acknowledges the frame IRQ.
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

    /// Writes an APU register ($4000-$4013, $4015, $4017). `cpu_cycle` determines the
    /// odd/even-cycle-dependent delays of $4015 and $4017 writes.
    pub fn write_register(&mut self, addr: u16, val: u8, cpu_cycle: u64) {
        self.lc_pending = true;
        self.mix_dirty = true;
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

    /// Serializes the APU state into `out`.
    pub fn save_state(&self, out: &mut Vec<u8>) {
        let pod_len = std::mem::offset_of!(Self, audio_samples);
        out.extend_from_slice(&(pod_len as u32).to_le_bytes());
        // SAFETY: All fields of `Apu` before `audio_samples` are plain-old-data.
        let pod = unsafe { std::slice::from_raw_parts(self as *const Self as *const u8, pod_len) };
        out.extend_from_slice(pod);
        out.extend_from_slice(&self.output_rate.to_le_bytes());
        out.extend_from_slice(&self.expansion.to_le_bytes());
    }

    /// Restores the APU state from `cur`.
    pub fn load_state(&mut self, cur: &mut &[u8]) -> Result<(), String> {
        let pod_len = std::mem::offset_of!(Self, audio_samples);
        if cur.len() < 4 {
            return Err("Truncated save state (APU header)".into());
        }
        let (len_b, rest) = cur.split_at(4);
        *cur = rest;
        let saved_pod = u32::from_le_bytes(len_b.try_into().unwrap()) as usize;
        if saved_pod != pod_len || cur.len() < pod_len + 12 {
            return Err("APU state size mismatch".into());
        }
        let (pod, rest2) = cur.split_at(pod_len);
        let (tail, rest3) = rest2.split_at(12);
        *cur = rest3;
        // SAFETY: All fields before `audio_samples` are plain-old-data.
        unsafe {
            std::ptr::copy_nonoverlapping(pod.as_ptr(), self as *mut Self as *mut u8, pod_len);
        }
        self.output_rate = f64::from_le_bytes(tail[0..8].try_into().unwrap());
        self.expansion = f32::from_le_bytes(tail[8..12].try_into().unwrap());
        self.audio_samples.clear();
        Ok(())
    }
}
