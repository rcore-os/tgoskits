use core::marker::PhantomData;

use crate::{
    X86HostOps, X86HostPhysAddr, X86VcpuError, X86VcpuResult, host,
    types::X86_PAGE_SIZE_4K as PAGE_SIZE,
};

/// Contiguous physical frames for SVM structures such as IOPM and MSRPM.
#[derive(Debug)]
pub struct ContiguousPhysFrames<H: X86HostOps> {
    start_paddr: Option<X86HostPhysAddr>,
    frame_count: usize,
    _host: PhantomData<fn() -> H>,
}

impl<H: X86HostOps> ContiguousPhysFrames<H> {
    pub fn alloc(frame_count: usize) -> X86VcpuResult<Self> {
        let start_paddr = host::alloc_contiguous_frames::<H>(frame_count, PAGE_SIZE)
            .ok_or(X86VcpuError::NoMemory)?;

        assert_ne!(start_paddr.as_usize(), 0);
        Ok(Self {
            start_paddr: Some(start_paddr),
            frame_count,
            _host: PhantomData,
        })
    }

    pub fn alloc_zero(frame_count: usize) -> X86VcpuResult<Self> {
        let mut frames = Self::alloc(frame_count)?;
        frames.fill(0);
        Ok(frames)
    }

    pub fn start_paddr(&self) -> X86HostPhysAddr {
        self.start_paddr
            .expect("uninitialized ContiguousPhysFrames")
    }

    pub fn size(&self) -> usize {
        PAGE_SIZE * self.frame_count
    }

    pub fn as_mut_ptr(&self) -> *mut u8 {
        <H as X86HostOps>::phys_to_virt(self.start_paddr()).as_mut_ptr()
    }

    pub fn fill(&mut self, byte: u8) {
        unsafe {
            core::ptr::write_bytes(self.as_mut_ptr(), byte, self.size());
        }
    }
}

impl<H: X86HostOps> Drop for ContiguousPhysFrames<H> {
    fn drop(&mut self) {
        if let Some(start_paddr) = self.start_paddr {
            host::dealloc_contiguous_frames::<H>(start_paddr, self.frame_count);
            debug!(
                "[AxVM] deallocated ContiguousPhysFrames({:#x}, {} frames)",
                start_paddr, self.frame_count
            );
        }
    }
}
