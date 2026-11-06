// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones

//! Standard NES controller shift register ($4016 / $4017).

/// A button (bit 0 of the controller report).
pub const BTN_A: u8 = 0x01;
/// B button.
pub const BTN_B: u8 = 0x02;
/// Select button.
pub const BTN_SELECT: u8 = 0x04;
/// Start button.
pub const BTN_START: u8 = 0x08;
/// D-pad up.
pub const BTN_UP: u8 = 0x10;
/// D-pad down.
pub const BTN_DOWN: u8 = 0x20;
/// D-pad left.
pub const BTN_LEFT: u8 = 0x40;
/// D-pad right (bit 7: reported last).
pub const BTN_RIGHT: u8 = 0x80;

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// Two standard NES controllers behind the $4016/$4017 serial ports.
///
/// Writing 1 then 0 to $4016 latches the buttons into an 8-bit shift register; each read
/// returns the next button (A, B, Select, Start, Up, Down, Left, Right), then 1s.
#[derive(Clone, Default)]
pub struct Joypad {
    /// Player 1 buttons (`BTN_*` bits) set by the front end for this frame.
    pub buttons_p1: u8,
    /// Live button state shared with the UI thread. Sampled at the moment the game
    /// strobes $4016, so the freshest input is used instead of the frame-start value.
    pub live_p1: Option<Arc<AtomicU8>>,
    /// Player 2 buttons (`BTN_*` bits).
    pub buttons_p2: u8,
    /// Player 1 shift register; bit 0 is the next bit returned by $4016. Refilled with 1s
    /// from the top as it shifts, so reads past the eighth return 1.
    shift_p1: u8,
    /// Player 2 shift register, read via $4017 (same behaviour as `shift_p1`).
    shift_p2: u8,
    /// Strobe (bit 0 of the last $4016 write). While set, the shift registers are held
    /// loaded and reads always return the current A button.
    strobe: bool,
}

impl Joypad {
    /// Both controllers idle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Frame-start buttons OR'd with the live state; opposing D-pad directions are
    /// masked (impossible on a real pad, and some games crash on them).
    #[inline]
    pub fn effective_p1(&self) -> u8 {
        let mut b = self.buttons_p1 | self.live_p1.as_ref().map_or(0, |l| l.load(Ordering::Acquire));
        if b & (BTN_UP | BTN_DOWN) == BTN_UP | BTN_DOWN {
            b &= !(BTN_UP | BTN_DOWN);
        }
        if b & (BTN_LEFT | BTN_RIGHT) == BTN_LEFT | BTN_RIGHT {
            b &= !(BTN_LEFT | BTN_RIGHT);
        }
        b
    }

    /// $4016 write: while bit 0 is 1 the shift registers continuously reload.
    pub fn write_strobe(&mut self, val: u8) {
        self.strobe = (val & 1) != 0;
        if self.strobe {
            self.shift_p1 = self.effective_p1();
            self.shift_p2 = self.buttons_p2;
        }
    }

    /// $4016 read: next player-1 button bit (bit 0 of the result).
    pub fn read_p1(&mut self) -> u8 {
        if self.strobe {
            return self.effective_p1() & 1;
        }
        let bit = self.shift_p1 & 1;
        self.shift_p1 = (self.shift_p1 >> 1) | 0x80;
        bit
    }

    /// $4017 read: next player-2 button bit.
    pub fn read_p2(&mut self) -> u8 {
        if self.strobe {
            return self.buttons_p2 & 1;
        }
        let bit = self.shift_p2 & 1;
        self.shift_p2 = (self.shift_p2 >> 1) | 0x80;
        bit
    }
}
