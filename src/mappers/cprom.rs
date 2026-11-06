//! Mapper 13: CPROM (Videomation) - 16 KB CHR-RAM, upper 4 KB switchable.

use super::{CartBase, Mapper};
use crate::cartridge::Mirroring;

#[derive(Clone)]
pub struct CpRom {
    base: CartBase,
}

impl CpRom {
    pub fn new(mut base: CartBase) -> Self {
        if base.chr_ram.len() < 0x4000 {
            base.chr_ram = vec![0; 0x4000];
        }
        base.set_prg_32k(0);
        base.set_chr_4k(0, 0);
        base.set_chr_4k(1, 0);
        base.set_mirroring(Mirroring::Vertical);
        Self { base }
    }
}

impl Mapper for CpRom {
    fn base(&self) -> &CartBase { &self.base }
    fn base_mut(&mut self) -> &mut CartBase { &mut self.base }
    fn name(&self) -> &'static str { "CPROM (13)" }

    fn write_register(&mut self, addr: u16, val: u8, _cycle: u64) {
        let v = val & self.base.read_prg(addr).unwrap_or(0xFF);
        self.base.set_chr_4k(1, (v & 3) as usize);
    }
}
