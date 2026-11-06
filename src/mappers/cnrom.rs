//! Mapper 3: CNROM (8 KB CHR bank switching, with bus conflicts).

use super::{CartBase, Mapper};

#[derive(Clone)]
pub struct CnRom {
    base: CartBase,
}

impl CnRom {
    pub fn new(base: CartBase) -> Self {
        Self { base }
    }
}

impl Mapper for CnRom {
    fn base(&self) -> &CartBase { &self.base }
    fn base_mut(&mut self) -> &mut CartBase { &mut self.base }
    fn name(&self) -> &'static str { "CNROM (3)" }

    fn write_register(&mut self, addr: u16, val: u8, _cycle: u64) {
        let v = val & self.base.read_prg(addr).unwrap_or(0xFF);
        self.base.set_chr_8k(v as usize);
    }
}
