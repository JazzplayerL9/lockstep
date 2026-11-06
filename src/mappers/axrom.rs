//! Mapper 7: AxROM (32 KB PRG switching + one-screen mirroring select).

use super::{CartBase, Mapper};
use crate::cartridge::Mirroring;

#[derive(Clone)]
pub struct AxRom {
    base: CartBase,
}

impl AxRom {
    pub fn new(mut base: CartBase) -> Self {
        base.set_prg_32k(0);
        base.set_mirroring(Mirroring::OneScreenLower);
        Self { base }
    }
}

impl Mapper for AxRom {
    fn base(&self) -> &CartBase { &self.base }
    fn base_mut(&mut self) -> &mut CartBase { &mut self.base }
    fn name(&self) -> &'static str { "AxROM (7)" }

    fn write_register(&mut self, _addr: u16, val: u8, _cycle: u64) {
        self.base.set_prg_32k((val & 0x0F) as i32);
        self.base.set_mirroring(if val & 0x10 != 0 { Mirroring::OneScreenUpper } else { Mirroring::OneScreenLower });
    }
}
