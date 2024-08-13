//! Address Space management for AxVM.
//! Todo: reuse code from arceos under monolithickernel-new branch
//! 	* module: https://github.com/arceos-org/arceos/tree/monolithickernel-new/modules/axmm
//! 	* crate: https://github.com/arceos-org/arceos/tree/monolithickernel-new/crates/memory_set

use core::marker::PhantomData;

use axerrno::{ax_err_type, AxResult};
use memory_addr::PhysAddr;
use page_table_entry::MappingFlags;
use page_table_multiarch::PagingHandler;

use axaddrspace::{GuestPhysAddr, HostPhysAddr};

use crate::AxVMHal;

pub(crate) use memory_addr::PAGE_SIZE_4K as PAGE_SIZE;

/// Information about nested page faults.
#[derive(Debug)]
pub struct NestedPageFaultInfo {
    /// Access type that caused the nested page fault.
    pub access_flags: MappingFlags,
    /// Guest physical address that caused the nested page fault.
    pub fault_guest_paddr: GuestPhysAddr,
}

/// A 4K-sized contiguous physical memory page, it will deallocate the page
/// automatically on drop.
#[derive(Debug)]
pub struct PhysFrame<H: AxVMHal> {
    start_paddr: HostPhysAddr,
    _phantom: PhantomData<H>,
}

impl<H: AxVMHal> PhysFrame<H> {
    pub fn alloc() -> AxResult<Self> {
        let start_paddr = H::PagingHandler::alloc_frame()
            .ok_or_else(|| ax_err_type!(NoMemory, "allocate physical frame failed"))?;
        assert_ne!(start_paddr.as_usize(), 0);
        debug!("[AxVM] allocated PhysFrame({:#x})", start_paddr);
        Ok(Self {
            start_paddr,
            _phantom: PhantomData,
        })
    }

    pub fn alloc_zero() -> AxResult<Self> {
        let mut f = Self::alloc()?;
        f.fill(0);
        Ok(f)
    }

    pub const unsafe fn uninit() -> Self {
        Self {
            start_paddr: PhysAddr::from(0),
            _phantom: PhantomData,
        }
    }

    pub fn start_paddr(&self) -> HostPhysAddr {
        self.start_paddr
    }

    pub fn as_mut_ptr(&self) -> *mut u8 {
        H::PagingHandler::phys_to_virt(self.start_paddr).as_mut_ptr()
    }

    pub fn fill(&mut self, byte: u8) {
        unsafe { core::ptr::write_bytes(self.as_mut_ptr(), byte, PAGE_SIZE) }
    }
}

impl<H: AxVMHal> Drop for PhysFrame<H> {
    fn drop(&mut self) {
        if self.start_paddr.as_usize() > 0 {
            H::PagingHandler::dealloc_frame(self.start_paddr);
            debug!("[AxVM] deallocated PhysFrame({:#x})", self.start_paddr);
        }
    }
}
