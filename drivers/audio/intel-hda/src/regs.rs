//! HDA 1.0a offsets. Access width is part of the register contract.
pub const GCAP: usize = 0;
pub const GCTL: usize = 8;
pub const STATESTS: usize = 0x0e;
pub const CORB: usize = 0x40;
pub const CORBWP: usize = 0x48;
pub const CORBRP: usize = 0x4a;
pub const CORBCTL: usize = 0x4c;
pub const CORBSIZE: usize = 0x4e;
pub const RIRB: usize = 0x50;
pub const RIRBWP: usize = 0x58;
pub const RINTCNT: usize = 0x5a;
pub const RIRBCTL: usize = 0x5c;
pub const RIRBSTS: usize = 0x5d;
pub const RIRBSIZE: usize = 0x5e;
