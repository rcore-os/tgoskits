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
            debug!("[AxVM] deallocated PhysFrame({start_paddr:#x})");
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::test_utils::{BASE_PADDR, MockHal, mock_hal_test, test_dealloc_count};
    use alloc::vec::Vec;
    use assert_matches::assert_matches;
    use axin::axin;

    #[test]
    #[axin(decorator(mock_hal_test), on_exit(test_dealloc_count(1)))]
    fn test_alloc_dealloc_cycle() {
        let frame = PhysFrame::<MockHal>::alloc()
            .unwrap_or_else(|e| panic!("Failed to allocate frame: {:?}", e));
        assert_eq!(frame.start_paddr().as_usize(), BASE_PADDR);
        // frame is dropped here, dealloc_frame should be called
    }

    #[test]
    #[axin(decorator(mock_hal_test), on_exit(test_dealloc_count(1)))]
    fn test_alloc_zero() {
        let frame = PhysFrame::<MockHal>::alloc_zero()
            .unwrap_or_else(|e| panic!("Failed to allocate zero frame: {:?}", e));
        assert_eq!(frame.start_paddr().as_usize(), BASE_PADDR);
        let ptr = frame.as_mut_ptr();
        let page = unsafe { &*(ptr as *const [u8; PAGE_SIZE]) };
        assert!(page.iter().all(|&x| x == 0));
    }

    #[test]
    #[axin(decorator(mock_hal_test), on_exit(test_dealloc_count(1)))]
    fn test_fill_operation() {
        let mut frame = PhysFrame::<MockHal>::alloc()
            .unwrap_or_else(|e| panic!("Failed to allocate frame: {:?}", e));
        assert_eq!(frame.start_paddr().as_usize(), BASE_PADDR);
        frame.fill(0xAA);
        let ptr = frame.as_mut_ptr();
        let page = unsafe { &*(ptr as *const [u8; PAGE_SIZE]) };
        assert!(page.iter().all(|&x| x == 0xAA));
    }

    #[test]
    #[axin(decorator(mock_hal_test), on_exit(test_dealloc_count(5)))]
    fn test_fill_multiple_frames() {
        const NUM_FRAMES: usize = 5;

        let mut frames = Vec::new();
        let mut patterns = Vec::new();

        for i in 0..NUM_FRAMES {
            let mut frame = PhysFrame::<MockHal>::alloc().unwrap();
            let pattern = (0xA0 + i) as u8;
            frame.fill(pattern);
            frames.push(frame);
            patterns.push(pattern);
        }

        for i in 0..NUM_FRAMES {
            let actual_page = unsafe { &*(frames[i].as_mut_ptr() as *mut [u8; PAGE_SIZE]) };
            let expected_page = &[patterns[i]; PAGE_SIZE];

            assert_eq!(
                actual_page, expected_page,
                "Frame verification failed for frame index {i}: Expected pattern 0x{:02x}",
                patterns[i]
            );
        }
    }

    #[test]
    #[should_panic(expected = "uninitialized PhysFrame")]
    fn test_uninit_access() {
        // This test verifies that accessing an uninitialized PhysFrame (created with `unsafe { uninit() }`)
        // leads to a panic when trying to retrieve its physical address.
        let frame = unsafe { PhysFrame::<MockHal>::uninit() };
        frame.start_paddr(); // This should panic
    }

    #[test]
    #[axin(decorator(mock_hal_test), on_exit(test_dealloc_count(0)))]
    fn test_alloc_no_memory() {
        // Configure MockHal to simulate an allocation failure.
        MockHal::set_alloc_fail(true);
        let result = PhysFrame::<MockHal>::alloc();
        // Assert that allocation failed and verify the specific error type.
        assert_matches!(result, Err(axerrno::AxError::NoMemory));
        MockHal::set_alloc_fail(false); // Reset for other tests
    }
}
