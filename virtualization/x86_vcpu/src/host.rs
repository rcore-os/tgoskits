//! Host callbacks required by the OS-neutral x86 vCPU implementation.

use core::marker::PhantomData;

use x86_vlapic::X86VlapicHostOps;

use crate::{
    X86GuestPhysAddr, X86HostPhysAddr, X86HostVirtAddr, X86VcpuError, X86VcpuResult,
    types::X86_PAGE_SIZE_4K,
};

/// Host memory, time, and interrupt operations required by x86 virtualization backends.
pub trait X86HostOps: X86VlapicHostOps {
    /// Allocate one host frame.
    fn alloc_frame() -> Option<X86HostPhysAddr>;

    /// Deallocate one host frame.
    fn dealloc_frame(paddr: X86HostPhysAddr);

    /// Allocate contiguous host frames.
    fn alloc_contiguous_frames(frame_count: usize, frame_align: usize) -> Option<X86HostPhysAddr>;

    /// Deallocate contiguous host frames.
    fn dealloc_contiguous_frames(start_paddr: X86HostPhysAddr, frame_count: usize);

    /// Convert host physical address to host virtual address.
    fn phys_to_virt(paddr: X86HostPhysAddr) -> X86HostVirtAddr;

    /// Read one byte from guest physical memory.
    fn read_guest_u8(paddr: X86GuestPhysAddr) -> X86VcpuResult<u8>;

    /// Convert nanoseconds to host ticks.
    fn nanos_to_ticks(nanos: u64) -> u64;

    /// Poll the host interrupt controller for a pending vector.
    fn poll_host_interrupt() -> Option<u8>;
}

/// RAII host frame used by x86 VMX/SVM structures.
#[derive(Debug)]
pub struct PhysFrame<H: X86HostOps> {
    start_paddr: Option<X86HostPhysAddr>,
    _host: PhantomData<fn() -> H>,
}

impl<H: X86HostOps> PhysFrame<H> {
    /// Allocate a host frame.
    pub fn alloc() -> X86VcpuResult<Self> {
        let start_paddr = <H as X86HostOps>::alloc_frame().ok_or(X86VcpuError::NoMemory)?;
        assert_ne!(start_paddr.as_usize(), 0);
        Ok(Self {
            start_paddr: Some(start_paddr),
            _host: PhantomData,
        })
    }

    /// Allocate a host frame and fill it with zeros.
    pub fn alloc_zero() -> X86VcpuResult<Self> {
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
        Self {
            start_paddr: None,
            _host: PhantomData,
        }
    }

    /// Get the starting physical address of the frame.
    pub fn start_paddr(&self) -> X86HostPhysAddr {
        self.start_paddr.expect("uninitialized PhysFrame")
    }

    /// Get a mutable pointer to the frame.
    pub fn as_mut_ptr(&self) -> *mut u8 {
        <H as X86HostOps>::phys_to_virt(self.start_paddr()).as_mut_ptr()
    }

    /// Fill the frame with a byte.
    pub fn fill(&mut self, byte: u8) {
        unsafe { core::ptr::write_bytes(self.as_mut_ptr(), byte, X86_PAGE_SIZE_4K) };
    }
}

impl<H: X86HostOps> Drop for PhysFrame<H> {
    fn drop(&mut self) {
        if let Some(start_paddr) = self.start_paddr {
            <H as X86HostOps>::dealloc_frame(start_paddr);
            log::debug!("[x86_vcpu] deallocated PhysFrame({start_paddr:#x})");
        }
    }
}

#[cfg(feature = "svm")]
pub(crate) fn alloc_contiguous_frames<H: X86HostOps>(
    frame_count: usize,
    frame_align: usize,
) -> Option<X86HostPhysAddr> {
    <H as X86HostOps>::alloc_contiguous_frames(frame_count, frame_align)
}

#[cfg(feature = "svm")]
pub(crate) fn dealloc_contiguous_frames<H: X86HostOps>(
    start_paddr: X86HostPhysAddr,
    frame_count: usize,
) {
    <H as X86HostOps>::dealloc_contiguous_frames(start_paddr, frame_count);
}

#[cfg(any(feature = "vmx", feature = "svm"))]
pub(crate) fn read_guest_u8<H: X86HostOps>(paddr: X86GuestPhysAddr) -> X86VcpuResult<u8> {
    H::read_guest_u8(paddr)
}

#[cfg(any(feature = "vmx", feature = "svm"))]
pub(crate) fn nanos_to_ticks<H: X86HostOps>(nanos: u64) -> u64 {
    H::nanos_to_ticks(nanos)
}
