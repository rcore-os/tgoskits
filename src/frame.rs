use axaddrspace::HostPhysAddr;
use axerrno::{ax_err_type, AxResult};
use memory_addr::{PhysAddr, VirtAddr};

pub(crate) use memory_addr::PAGE_SIZE_4K as PAGE_SIZE;

/// Low-level resource interfaces that must be implemented by the crate user.
#[crate_interface::def_interface]
pub trait PhysFrameIf {
    /// Request to allocate a 4K-sized physical frame.
    fn alloc_frame() -> Option<PhysAddr>;
    /// Request to free a allocated physical frame.
    fn dealloc_frame(paddr: PhysAddr);
    /// Returns a virtual address that maps to the given physical address.
    ///
    /// Used to access the physical memory directly in PhysFrame implementation through `as_mut_ptr()`.
    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr;
}

/// A 4K-sized contiguous physical memory page, it will deallocate the page
/// automatically on drop.
#[derive(Debug)]
pub struct PhysFrame {
    start_paddr: Option<HostPhysAddr>,
}

impl PhysFrame {
    pub fn alloc() -> AxResult<Self> {
        let start_paddr = crate_interface::call_interface!(PhysFrameIf::alloc_frame)
            .ok_or_else(|| ax_err_type!(NoMemory, "allocate physical frame failed"))?;
        assert_ne!(start_paddr.as_usize(), 0);
        debug!("[AxVM] allocated PhysFrame({:#x})", start_paddr);
        Ok(Self {
            start_paddr: Some(start_paddr),
        })
    }

    pub fn alloc_zero() -> AxResult<Self> {
        let mut f = Self::alloc()?;
        f.fill(0);
        Ok(f)
    }

    pub const unsafe fn uninit() -> Self {
        Self { start_paddr: None }
    }

    pub fn start_paddr(&self) -> HostPhysAddr {
        self.start_paddr.expect("uninitialized PhysFrame")
    }

    pub fn as_mut_ptr(&self) -> *mut u8 {
        crate_interface::call_interface!(PhysFrameIf::phys_to_virt(self.start_paddr())).as_mut_ptr()
    }

    pub fn fill(&mut self, byte: u8) {
        unsafe { core::ptr::write_bytes(self.as_mut_ptr(), byte, PAGE_SIZE) }
    }
}

impl Drop for PhysFrame {
    fn drop(&mut self) {
        if let Some(start_paddr) = self.start_paddr {
            crate_interface::call_interface!(PhysFrameIf::dealloc_frame(start_paddr));
            debug!("[AxVM] deallocated PhysFrame({:#x})", start_paddr);
        }
    }
}
