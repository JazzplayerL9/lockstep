// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Zach Jones

//! Mapper 0: NROM.

use super::{CartBase, Mapper};

/// NROM board: no bank switching. 16 or 32 KB PRG-ROM at $8000-$FFFF (16 KB is
/// mirrored at $C000), 8 KB CHR at PPU $0000-$1FFF, hardwired mirroring. Everything
/// is handled by the default [`CartBase`] mapping.
#[derive(Clone)]
pub struct Nrom {
    /// Shared PRG/CHR/nametable state; left at its default mapping.
    base: CartBase,
}

impl Nrom {
    /// Wraps `base` without changing its power-on PRG/CHR mapping.
    pub fn new(base: CartBase) -> Self {
        Self { base }
    }
}

impl Mapper for Nrom {
    fn base(&self) -> &CartBase { &self.base }
    fn base_mut(&mut self) -> &mut CartBase { &mut self.base }
    fn name(&self) -> &'static str { "NROM (0)" }
}
