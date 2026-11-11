// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones

//! Standard NES controller shift registers ($4016 / $4017) and plug-and-play USB / Bluetooth
//! gamepad input via `gilrs` (Linux `evdev`/`udev`, Windows `XInput`/`RawInput`).

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

/// Analog stick deflection threshold (0.0-1.0) required to register a D-pad direction.
const STICK_DEADZONE: f32 = 0.35;

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use gilrs::{Axis, Button, Event, EventType, Gamepad, GamepadId, Gilrs};

/// Masks opposing D-pad directions (Up+Down or Left+Right), which are physically impossible
/// on a real NES D-pad and cause glitches in several games (e.g. Zelda, Ninja Gaiden).
#[inline]
pub fn sanitize_dpad(mut b: u8) -> u8 {
    if b & (BTN_UP | BTN_DOWN) == BTN_UP | BTN_DOWN {
        b &= !(BTN_UP | BTN_DOWN);
    }
    if b & (BTN_LEFT | BTN_RIGHT) == BTN_LEFT | BTN_RIGHT {
        b &= !(BTN_LEFT | BTN_RIGHT);
    }
    b
}

/// Two standard NES controllers behind the $4016/$4017 serial ports.
///
/// Writing 1 then 0 to $4016 latches the buttons into an 8-bit shift register; each read
/// returns the next button (A, B, Select, Start, Up, Down, Left, Right), then 1s.
#[derive(Clone, Default)]
pub struct Joypad {
    /// Player 1 buttons (`BTN_*` bits) set by the front end for this frame.
    pub buttons_p1: u8,
    /// Live Player 1 button state shared with the UI thread. Sampled at the moment the game
    /// strobes $4016, so the freshest input is used instead of the frame-start value.
    pub live_p1: Option<Arc<AtomicU8>>,
    /// Player 2 buttons (`BTN_*` bits) set by the front end for this frame.
    pub buttons_p2: u8,
    /// Live Player 2 button state shared with the UI thread, sampled when $4016 is strobed.
    pub live_p2: Option<Arc<AtomicU8>>,
    /// Player 1 shift register; bit 0 is the next bit returned by $4016. Refilled with 1s
    /// from the top as it shifts, so reads past the eighth return 1.
    shift_p1: u8,
    /// Player 2 shift register, read via $4017 (same behaviour as `shift_p1`).
    shift_p2: u8,
    /// Strobe (bit 0 of the last $4016 write). While set, the shift registers are held
    /// loaded and reads always return the current A button.
    strobe: bool,
    /// Whether the NES Zapper light gun is active on controller port 2 (`$4017` bits 3 and 4).
    pub zapper_connected: bool,
    /// Current Zapper aim X coordinate in NES pixels (`0..256`, or `-1` when aimed off-screen).
    pub zapper_x: i32,
    /// Current Zapper aim Y coordinate in NES pixels (`0..240`, or `-1` when aimed off-screen).
    pub zapper_y: i32,
    /// Remaining frames for which the Zapper trigger switch (`$4017` bit 4) is held active (`0` = released).
    pub zapper_trigger_frames: u8,
}

impl Joypad {
    /// Both controllers idle.
    pub fn new() -> Self {
        Self {
            zapper_x: -1,
            zapper_y: -1,
            ..Self::default()
        }
    }

    /// Updates Zapper aim coordinates (`0..256`, `0..240`, or `-1` for off-screen) and optional trigger pull.
    pub fn set_zapper(&mut self, x: i32, y: i32, trigger_frames: u8) {
        self.zapper_x = x;
        self.zapper_y = y;
        if trigger_frames > 0 {
            self.zapper_connected = true;
            self.zapper_trigger_frames = trigger_frames;
        }
    }

    /// Advances the Zapper trigger hold counter by one frame (called once per VBlank).
    pub fn step_zapper_frame(&mut self) {
        if self.zapper_trigger_frames > 0 {
            self.zapper_trigger_frames -= 1;
        }
    }

    /// Serializes controller shift-register and Zapper state into `out`.
    pub fn save_state(&self, out: &mut Vec<u8>) {
        out.push(self.buttons_p1);
        out.push(self.buttons_p2);
        out.push(self.shift_p1);
        out.push(self.shift_p2);
        out.push(self.strobe as u8);
        out.push(self.zapper_connected as u8);
        out.extend_from_slice(&self.zapper_x.to_le_bytes());
        out.extend_from_slice(&self.zapper_y.to_le_bytes());
        out.push(self.zapper_trigger_frames);
    }

    /// Restores controller shift-register and Zapper state from `cur`.
    pub fn load_state(&mut self, cur: &mut &[u8]) -> Result<(), String> {
        if cur.len() < 15 {
            return Err("Truncated save state (Joypad)".into());
        }
        let (b, rest) = cur.split_at(15);
        *cur = rest;
        self.buttons_p1 = b[0];
        self.buttons_p2 = b[1];
        self.shift_p1 = b[2];
        self.shift_p2 = b[3];
        self.strobe = b[4] != 0;
        self.zapper_connected = b[5] != 0;
        self.zapper_x = i32::from_le_bytes(b[6..10].try_into().unwrap());
        self.zapper_y = i32::from_le_bytes(b[10..14].try_into().unwrap());
        self.zapper_trigger_frames = b[14];
        Ok(())
    }

    /// Player 1 frame-start buttons OR'd with the live state; opposing D-pad directions are
    /// masked (impossible on a real pad, and some games crash on them).
    #[inline]
    pub fn effective_p1(&self) -> u8 {
        let b = self.buttons_p1 | self.live_p1.as_ref().map_or(0, |l| l.load(Ordering::Acquire));
        sanitize_dpad(b)
    }

    /// Player 2 frame-start buttons OR'd with the live state, with opposing D-pad directions masked.
    #[inline]
    pub fn effective_p2(&self) -> u8 {
        let b = self.buttons_p2 | self.live_p2.as_ref().map_or(0, |l| l.load(Ordering::Acquire));
        sanitize_dpad(b)
    }

    /// $4016 write: while bit 0 is 1 the shift registers continuously reload.
    pub fn write_strobe(&mut self, val: u8) {
        self.strobe = (val & 1) != 0;
        if self.strobe {
            self.shift_p1 = self.effective_p1();
            self.shift_p2 = self.effective_p2();
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
            return self.effective_p2() & 1;
        }
        let bit = self.shift_p2 & 1;
        self.shift_p2 = (self.shift_p2 >> 1) | 0x80;
        bit
    }
}

/// Snapshot of gamepad state and hot-plug notifications from one [`GamepadHub::poll`] call.
#[derive(Default, Debug, Clone)]
pub struct GamepadPoll {
    /// Player 1 held NES buttons (`BTN_*`).
    pub p1_held: u8,
    /// Player 1 buttons newly pressed since the previous poll.
    pub p1_pressed: u8,
    /// Player 2 held NES buttons (`BTN_*`).
    pub p2_held: u8,
    /// Player 2 buttons newly pressed since the previous poll.
    pub p2_pressed: u8,
    /// Guide / Mode button (or Select+Start chord) pressed to toggle the ROM browser.
    pub toggle_browser: bool,
    /// Left shoulder (L1) held on Player 1's pad (used for page-up in the ROM browser).
    pub p1_page_up: bool,
    /// Right shoulder (R1) held on Player 1's pad (used for page-down in the ROM browser).
    pub p1_page_down: bool,
    /// Hot-plug toast messages (e.g. `"Gamepad 1 connected: Xbox Series X Controller"`).
    pub notifications: Vec<String>,
}

/// Plug-and-play USB / Bluetooth gamepad manager backed by `gilrs`.
///
/// Automatically detects controllers present at start-up as well as controllers plugged in or
/// unplugged at runtime, assigning up to two active controllers to Player 1 and Player 2.
pub struct GamepadHub {
    /// Underlying `gilrs` context, or `None` if the OS input subsystem could not be opened.
    gilrs: Option<Gilrs>,
    /// Gamepad currently assigned to Player 1.
    p1_id: Option<GamepadId>,
    /// Gamepad currently assigned to Player 2.
    p2_id: Option<GamepadId>,
    /// Previous poll's held mask for Player 1 (used to detect rising edges on analog sticks).
    prev_p1: u8,
    /// Previous poll's held mask for Player 2.
    prev_p2: u8,
    /// Whether `Select+Start` was already held on the previous poll (so the browser toggle fires once).
    prev_chord_toggle: bool,
    /// When true (`LOCKSTEP_PAD_SWAP=1`), swaps South and East face buttons (Nintendo A/B layout).
    pub swap_ab: bool,
}

impl Default for GamepadHub {
    fn default() -> Self {
        Self::new()
    }
}

impl GamepadHub {
    /// Initialises `gilrs` and binds any already-connected gamepads to Player 1 / Player 2.
    pub fn new() -> Self {
        let swap_ab = std::env::var_os("LOCKSTEP_PAD_SWAP").is_some();
        let mut hub = Self {
            gilrs: match Gilrs::new() {
                Ok(g) => Some(g),
                Err(e) => {
                    eprintln!("[lockstep] gamepad support unavailable: {e}");
                    None
                }
            },
            p1_id: None,
            p2_id: None,
            prev_p1: 0,
            prev_p2: 0,
            prev_chord_toggle: false,
            swap_ab,
        };
        hub.reassign_slots();
        if let Some(g) = &hub.gilrs {
            if let Some(id) = hub.p1_id {
                eprintln!("[lockstep] Gamepad P1: {}", g.gamepad(id).name());
            }
            if let Some(id) = hub.p2_id {
                eprintln!("[lockstep] Gamepad P2: {}", g.gamepad(id).name());
            }
        }
        hub
    }

    /// Ensures `p1_id` and `p2_id` reference connected gamepads, filling any vacant slot from
    /// the currently connected devices.
    fn reassign_slots(&mut self) {
        let Some(gilrs) = &self.gilrs else { return };
        if self.p1_id.is_some_and(|id| !gilrs.gamepad(id).is_connected()) {
            self.p1_id = None;
        }
        if self.p2_id.is_some_and(|id| !gilrs.gamepad(id).is_connected()) {
            self.p2_id = None;
        }
        // If P1 disconnected while P2 is still connected, promote P2 to P1.
        if self.p1_id.is_none() && self.p2_id.is_some() {
            self.p1_id = self.p2_id.take();
        }
        for (id, pad) in gilrs.gamepads() {
            if !pad.is_connected() {
                continue;
            }
            if self.p1_id.is_none() && self.p2_id != Some(id) {
                self.p1_id = Some(id);
            } else if self.p2_id.is_none() && self.p1_id != Some(id) {
                self.p2_id = Some(id);
            }
        }
    }

    /// Maps a single `gilrs::Button` to its NES button mask (`BTN_*`), respecting `swap_ab`.
    #[inline]
    fn map_button(btn: Button, swap_ab: bool) -> u8 {
        match btn {
            Button::South => if swap_ab { BTN_B } else { BTN_A },
            Button::East => if swap_ab { BTN_A } else { BTN_B },
            Button::North => BTN_A,
            Button::West => BTN_B,
            Button::RightTrigger | Button::RightTrigger2 => BTN_A,
            Button::LeftTrigger | Button::LeftTrigger2 => BTN_B,
            Button::Select => BTN_SELECT,
            Button::Start => BTN_START,
            Button::DPadUp => BTN_UP,
            Button::DPadDown => BTN_DOWN,
            Button::DPadLeft => BTN_LEFT,
            Button::DPadRight => BTN_RIGHT,
            _ => 0,
        }
    }

    /// Reads the current held NES button state (buttons + left analog stick) of `pad`.
    fn sample_gamepad(&self, pad: Gamepad<'_>) -> u8 {
        let mut mask = 0u8;
        for btn in [
            Button::South,
            Button::East,
            Button::North,
            Button::West,
            Button::LeftTrigger,
            Button::LeftTrigger2,
            Button::RightTrigger,
            Button::RightTrigger2,
            Button::Select,
            Button::Start,
            Button::DPadUp,
            Button::DPadDown,
            Button::DPadLeft,
            Button::DPadRight,
        ] {
            if pad.is_pressed(btn) {
                mask |= Self::map_button(btn, self.swap_ab);
            }
        }

        let lx = pad.value(Axis::LeftStickX);
        let ly = pad.value(Axis::LeftStickY);
        if lx <= -STICK_DEADZONE {
            mask |= BTN_LEFT;
        } else if lx >= STICK_DEADZONE {
            mask |= BTN_RIGHT;
        }
        // In gilrs, positive LeftStickY is Up and negative is Down.
        if ly >= STICK_DEADZONE {
            mask |= BTN_UP;
        } else if ly <= -STICK_DEADZONE {
            mask |= BTN_DOWN;
        }

        sanitize_dpad(mask)
    }

    /// Drains all pending `gilrs` events (hot-plugs, button presses) and samples the current
    /// state of Player 1 and Player 2 controllers.
    pub fn poll(&mut self) -> GamepadPoll {
        let mut out = GamepadPoll::default();
        let mut event_pressed_p1 = 0u8;
        let mut event_pressed_p2 = 0u8;
        let mut mode_pressed = false;

        if let Some(gilrs) = &mut self.gilrs {
            while let Some(Event { id, event, .. }) = gilrs.next_event() {
                match event {
                    EventType::Connected => {
                        let name = gilrs.gamepad(id).name().to_string();
                        if self.p1_id.is_none() && self.p2_id != Some(id) {
                            self.p1_id = Some(id);
                            out.notifications.push(format!("Gamepad P1 connected: {name}"));
                        } else if self.p2_id.is_none() && self.p1_id != Some(id) {
                            self.p2_id = Some(id);
                            out.notifications.push(format!("Gamepad P2 connected: {name}"));
                        } else {
                            out.notifications.push(format!("Gamepad connected: {name}"));
                        }
                    }
                    EventType::Disconnected => {
                        let name = gilrs.gamepad(id).name().to_string();
                        if self.p1_id == Some(id) {
                            self.p1_id = None;
                            out.notifications.push(format!("Gamepad P1 disconnected: {name}"));
                        } else if self.p2_id == Some(id) {
                            self.p2_id = None;
                            out.notifications.push(format!("Gamepad P2 disconnected: {name}"));
                        }
                    }
                    EventType::ButtonPressed(btn, _) => {
                        // Auto-assign an unassigned active pad when a button is pressed on it.
                        if self.p1_id.is_none() && self.p2_id != Some(id) {
                            self.p1_id = Some(id);
                        } else if self.p2_id.is_none() && self.p1_id != Some(id) {
                            self.p2_id = Some(id);
                        }
                        if btn == Button::Mode {
                            mode_pressed = true;
                        }
                        let bit = Self::map_button(btn, self.swap_ab);
                        if Some(id) == self.p1_id {
                            event_pressed_p1 |= bit;
                        } else if Some(id) == self.p2_id {
                            event_pressed_p2 |= bit;
                        }
                    }
                    _ => {}
                }
            }
        }

        self.reassign_slots();

        if let Some(gilrs) = &self.gilrs {
            if let Some(id) = self.p1_id {
                let pad = gilrs.gamepad(id);
                if pad.is_connected() {
                    out.p1_held = self.sample_gamepad(pad);
                    out.p1_page_up = pad.is_pressed(Button::LeftTrigger);
                    out.p1_page_down = pad.is_pressed(Button::RightTrigger);
                }
            }
            if let Some(id) = self.p2_id {
                let pad = gilrs.gamepad(id);
                if pad.is_connected() {
                    out.p2_held = self.sample_gamepad(pad);
                }
            }
        }

        out.p1_pressed = (out.p1_held & !self.prev_p1) | event_pressed_p1;
        out.p2_pressed = (out.p2_held & !self.prev_p2) | event_pressed_p2;
        self.prev_p1 = out.p1_held;
        self.prev_p2 = out.p2_held;

        // Guide/Mode button or pressing Select+Start together toggles the ROM browser.
        let chord = (out.p1_held & (BTN_SELECT | BTN_START)) == (BTN_SELECT | BTN_START);
        if mode_pressed || (chord && !self.prev_chord_toggle) {
            out.toggle_browser = true;
        }
        self.prev_chord_toggle = chord;

        out
    }

    /// Short description of connected gamepads for the ROM browser status header.
    pub fn status_summary(&self) -> String {
        let Some(gilrs) = &self.gilrs else {
            return "Pad: unavailable".to_string();
        };
        match (self.p1_id, self.p2_id) {
            (Some(p1), Some(p2)) => format!(
                "Pads: P1 {} | P2 {}",
                gilrs.gamepad(p1).name(),
                gilrs.gamepad(p2).name()
            ),
            (Some(p1), None) => format!("Pad P1: {}", gilrs.gamepad(p1).name()),
            _ => "Pad: none (plug in any USB/BT controller)".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpad_sanitization_masks_opposing_axes() {
        assert_eq!(sanitize_dpad(BTN_UP | BTN_DOWN | BTN_A), BTN_A);
        assert_eq!(sanitize_dpad(BTN_LEFT | BTN_RIGHT | BTN_B), BTN_B);
        assert_eq!(sanitize_dpad(BTN_UP | BTN_RIGHT), BTN_UP | BTN_RIGHT);
    }

    #[test]
    fn joypad_shift_registers_and_live_p1_p2_sampling() {
        let mut pad = Joypad::new();
        let live1 = Arc::new(AtomicU8::new(BTN_A | BTN_UP | BTN_DOWN));
        let live2 = Arc::new(AtomicU8::new(BTN_B | BTN_RIGHT));
        pad.live_p1 = Some(live1);
        pad.live_p2 = Some(live2);
        pad.buttons_p1 = BTN_START;
        pad.buttons_p2 = BTN_SELECT;

        // Latch and release strobe: P1 should have A + Start (Up+Down cancelled),
        // P2 should have B + Select + Right.
        pad.write_strobe(1);
        pad.write_strobe(0);

        let mut p1_read = 0u8;
        let mut p2_read = 0u8;
        for i in 0..8 {
            p1_read |= (pad.read_p1() & 1) << i;
            p2_read |= (pad.read_p2() & 1) << i;
        }
        assert_eq!(p1_read, BTN_A | BTN_START);
        assert_eq!(p2_read, BTN_B | BTN_SELECT | BTN_RIGHT);
        // 9th read returns 1.
        assert_eq!(pad.read_p1(), 1);
        assert_eq!(pad.read_p2(), 1);
    }

    #[test]
    fn gamepad_button_mapping_default_and_swapped() {
        assert_eq!(GamepadHub::map_button(Button::South, false), BTN_A);
        assert_eq!(GamepadHub::map_button(Button::East, false), BTN_B);
        assert_eq!(GamepadHub::map_button(Button::West, false), BTN_B);
        assert_eq!(GamepadHub::map_button(Button::North, false), BTN_A);
        assert_eq!(GamepadHub::map_button(Button::South, true), BTN_B);
        assert_eq!(GamepadHub::map_button(Button::East, true), BTN_A);
    }
}

