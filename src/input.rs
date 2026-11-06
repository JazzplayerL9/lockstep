//! Standard NES controller shift register ($4016 / $4017).

pub const BTN_A: u8 = 0x01;
pub const BTN_B: u8 = 0x02;
pub const BTN_SELECT: u8 = 0x04;
pub const BTN_START: u8 = 0x08;
pub const BTN_UP: u8 = 0x10;
pub const BTN_DOWN: u8 = 0x20;
pub const BTN_LEFT: u8 = 0x40;
pub const BTN_RIGHT: u8 = 0x80;

#[derive(Clone, Default)]
pub struct Joypad {
    pub buttons_p1: u8,
    pub buttons_p2: u8,
    shift_p1: u8,
    shift_p2: u8,
    strobe: bool,
}

impl Joypad {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write_strobe(&mut self, val: u8) {
        self.strobe = (val & 1) != 0;
        if self.strobe {
            self.shift_p1 = self.buttons_p1;
            self.shift_p2 = self.buttons_p2;
        }
    }

    pub fn read_p1(&mut self) -> u8 {
        if self.strobe {
            return self.buttons_p1 & 1;
        }
        let bit = self.shift_p1 & 1;
        self.shift_p1 = (self.shift_p1 >> 1) | 0x80;
        bit
    }

    pub fn read_p2(&mut self) -> u8 {
        if self.strobe {
            return self.buttons_p2 & 1;
        }
        let bit = self.shift_p2 & 1;
        self.shift_p2 = (self.shift_p2 >> 1) | 0x80;
        bit
    }
}
