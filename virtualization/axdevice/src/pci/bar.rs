//! 32-bit non-prefetchable memory BAR descriptors and decode state.

use core::ops::Range;

use super::{FOUR_GIB, PciBarIndex, PciError, PciResult};
use crate::ResourceRequest;

/// One 32-bit non-prefetchable memory BAR requested by a PCI function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PciMemoryBar {
    index: PciBarIndex,
    size: u64,
    address: ResourceRequest<u64>,
}

impl PciMemoryBar {
    /// Creates an automatically placed 32-bit non-prefetchable memory BAR.
    ///
    /// # Errors
    ///
    /// Returns [`PciError::InvalidBar`] if `size` is below 16 bytes, is not a
    /// power of two, or exceeds the 32-bit address space.
    pub fn new(index: PciBarIndex, size: u64) -> PciResult<Self> {
        if size < 16 || !size.is_power_of_two() {
            return Err(invalid_bar(
                index,
                "size must be a power of two of at least 16 bytes",
            ));
        }
        if size > FOUR_GIB {
            return Err(invalid_bar(index, "32-bit BAR size exceeds 4 GiB"));
        }
        Ok(Self {
            index,
            size,
            address: ResourceRequest::Auto,
        })
    }

    /// Selects automatic or fixed initial placement inside the host aperture.
    pub const fn with_address(mut self, address: ResourceRequest<u64>) -> Self {
        self.address = address;
        self
    }

    /// Returns the BAR slot.
    pub const fn index(&self) -> PciBarIndex {
        self.index
    }

    /// Returns the fixed BAR size.
    pub const fn size(&self) -> u64 {
        self.size
    }

    pub(crate) const fn address_request(&self) -> ResourceRequest<u64> {
        self.address
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedBarPlan {
    pub(crate) index: PciBarIndex,
    pub(crate) size: u64,
    pub(crate) address: u64,
}

pub(crate) struct BarState {
    plan: ResolvedBarPlan,
    address: u64,
    probing: bool,
}

impl BarState {
    pub(crate) const fn new(plan: ResolvedBarPlan) -> Self {
        Self {
            plan,
            address: plan.address,
            probing: false,
        }
    }

    pub(crate) const fn index(&self) -> PciBarIndex {
        self.plan.index
    }

    pub(crate) const fn size(&self) -> u64 {
        self.plan.size
    }

    pub(crate) fn range(&self) -> Option<Range<u64>> {
        Some(self.address..self.address.checked_add(self.plan.size)?)
    }

    pub(crate) const fn raw_dword(&self) -> u32 {
        if self.probing {
            self.size_mask()
        } else {
            self.committed_dword()
        }
    }

    pub(crate) const fn committed_dword(&self) -> u32 {
        // Aperture resolution caps addresses below 4 GiB; the cast relies on
        // that validated invariant.
        debug_assert!(self.address < FOUR_GIB);
        self.address as u32 & 0xffff_fff0
    }

    pub(crate) const fn candidate_address(dword: u32) -> u64 {
        (dword & 0xffff_fff0) as u64
    }

    pub(crate) fn set_probe(&mut self) {
        self.probing = true;
    }

    pub(crate) fn finish_relocation(&mut self, accepted: Option<u64>) {
        self.probing = false;
        if let Some(address) = accepted {
            self.address = address;
        }
    }

    pub(crate) fn reset(&mut self) {
        self.address = self.plan.address;
        self.probing = false;
    }

    const fn size_mask(&self) -> u32 {
        (!(self.plan.size - 1) as u32) & 0xffff_fff0
    }
}

fn invalid_bar(index: PciBarIndex, detail: &str) -> PciError {
    PciError::InvalidBar {
        bar: index,
        detail: detail.into(),
    }
}
