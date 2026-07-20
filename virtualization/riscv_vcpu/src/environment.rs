//! Guest-visible RISC-V ISA execution environment.

use riscv_h::register::henvcfg::{CacheBlockInvalidate, Henvcfg};

const ZICBOM: u8 = 1 << 0;
const ZICBOZ: u8 = 1 << 1;

/// ISA extensions whose execution requires hypervisor environment controls.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RiscvGuestIsaConfig {
    extensions: u8,
}

impl RiscvGuestIsaConfig {
    /// Creates the baseline guest ISA environment without optional CBO access.
    pub const fn baseline() -> Self {
        Self { extensions: 0 }
    }

    /// Creates the environment used when the guest inherits the host CPU ISA.
    pub const fn inherited_host() -> Self {
        Self {
            extensions: ZICBOM | ZICBOZ,
        }
    }

    /// Adds the Zicbom cache-block management extension.
    pub const fn with_zicbom(mut self) -> Self {
        self.extensions |= ZICBOM;
        self
    }

    /// Adds the Zicboz cache-block zero extension.
    pub const fn with_zicboz(mut self) -> Self {
        self.extensions |= ZICBOZ;
        self
    }

    pub(crate) fn henvcfg(self) -> Henvcfg {
        let mut config = Henvcfg::from_bits(0);
        if self.extensions & ZICBOM != 0 {
            config.set_cache_block_invalidate(CacheBlockInvalidate::Invalidate);
            config.set_cache_block_clean_flush(true);
        }
        if self.extensions & ZICBOZ != 0 {
            config.set_cache_block_zero(true);
        }
        config
    }
}
