//! Address-space page-table concurrency domains.
//!
//! VMA publication and page-table mutation have different sleep rules.  The
//! domain below provides a small, architecture-neutral stripe protocol for the
//! latter: callers acquire stripes in numeric order and keep the critical
//! section limited to PTE/structure operations.  It does not own the hardware
//! page table; [`AddrSpace`](super::AddrSpace) remains the owner of the
//! materialized root.

use alloc::vec::Vec;

use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr, VirtAddrRange};

use crate::sync::{IrqMutex, IrqMutexGuard};

/// Number of fixed PTE stripes.  A power of two keeps the index operation
/// cheap while making the lock order deterministic across architectures.
pub const PTE_STRIPE_COUNT: usize = 64;

/// Lock set for PTE updates and page-table structure updates.
pub struct PageTableDomain {
    stripes: Vec<IrqMutex<()>>,
    structure: IrqMutex<()>,
}

impl PageTableDomain {
    pub fn new() -> Self {
        let mut stripes = Vec::with_capacity(PTE_STRIPE_COUNT);
        for _ in 0..PTE_STRIPE_COUNT {
            stripes.push(IrqMutex::new(()));
        }
        Self {
            stripes,
            structure: IrqMutex::new(()),
        }
    }

    /// Returns the stripe owning the base page containing `address`.
    pub fn stripe_index(&self, address: VirtAddr) -> usize {
        (address.as_usize() / PAGE_SIZE_4K) & (PTE_STRIPE_COUNT - 1)
    }

    /// Computes the ordered, de-duplicated stripes touched by a range.
    pub fn stripe_indices(&self, range: VirtAddrRange) -> Vec<usize> {
        let mut result = Vec::new();
        let mut address = range.start.align_down_4k();
        while address < range.end {
            let stripe = self.stripe_index(address);
            if !result.contains(&stripe) {
                result.push(stripe);
            }
            let Some(next) = address.checked_add(PAGE_SIZE_4K) else {
                break;
            };
            address = next;
        }
        result.sort_unstable();
        result
    }

    /// Acquires all PTE stripes touched by `range` in ascending order.
    ///
    /// The returned cursor is intentionally a capability rather than a page
    /// table reference.  It can be held while a bounded PTE operation runs, but
    /// it does not expose any API for file I/O or user memory access.
    pub fn lock_range(&self, range: VirtAddrRange) -> PteStripeCursor<'_> {
        self.lock_ranges(core::slice::from_ref(&range))
    }

    /// Acquires the union of several ranges while deduplicating and sorting
    /// stripe ids.  This is used by move/copy operations that touch source and
    /// destination ranges and must never lock the same stripe twice.
    pub fn lock_ranges(&self, ranges: &[VirtAddrRange]) -> PteStripeCursor<'_> {
        let mut indices = Vec::new();
        for range in ranges {
            for index in self.stripe_indices(*range) {
                if !indices.contains(&index) {
                    indices.push(index);
                }
            }
        }
        indices.sort_unstable();
        let mut guards = Vec::with_capacity(indices.len());
        for index in &indices {
            guards.push(self.stripes[*index].lock());
        }
        PteStripeCursor {
            range: ranges
                .first()
                .copied()
                .unwrap_or_else(|| VirtAddrRange::new(VirtAddr::from_usize(0), VirtAddr::from_usize(0))),
            indices,
            _guards: guards,
        }
    }

    /// Acquires the independent structure lock used when intermediate page
    /// table nodes are attached or detached.
    pub fn lock_structure(&self) -> StructureCursor<'_> {
        StructureCursor {
            _guard: self.structure.lock(),
        }
    }
}

impl Default for PageTableDomain {
    fn default() -> Self {
        Self::new()
    }
}

/// Proof that all PTE stripes for a range are held in lock-order.
pub struct PteStripeCursor<'a> {
    range: VirtAddrRange,
    indices: Vec<usize>,
    _guards: Vec<IrqMutexGuard<'a, ()>>,
}

impl Drop for PteStripeCursor<'_> {
    fn drop(&mut self) {
        // Every IRQ-saving guard records the state observed when that stripe
        // was acquired. `Vec` drops elements from front to back, which would
        // restore the first guard's enabled state while later guards still
        // exist and finally leave IRQs disabled. Pop in strict reverse lock
        // order so the outermost guard restores the caller's entry state last.
        while self._guards.pop().is_some() {}
    }
}

impl PteStripeCursor<'_> {
    pub fn range(&self) -> VirtAddrRange {
        self.range
    }

    pub fn stripe_indices(&self) -> &[usize] {
        &self.indices
    }
}

/// Capability for page-table intermediate-node operations.
pub struct StructureCursor<'a> {
    _guard: IrqMutexGuard<'a, ()>,
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    #[test]
    fn stripe_lock_order_is_stable_and_unique() {
        let domain = PageTableDomain::new();
        let range = VirtAddrRange::from_start_size(VirtAddr::from_usize(0x1000), 0x20_0000);
        let cursor = domain.lock_range(range);
        assert!(cursor
            .stripe_indices()
            .windows(2)
            .all(|pair| pair[0] < pair[1]));
        assert!(!cursor.stripe_indices().is_empty());
    }

    #[test]
    fn one_page_only_takes_one_stripe() {
        let domain = PageTableDomain::new();
        let range = VirtAddrRange::from_start_size(VirtAddr::from_usize(0x4000), PAGE_SIZE_4K);
        let cursor = domain.lock_range(range);
        assert_eq!(cursor.stripe_indices().len(), 1);
    }
}

#[cfg(all(test, axtest))]
mod axtests {
    use ax_runtime::hal::cpu::asm::irqs_enabled;

    use super::*;

    #[axtest::axtest]
    fn dropping_multiple_pte_stripes_restores_irq_state() {
        assert!(irqs_enabled(), "axtest must enter with local IRQs enabled");
        let domain = PageTableDomain::new();
        {
            let cursor = domain.lock_range(VirtAddrRange::from_start_size(
                VirtAddr::from_usize(0),
                PAGE_SIZE_4K * 2,
            ));
            assert_eq!(cursor.stripe_indices(), &[0, 1]);
            assert!(!irqs_enabled());
        }
        assert!(
            irqs_enabled(),
            "dropping an ordered stripe set must restore the entry IRQ state"
        );
    }
}
