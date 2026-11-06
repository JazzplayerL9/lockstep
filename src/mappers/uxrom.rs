//! Mapper 2: UxROM (UNROM/UOROM), and mapper 71: Camerica BF909x.

use super::{CartBase, Mapper};
use crate::cartridge::Mirroring;

#[derive(Clone)]
pub struct UxRom {
    base: CartBase,
    bus_conflicts: bool,
    camerica: bool,
}

impl UxRom {
    pub fn new(mut base: CartBase, bus_conflicts: bool) -> Self {
        base.set_prg_16k(0, 0);
        base.set_prg_16k(1, -1);
        Self { base, bus_conflicts, camerica: false }
    }

    pub fn new_camerica(base: CartBase) -> Self {
        let mut m = Self::new(base, false);
        m.camerica = true;
        m
    }
}

impl Mapper for UxRom {
    fn base(&self) -> &CartBase { &self.base }
    fn base_mut(&mut self) -> &mut CartBase { &mut self.base }
    fn name(&self) -> &'static str { if self.camerica { "Camerica (71)" } else { "UxROM (2)" } }

    fn write_register(&mut self, addr: u16, mut val: u8, _cycle: u64) {
        if self.camerica {
            if addr < 0xC000 {
                // Fire Hawk one-screen mirroring control.
                if addr >= 0x9000 {
                    self.base.set_mirroring(if val & 0x10 != 0 { Mirroring::OneScreenUpper } else { Mirroring::OneScreenLower });
                }
                return;
            }
        } else if self.bus_conflicts {
            val &= self.base.read_prg(addr).unwrap_or(0xFF);
        }
        self.base.set_prg_16k(0, val as i32);
    }
}
