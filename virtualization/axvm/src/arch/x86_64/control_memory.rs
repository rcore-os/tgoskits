//! Host allocation policy for CPU-owned VMX/SVM control-memory leases.

use core::ptr::NonNull;

use ax_cpu::{PhysAddr, virtualization::ControlMemory};
use axvm_types::{VmBackendError, VmBackendResult};

use crate::host::{HostMemory, default_host};

/// Sole owning lease of contiguous, permanently direct-mapped host pages.
pub(super) struct ControlPages {
    physical: PhysAddr,
    count: usize,
}

impl ControlPages {
    pub(super) fn allocate(count: usize) -> VmBackendResult<Self> {
        let size = count
            .checked_mul(4096)
            .filter(|&size| size != 0)
            .ok_or(VmBackendError::InvalidInput)?;
        let physical = if count == 1 {
            default_host().alloc_frame()
        } else {
            default_host().alloc_contiguous_frames(count, 4096)
        }
        .ok_or(VmBackendError::OutOfMemory)?;
        let pointer = default_host().phys_to_virt(physical).as_mut_ptr();
        // SAFETY: this allocation is exclusively owned and permanently mapped
        // by ArceOS. Initialize it before granting the CPU its control lease.
        unsafe { core::ptr::write_bytes(pointer, 0, size) };
        Ok(Self { physical, count })
    }
}

// SAFETY: construction allocates distinct coherent WB pages from ArceOS and
// initializes the complete range. This non-Clone token is their sole release
// owner; its direct map is permanent, and forgetting the token keeps the pages
// allocated. CPU retirement must return or drop the token before reclamation.
unsafe impl ControlMemory for ControlPages {
    fn physical_address(&self) -> PhysAddr {
        self.physical
    }

    fn virtual_address(&self) -> NonNull<u8> {
        NonNull::new(default_host().phys_to_virt(self.physical).as_mut_ptr())
            .expect("allocated host pages have a non-null direct-map alias")
    }

    fn byte_len(&self) -> usize {
        self.count * 4096
    }
}

impl Drop for ControlPages {
    fn drop(&mut self) {
        if self.count == 1 {
            default_host().dealloc_frame(self.physical);
        } else {
            default_host().dealloc_contiguous_frames(self.physical, self.count);
        }
    }
}
