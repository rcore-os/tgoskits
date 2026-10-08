//! Shared-page ownership and the guest-visible AXIVC header.

use std::sync::Arc;

use ax_memory_addr::PAGE_SIZE_4K;
use axvm_types::{GuestPhysAddr, HostPhysAddr};

use super::aperture::shared_memory_mapping_flags;
use crate::{
    AxVmError,
    guest_memory::{GuestRange, MappingLease, MemoryBacking},
    host::paging::PagingHandler,
};

/// The first two `u64` fields of an AXIVC shared region.
///
/// The type only describes the guest ABI layout. It has no accessor that
/// creates a Rust reference to the shared page, because guest and peer writes
/// may race with host access; the header is written through raw volatile stores
/// during allocation.
#[repr(C)]
pub(crate) struct IVCChannelHeader {
    pub(crate) publisher_id: u64,
    pub(crate) key: u64,
}

/// Owns contiguous host frames until every shared-page lease has been retired.
struct ContiguousFrameOwner<H: PagingHandler> {
    physical_base: HostPhysAddr,
    page_count: usize,
    _handler: core::marker::PhantomData<fn() -> H>,
}

impl<H: PagingHandler> Drop for ContiguousFrameOwner<H> {
    fn drop(&mut self) {
        H::dealloc_frames(self.physical_base, self.page_count);
    }
}

/// A real RAM backing shared by all installed endpoint mappings.
///
/// The backing retains the contiguous-frame owner through an [`Arc`]. Dropping
/// the manager's channel entry therefore cannot free pages that a peer's
/// installed mapping still holds: every [`MappingLease`] keeps its own strong
/// reference to the same backing.
pub(super) struct SharedBacking<H> {
    physical_base: HostPhysAddr,
    size: usize,
    backing: Arc<MemoryBacking>,
    _handler: core::marker::PhantomData<fn() -> H>,
}

impl<H: PagingHandler + 'static> SharedBacking<H> {
    pub(super) fn new(
        publisher_vm_id: usize,
        key: usize,
        requested_size: usize,
    ) -> Result<Self, AxVmError> {
        if requested_size == 0 {
            return Err(AxVmError::invalid_input(
                "allocate IVC shared region",
                "size must be greater than zero",
            ));
        }
        if !requested_size.is_multiple_of(PAGE_SIZE_4K) {
            return Err(AxVmError::invalid_input(
                "allocate IVC shared region",
                "size must be 4 KiB aligned",
            ));
        }

        let page_count = requested_size / PAGE_SIZE_4K;
        let physical_base =
            H::alloc_frames(page_count, PAGE_SIZE_4K).ok_or(AxVmError::OutOfMemory {
                operation: "allocate IVC shared region frames",
            })?;
        let virtual_base = H::phys_to_virt(physical_base);
        let bytes = virtual_base.as_mut_ptr();

        for offset in 0..requested_size {
            // SAFETY: the paging handler supplied `requested_size` contiguous,
            // page-aligned writable RAM bytes beginning at `virtual_base`.
            unsafe { bytes.add(offset).write_volatile(0) };
        }

        let header = bytes.cast::<IVCChannelHeader>();
        // SAFETY: the backing is page-aligned and at least one page long, so
        // both ABI header fields are in bounds and aligned. `addr_of_mut!` does
        // not create a Rust reference to the guest-visible shared page.
        unsafe {
            core::ptr::addr_of_mut!((*header).publisher_id).write_volatile(publisher_vm_id as u64);
            core::ptr::addr_of_mut!((*header).key).write_volatile(key as u64);
        }
        H::clean_dcache_range(physical_base, requested_size);

        let owner = Arc::new(ContiguousFrameOwner::<H> {
            physical_base,
            page_count,
            _handler: core::marker::PhantomData,
        });
        // SAFETY: `owner` owns the complete contiguous RAM allocation and keeps
        // it mapped until the final strong reference ends.
        let backing = unsafe { MemoryBacking::reserved(virtual_base, requested_size, owner) };

        Ok(Self {
            physical_base,
            size: requested_size,
            backing,
            _handler: core::marker::PhantomData,
        })
    }

    /// Clones one mapping lease over the shared backing for a guest endpoint.
    pub(super) fn lease(
        &self,
        guest: GuestPhysAddr,
        length: usize,
    ) -> Result<MappingLease, AxVmError> {
        let range = GuestRange::new(guest, length)?;
        MappingLease::new(
            range,
            self.physical_base,
            shared_memory_mapping_flags(),
            self.backing.clone(),
            0,
        )
    }

    pub(super) fn size(&self) -> usize {
        self.size
    }
}

#[cfg(test)]
mod tests {

    use ax_memory_addr::PAGE_SIZE_4K;
    use axvm_types::GuestPhysAddr;

    use super::{IVCChannelHeader, SharedBacking};
    use crate::host::paging::{PagingHandler, test_frames::TestFrames};

    #[test]
    fn shared_region_writes_the_guest_abi_header_and_zeroes_the_payload() {
        let backing = SharedBacking::<TestFrames>::new(3, 0x55, PAGE_SIZE_4K).unwrap();
        let bytes = TestFrames::phys_to_virt(backing.physical_base).as_ptr();

        // SAFETY: the backing owns one contiguous mapped page, so both header
        // fields are in bounds and aligned.
        let publisher_id = unsafe { (bytes as *const u64).read_volatile() };
        let key = unsafe { (bytes as *const u64).add(1).read_volatile() };
        assert_eq!(publisher_id, 3);
        assert_eq!(key, 0x55);

        for offset in core::mem::size_of::<IVCChannelHeader>()..PAGE_SIZE_4K {
            // SAFETY: `offset` stays inside the single mapped page.
            assert_eq!(unsafe { bytes.add(offset).read_volatile() }, 0);
        }
    }

    #[test]
    fn a_peer_lease_retains_the_shared_backing_owner() {
        let backing = SharedBacking::<TestFrames>::new(1, 1, PAGE_SIZE_4K).unwrap();
        let lease = backing
            .lease(GuestPhysAddr::from_usize(0x7000_0000), PAGE_SIZE_4K)
            .unwrap();

        let address = backing.physical_base;
        let identity = TestFrames::identity(address);
        drop(backing);
        assert!(
            TestFrames::is_live(address, identity),
            "a peer mapping must retain its RAM"
        );
        // SAFETY: the live allocation is retained by this mapping lease and its
        // first two aligned words contain the header written during preparation.
        let publisher = unsafe {
            TestFrames::phys_to_virt(address)
                .as_ptr()
                .cast::<u64>()
                .read_volatile()
        };
        assert_eq!(publisher, 1);
        drop(lease);
        assert!(
            !TestFrames::is_live(address, identity),
            "the final lease must release its RAM"
        );
    }
}
