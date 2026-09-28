//! Shared BAR2 backing of one ivshmem link.
//!
//! The backing owns physically contiguous host memory with a permanent
//! hypervisor virtual mapping, so guests can map the host-physical address
//! directly into their stage-2 (F6) while trap-side writes (state-table
//! publish) keep working through the virtual view. Allocation goes through
//! the [`SharedBackingAllocator`] port injected at registry creation.

use alloc::sync::Arc;
use core::ops::Range;

use ax_sync::SpinLock;

use super::error::IvshmemError;

/// Host memory behind one link's shared BAR2.
///
/// The allocation is physically contiguous and carries a permanent
/// hypervisor virtual mapping, so trap-side writes (state table publish)
/// keep working while guests map the HPA directly into stage-2.
#[derive(Clone, Copy)]
pub struct BackingAllocation {
    hpa_base: u64,
    size: u64,
    virtual_base: *mut u8,
}

impl BackingAllocation {
    /// Creates an allocation handle from its parts.
    ///
    /// Implementors of [`SharedBackingAllocator`] assemble the handle after
    /// reserving the frames; tests may use it to feed pure derivation paths.
    pub const fn from_parts(hpa_base: u64, size: u64, virtual_base: *mut u8) -> Self {
        Self {
            hpa_base,
            size,
            virtual_base,
        }
    }

    /// Returns the host-physical base of the allocation.
    pub const fn hpa_base(&self) -> u64 {
        self.hpa_base
    }

    /// Returns the allocation size in bytes.
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// Returns the hypervisor virtual base of the allocation.
    ///
    /// # Safety contract
    ///
    /// The pointer is valid, dereferenceable for `size` bytes and exclusively
    /// owned by this allocation until it is released. Aliasing is serialized
    /// by `SharedBarBacking`'s lock; no other hypervisor component may write
    /// through it.
    pub const fn virtual_base(&self) -> *mut u8 {
        self.virtual_base
    }
}

/// Host memory provider for link backing, injected at registry creation.
pub trait SharedBackingAllocator: Send + Sync {
    /// Allocates `size` bytes of physically contiguous, guest-mappable RAM
    /// with a permanent hypervisor virtual mapping. The memory is not
    /// guaranteed zeroed; callers clear it through the virtual view.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::AllocationFailed`] when the request cannot be
    /// satisfied or `size` is not 4 KiB aligned.
    fn allocate(&self, size: u64) -> Result<BackingAllocation, IvshmemError>;

    /// Releases a previously returned allocation. Passing an allocation of
    /// another allocator is a contract violation.
    fn release(&self, allocation: BackingAllocation);
}

/// Shared BAR2 backing behind every peer BAR2 of one link.
///
/// The lock only serializes single accesses; guest-visible ordering is a
/// protocol contract (write data, publish state, then ring the doorbell), not
/// an implementation detail of this lock.
pub struct SharedBarBacking {
    size: u64,
    allocation: BackingAllocation,
    allocator: Arc<dyn SharedBackingAllocator>,
    lock: SpinLock<()>,
}

// SAFETY: the allocation's virtual pointer is exclusively owned by the
// backing and every dereference happens under the backing lock; `Send` moves
// the whole backing (pointer included), `Sync` shares it behind that lock.
unsafe impl Send for SharedBarBacking {}
unsafe impl Sync for SharedBarBacking {}

impl SharedBarBacking {
    /// Allocates a zeroed backing of `size` bytes through `allocator`.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::AllocationFailed`] when the allocation fails
    /// or `size` is not 4 KiB aligned.
    pub fn try_new(
        size: u64,
        allocator: Arc<dyn SharedBackingAllocator>,
    ) -> Result<Self, IvshmemError> {
        if !size.is_multiple_of(0x1000) {
            return Err(IvshmemError::AllocationFailed {
                operation: "allocate ivshmem shared memory (size is not 4 KiB aligned)",
            });
        }
        let allocation = allocator.allocate(size)?;
        // Clear the freshly reserved frames through the permanent virtual
        // view so the shared region starts zeroed.
        // SAFETY: the allocation contract guarantees the pointer is valid and
        // dereferenceable for exactly `size` bytes; the backing exclusively
        // owns it until release.
        unsafe {
            core::ptr::write_bytes(allocation.virtual_base(), 0, size as usize);
        }
        Ok(Self {
            size,
            allocation,
            allocator,
            lock: SpinLock::new(()),
        })
    }

    /// Returns the underlying allocation (HPA and virtual view).
    pub const fn allocation(&self) -> &BackingAllocation {
        &self.allocation
    }

    /// Clears every byte of the backing through the permanent virtual view.
    ///
    /// Called when a link lifecycle ends so a later reactivation cannot
    /// observe state from the previous lifecycle.
    pub fn zero(&self) {
        // SAFETY: the virtual view is valid for `size` bytes; the exclusive
        // ownership contract keeps other components from writing it.
        unsafe {
            core::ptr::write_bytes(self.allocation.virtual_base(), 0, self.size as usize);
        }
    }

    /// Returns the backing size in bytes.
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// Reads `width` bytes at `offset` as one little-endian value.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::InvalidSharedMemoryWidth`] for widths outside
    /// `1 | 2 | 4 | 8` and [`IvshmemError::SharedMemoryOutOfRange`] when the
    /// access leaves the region.
    pub fn read(&self, offset: u64, width: usize) -> Result<u64, IvshmemError> {
        let range = self.access_range(offset, width)?;
        let value = self.copy_bytes::<false>(range, None)?;
        Ok(u64::from_le_bytes(value))
    }

    /// Writes the low `width` bytes of `value` at `offset` little-endian.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`read`](Self::read).
    pub fn write(&self, offset: u64, width: usize, value: u64) -> Result<(), IvshmemError> {
        let range = self.access_range(offset, width)?;
        self.copy_bytes::<true>(range, Some(value))?;
        Ok(())
    }

    /// Privileged byte access for link-managed regions (state table).
    ///
    /// Guests reach the same bytes only through their BAR access path; this
    /// entry point exists so the link can publish state without going
    /// through a width-encoded BAR write.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::SharedMemoryOutOfRange`] when `offset +
    /// bytes.len()` leaves the region.
    pub(crate) fn write_bytes(&self, offset: u64, bytes: &[u8]) -> Result<(), IvshmemError> {
        let range = self.byte_range(offset, bytes.len() as u64)?;
        let _lock = self.lock.lock_irqsave();
        // SAFETY: the virtual view is valid for `size` bytes and this thread
        // holds the backing lock, so the exclusive-access contract holds.
        let view = unsafe {
            core::slice::from_raw_parts_mut(self.allocation.virtual_base(), self.size as usize)
        };
        view[range].copy_from_slice(bytes);
        Ok(())
    }

    /// Copies one validated range into or out of the allocation.
    ///
    /// `WRITE` selects the direction; reads fill an eight-byte little-endian
    /// buffer with the requested bytes (the remaining bytes stay zero).
    fn copy_bytes<const WRITE: bool>(
        &self,
        range: Range<usize>,
        value: Option<u64>,
    ) -> Result<[u8; 8], IvshmemError> {
        let mut buffer = [0u8; 8];
        let _lock = self.lock.lock_irqsave();
        // SAFETY: the virtual view is valid for `size` bytes and this thread
        // holds the backing lock, so the exclusive-access contract holds.
        let view = unsafe {
            core::slice::from_raw_parts_mut(self.allocation.virtual_base(), self.size as usize)
        };
        if WRITE {
            let byte_count = range.len();
            buffer[..byte_count].copy_from_slice(&value.unwrap_or(0).to_le_bytes()[..byte_count]);
            view[range].copy_from_slice(&buffer[..byte_count]);
        } else {
            buffer[..range.len()].copy_from_slice(&view[range]);
        }
        Ok(buffer)
    }

    fn access_range(&self, offset: u64, width: usize) -> Result<Range<usize>, IvshmemError> {
        if !matches!(width, 1 | 2 | 4 | 8) {
            return Err(IvshmemError::InvalidSharedMemoryWidth { width });
        }
        self.byte_range(offset, width as u64)
    }

    fn byte_range(&self, offset: u64, len: u64) -> Result<Range<usize>, IvshmemError> {
        let out_of_range = || IvshmemError::SharedMemoryOutOfRange {
            offset,
            width: len as usize,
            size: self.size,
        };
        let end = offset.checked_add(len).ok_or_else(out_of_range)?;
        if end > self.size {
            return Err(out_of_range());
        }
        let start = usize::try_from(offset).map_err(|_| out_of_range())?;
        Ok(start..start + len as usize)
    }
}

impl Drop for SharedBarBacking {
    fn drop(&mut self) {
        // The whole link is going away: the frames return to the platform
        // allocator, and guests lose access because their stage-2 mappings
        // are torn down with the VM.
        self.allocator.release(self.allocation);
    }
}

/// Page-aligned heap allocator for in-crate tests: the returned HPA is the
/// virtual address, which pure derivation paths never dereference.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestAllocator {
    live: SpinLock<alloc::vec::Vec<usize>>,
}

#[cfg(test)]
impl SharedBackingAllocator for TestAllocator {
    fn allocate(&self, size: u64) -> Result<BackingAllocation, IvshmemError> {
        let layout = core::alloc::Layout::from_size_align(size as usize, 0x1000).map_err(|_| {
            IvshmemError::AllocationFailed {
                operation: "test allocator layout",
            }
        })?;
        let pointer = unsafe { std::alloc::alloc_zeroed(layout) };
        if pointer.is_null() {
            return Err(IvshmemError::AllocationFailed {
                operation: "test allocator alloc",
            });
        }
        self.live.lock_irqsave().push(pointer as usize);
        Ok(BackingAllocation::from_parts(
            pointer as usize as u64,
            size,
            pointer,
        ))
    }

    fn release(&self, allocation: BackingAllocation) {
        let layout =
            core::alloc::Layout::from_size_align(allocation.size() as usize, 0x1000).unwrap();
        let live = self.live.lock_irqsave();
        if !live.contains(&(allocation.hpa_base() as usize)) {
            return;
        }
        drop(live);
        // SAFETY: the pointer came from alloc_zeroed with the same layout
        // and was returned exactly once.
        unsafe { std::alloc::dealloc(allocation.virtual_base(), layout) };
        self.live
            .lock_irqsave()
            .retain(|p| *p != allocation.hpa_base() as usize);
    }
}

/// Creates one page-aligned heap test allocator.
#[cfg(test)]
pub(crate) fn test_allocator() -> Arc<dyn SharedBackingAllocator> {
    Arc::new(TestAllocator::default())
}

#[cfg(test)]
mod tests {
    use alloc::{format, sync::Arc};

    use super::*;
    use crate::ivshmem::backing::{TestAllocator, test_allocator};

    fn backing() -> SharedBarBacking {
        // The allocator contract requires page-aligned sizes because the
        // backing maps into stage-2 at page granularity.
        SharedBarBacking::try_new(0x1000, test_allocator()).unwrap()
    }

    #[test]
    fn starts_zeroed_and_reports_its_size() {
        let backing = backing();
        assert_eq!(backing.size(), 0x1000);
        assert_eq!(backing.read(0, 8).unwrap(), 0);
    }

    #[test]
    fn exposes_the_allocation_base() {
        let backing = backing();
        assert!(backing.allocation().hpa_base() != 0);
        assert_eq!(backing.allocation().size(), 0x1000);
    }

    #[test]
    fn rejects_unaligned_sizes() {
        assert!(SharedBarBacking::try_new(0x180, test_allocator()).is_err());
    }

    #[test]
    fn reads_and_writes_all_access_widths_little_endian() {
        let backing = backing();
        backing.write(0x10, 1, 0xa5).unwrap();
        backing.write(0x12, 2, 0xbeef).unwrap();
        backing.write(0x20, 4, 0xdead_beef).unwrap();
        backing.write(0x40, 8, 0x0102_0304_0506_0708).unwrap();
        assert_eq!(backing.read(0x10, 1).unwrap(), 0xa5);
        assert_eq!(backing.read(0x12, 2).unwrap(), 0xbeef);
        assert_eq!(backing.read(0x20, 4).unwrap(), 0xdead_beef);
        assert_eq!(backing.read(0x40, 8).unwrap(), 0x0102_0304_0506_0708);
        // Wider reads observe the little-endian bytes of narrower writes.
        assert_eq!(backing.read(0x10, 4).unwrap(), 0xbeef_00a5);
    }

    #[test]
    fn rejects_widths_outside_the_allowed_set() {
        let backing = backing();
        for width in [0, 3, 5, 16] {
            assert_eq!(
                backing.read(0, width),
                Err(IvshmemError::InvalidSharedMemoryWidth { width })
            );
            assert_eq!(
                backing.write(0, width, 0),
                Err(IvshmemError::InvalidSharedMemoryWidth { width })
            );
        }
    }

    #[test]
    fn rejects_accesses_beyond_the_shared_region() {
        let backing = backing();
        let expected = |offset, width| IvshmemError::SharedMemoryOutOfRange {
            offset,
            width,
            size: 0x1000,
        };
        // The last byte is still accessible; one byte further is not.
        backing.read(0xfff, 1).unwrap();
        assert_eq!(backing.read(0xfff, 2), Err(expected(0xfff, 2)));
        assert_eq!(backing.read(0x1000, 1), Err(expected(0x1000, 1)));
        assert_eq!(backing.write(0xff9, 8, 0), Err(expected(0xff9, 8)));
        // Offset arithmetic itself must not overflow.
        assert_eq!(
            backing.read(u64::MAX - 1, 8),
            Err(expected(u64::MAX - 1, 8))
        );
        assert_eq!(
            format!("{}", expected(0x1000, 1)),
            "ivshmem shared-memory access at offset 0x1000 with 1 bytes exceeds the 0x1000-byte \
             region"
        );
    }

    #[test]
    fn privileged_writes_reach_the_shared_bytes() {
        let backing = backing();
        backing
            .write_bytes(0, &0x1234_5678u32.to_le_bytes())
            .unwrap();
        assert_eq!(backing.read(0, 4).unwrap(), 0x1234_5678);
        // Out-of-range privileged writes fail without corrupting memory.
        assert!(backing.write_bytes(0xffd, &[0xaa; 4]).is_err());
        assert_eq!(backing.read(0, 4).unwrap(), 0x1234_5678);
    }

    #[test]
    fn released_allocations_return_to_the_allocator() {
        let allocator = Arc::new(TestAllocator::default());
        {
            let backing = SharedBarBacking::try_new(0x1000, allocator.clone()).unwrap();
            backing.write(0, 4, 0x1234).unwrap();
            assert_eq!(allocator.live.lock_irqsave().len(), 1);
        }
        assert_eq!(allocator.live.lock_irqsave().len(), 0);
    }
}
