use core::marker::PhantomData;

use axerrno::{AxResult, ax_err_type};

pub(crate) use memory_addr::PAGE_SIZE_4K as PAGE_SIZE;

use crate::{AxMmHal, HostPhysAddr};

/// A physical frame which will be automatically deallocated when dropped.
///
/// The frame is allocated using the [`AxMmHal`] implementation. The size of the frame is likely to
/// be 4 KiB but the actual size is determined by the [`AxMmHal`] implementation.
#[derive(Debug)]
pub struct PhysFrame<H: AxMmHal> {
    start_paddr: Option<HostPhysAddr>,
    _marker: PhantomData<H>,
}

impl<H: AxMmHal> PhysFrame<H> {
    /// Allocate a [`PhysFrame`].
    pub fn alloc() -> AxResult<Self> {
        let start_paddr = H::alloc_frame()
            .ok_or_else(|| ax_err_type!(NoMemory, "allocate physical frame failed"))?;
        assert_ne!(start_paddr.as_usize(), 0);
        Ok(Self {
            start_paddr: Some(start_paddr),
            _marker: PhantomData,
        })
    }

    /// Allocate a [`PhysFrame`] and fill it with zeros.
    pub fn alloc_zero() -> AxResult<Self> {
        let mut f = Self::alloc()?;
        f.fill(0);
        Ok(f)
    }

    /// Create an uninitialized [`PhysFrame`].
    ///
    /// # Safety
    ///
    /// The caller must ensure that the [`PhysFrame`] is only used as a placeholder and never
    /// accessed.
    pub const unsafe fn uninit() -> Self {
        Self {
            start_paddr: None,
            _marker: PhantomData,
        }
    }

    /// Get the starting physical address of the frame.
    pub fn start_paddr(&self) -> HostPhysAddr {
        self.start_paddr.expect("uninitialized PhysFrame")
    }

    /// Get a mutable pointer to the frame.
    pub fn as_mut_ptr(&self) -> *mut u8 {
        H::phys_to_virt(self.start_paddr()).as_mut_ptr()
    }

    /// Fill the frame with a byte. Works only when the frame is 4 KiB in size.
    pub fn fill(&mut self, byte: u8) {
        unsafe { core::ptr::write_bytes(self.as_mut_ptr(), byte, PAGE_SIZE) }
    }
}

impl<H: AxMmHal> Drop for PhysFrame<H> {
    fn drop(&mut self) {
        if let Some(start_paddr) = self.start_paddr {
            H::dealloc_frame(start_paddr);
            debug!("[AxVM] deallocated PhysFrame({:#x})", start_paddr);
        }
    }
}
