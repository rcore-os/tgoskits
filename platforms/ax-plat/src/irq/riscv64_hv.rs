//! RISC-V hypervisor PLIC route-revocation extension.

use super::IrqError;

/// ABI version of [`RiscvPlicGuestRouteV1`].
pub const RISCV_PLIC_GUEST_ROUTE_ABI_V1: u16 = 1;
/// Number of PLIC sources represented by the fixed route bitmap.
pub const RISCV_PLIC_GUEST_SOURCE_COUNT: usize = 1024;
/// Number of words in the fixed route bitmap.
pub const RISCV_PLIC_GUEST_SOURCE_WORDS: usize =
    RISCV_PLIC_GUEST_SOURCE_COUNT.div_ceil(u64::BITS as usize);
/// Maximum work performed by one route prepare or revocation transaction.
pub const RISCV_PLIC_GUEST_ROUTE_MAX_SOURCES: usize = 64;

/// Value-only canonical identity for one guest-owned PLIC route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct RiscvPlicGuestRouteV1 {
    abi_version: u16,
    reserved: u16,
    target_cpu: u32,
    source_words: [u64; RISCV_PLIC_GUEST_SOURCE_WORDS],
}

impl RiscvPlicGuestRouteV1 {
    /// Builds a canonical route identity from physical PLIC source IDs.
    ///
    /// # Errors
    ///
    /// Returns [`IrqError::InvalidCpu`] when `target_cpu` does not fit the
    /// value ABI, and [`IrqError::InvalidIrq`] for source zero, an out-of-range
    /// source, or a duplicate source.
    pub fn new(target_cpu: usize, sources: &[u32]) -> Result<Self, IrqError> {
        let target_cpu = u32::try_from(target_cpu).map_err(|_| IrqError::InvalidCpu)?;
        if sources.is_empty() || sources.len() > RISCV_PLIC_GUEST_ROUTE_MAX_SOURCES {
            return Err(IrqError::InvalidIrq);
        }
        let mut source_words = [0; RISCV_PLIC_GUEST_SOURCE_WORDS];
        for &source in sources {
            let source = source as usize;
            if source == 0 || source >= RISCV_PLIC_GUEST_SOURCE_COUNT {
                return Err(IrqError::InvalidIrq);
            }
            let word = &mut source_words[source / u64::BITS as usize];
            let bit = 1 << (source % u64::BITS as usize);
            if *word & bit != 0 {
                return Err(IrqError::InvalidIrq);
            }
            *word |= bit;
        }
        Ok(Self {
            abi_version: RISCV_PLIC_GUEST_ROUTE_ABI_V1,
            reserved: 0,
            target_cpu,
            source_words,
        })
    }

    /// Returns whether the version and reserved fields match this ABI.
    pub const fn is_valid_v1(self) -> bool {
        self.abi_version == RISCV_PLIC_GUEST_ROUTE_ABI_V1 && self.reserved == 0
    }

    /// Returns the fixed logical CPU that owns the physical route.
    pub const fn target_cpu(self) -> usize {
        self.target_cpu as usize
    }

    /// Returns the canonical physical-source bitmap.
    pub const fn source_words(&self) -> &[u64; RISCV_PLIC_GUEST_SOURCE_WORDS] {
        &self.source_words
    }

    /// Returns whether one canonical source belongs to this route.
    pub const fn contains_source(&self, source: usize) -> bool {
        if source == 0 || source >= RISCV_PLIC_GUEST_SOURCE_COUNT {
            return false;
        }
        self.source_words[source / u64::BITS as usize] & (1 << (source % u64::BITS as usize)) != 0
    }
}

/// Generation-bearing capability for one fail-closed route revocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct RiscvPlicRouteRevocation(u64);

impl RiscvPlicRouteRevocation {
    /// Reconstructs a nonzero platform revocation generation.
    pub const fn try_new(generation: u64) -> Option<Self> {
        if generation == 0 {
            None
        } else {
            Some(Self(generation))
        }
    }

    /// Returns the platform route generation.
    pub const fn generation(self) -> u64 {
        self.0
    }
}

/// Progress of one bounded route-revocation poll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RiscvPlicRouteRevocationProgress {
    /// A publisher, endpoint reader, or controller lock still needs service.
    Pending  = 0,
    /// Every source lease was released and the platform route is vacant.
    Released = 1,
}

/// RISC-V hypervisor PLIC route-revocation extension.
#[def_plat_interface]
pub trait Riscv64HvIrqIf {
    /// Blocks new guest publication and masks every source in one active route.
    fn begin_guest_irq_route_revocation(
        route: RiscvPlicGuestRouteV1,
    ) -> Result<RiscvPlicRouteRevocation, IrqError>;

    /// Performs one bounded drain/release attempt without sleeping or spinning.
    fn poll_guest_irq_route_revocation(
        revocation: RiscvPlicRouteRevocation,
    ) -> Result<RiscvPlicRouteRevocationProgress, IrqError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_route_rejects_reserved_duplicate_and_out_of_range_sources() {
        assert_eq!(
            RiscvPlicGuestRouteV1::new(2, &[7, 65])
                .unwrap()
                .target_cpu(),
            2
        );
        assert_eq!(
            RiscvPlicGuestRouteV1::new(2, &[0]),
            Err(IrqError::InvalidIrq)
        );
        assert_eq!(
            RiscvPlicGuestRouteV1::new(2, &[7, 7]),
            Err(IrqError::InvalidIrq)
        );
        assert_eq!(
            RiscvPlicGuestRouteV1::new(2, &[RISCV_PLIC_GUEST_SOURCE_COUNT as u32]),
            Err(IrqError::InvalidIrq)
        );
    }
}
