//! Immutable backing for base-page anonymous reads.

use alloc::sync::Arc;

use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, VirtAddr};
use ax_runtime::hal::mem::virt_to_phys;

use super::super::objects::{FrameLease, PageId, PageObject};
use crate::{StarryError, StarryResult};

#[repr(C, align(4096))]
struct ZeroBytes([u8; PAGE_SIZE_4K]);

// The image owns this read-only RAM for its entire lifetime. It must never
// reach either a writable user PTE or the frame allocator's release path.
static ZERO_BYTES: ZeroBytes = ZeroBytes([0; PAGE_SIZE_4K]);

pub(super) struct ZeroPage;

impl ZeroPage {
    fn address() -> PhysAddr {
        virt_to_phys(VirtAddr::from_usize(ZERO_BYTES.0.as_ptr() as usize))
    }

    pub(super) fn owns(page: &PageObject) -> bool {
        page.frame().size() == PAGE_SIZE_4K && page.frame().paddr() == Self::address()
    }

    pub(super) fn object() -> StarryResult<Arc<PageObject>> {
        let lease = FrameLease::borrowed(Self::address(), PAGE_SIZE_4K, None)
            .ok_or(StarryError::BadState)?;
        Ok(PageObject::new_present(PageId::allocate(), lease))
    }
}
