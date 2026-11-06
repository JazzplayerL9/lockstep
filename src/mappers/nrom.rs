//! Mapper 0: NROM.

use super::{CartBase, Mapper};

#[derive(Clone)]
pub struct Nrom {
    base: CartBase,
}

impl Nrom {
    pub fn new(base: CartBase) -> Self {
        Self { base }
    }
}

impl Mapper for Nrom {
    fn base(&self) -> &CartBase { &self.base }
    fn base_mut(&mut self) -> &mut CartBase { &mut self.base }
    fn name(&self) -> &'static str { "NROM (0)" }
}
