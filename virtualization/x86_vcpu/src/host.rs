//! Host callbacks required by the x86 vCPU implementation.

use ax_errno::{AxResult, ax_err_type};
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, VirtAddr};

/// Host memory and time operations required by x86 virtualization backends.
#[ax_crate_interface::def_interface]
pub trait X86VcpuHostIf {
    /// Allocate one host frame.
    fn alloc_frame() -> Option<PhysAddr>;

    /// Deallocate one host frame.
    fn dealloc_frame(paddr: PhysAddr);

    /// Allocate contiguous host frames.
    fn alloc_contiguous_frames(frame_count: usize, frame_align: usize) -> Option<PhysAddr>;

    /// Deallocate contiguous host frames.
    fn dealloc_contiguous_frames(start_paddr: PhysAddr, frame_count: usize);

    /// Convert host physical address to host virtual address.
    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr;

    /// Convert nanoseconds to host ticks.
    fn nanos_to_ticks(nanos: u64) -> u64;
}

/// RAII host frame used by x86 VMX/SVM structures.
#[derive(Debug)]
pub struct PhysFrame {
    start_paddr: Option<PhysAddr>,
}

impl PhysFrame {
    /// Allocate a host frame.
    pub fn alloc() -> AxResult<Self> {
        let start_paddr = ax_crate_interface::call_interface!(X86VcpuHostIf::alloc_frame())
            .ok_or_else(|| ax_err_type!(NoMemory, "allocate physical frame failed"))?;
        assert_ne!(start_paddr.as_usize(), 0);
        Ok(Self {
            start_paddr: Some(start_paddr),
        })
    }

    /// Allocate a host frame and fill it with zeros.
    pub fn alloc_zero() -> AxResult<Self> {
        let mut frame = Self::alloc()?;
        frame.fill(0);
        Ok(frame)
    }

    /// Create an uninitialized frame placeholder.
    ///
    /// # Safety
    ///
    /// The caller must ensure the placeholder is replaced before being accessed.
    pub const unsafe fn uninit() -> Self {
        Self { start_paddr: None }
    }

    /// Get the starting physical address of the frame.
    pub fn start_paddr(&self) -> PhysAddr {
        self.start_paddr.expect("uninitialized PhysFrame")
    }

    /// Get a mutable pointer to the frame.
    pub fn as_mut_ptr(&self) -> *mut u8 {
        ax_crate_interface::call_interface!(X86VcpuHostIf::phys_to_virt(self.start_paddr()))
            .as_mut_ptr()
    }

    /// Fill the frame with a byte.
    pub fn fill(&mut self, byte: u8) {
        unsafe { core::ptr::write_bytes(self.as_mut_ptr(), byte, PAGE_SIZE_4K) };
    }
}

impl Drop for PhysFrame {
    fn drop(&mut self) {
        if let Some(start_paddr) = self.start_paddr {
            ax_crate_interface::call_interface!(X86VcpuHostIf::dealloc_frame(start_paddr));
            log::debug!("[x86_vcpu] deallocated PhysFrame({start_paddr:#x})");
        }
    }
}

#[cfg(feature = "svm")]
pub(crate) fn alloc_contiguous_frames(frame_count: usize, frame_align: usize) -> Option<PhysAddr> {
    ax_crate_interface::call_interface!(X86VcpuHostIf::alloc_contiguous_frames(
        frame_count,
        frame_align
    ))
}

#[cfg(feature = "svm")]
pub(crate) fn dealloc_contiguous_frames(start_paddr: PhysAddr, frame_count: usize) {
    ax_crate_interface::call_interface!(X86VcpuHostIf::dealloc_contiguous_frames(
        start_paddr,
        frame_count
    ));
}

#[cfg(any(feature = "vmx", feature = "svm"))]
pub(crate) fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
    ax_crate_interface::call_interface!(X86VcpuHostIf::phys_to_virt(paddr))
}

#[cfg(any(feature = "vmx", feature = "svm"))]
pub(crate) fn nanos_to_ticks(nanos: u64) -> u64 {
    ax_crate_interface::call_interface!(X86VcpuHostIf::nanos_to_ticks(nanos))
}
