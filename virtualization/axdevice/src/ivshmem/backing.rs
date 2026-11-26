//! Shared BAR2 backing of one ivshmem link.
//!
//! The backing owns physically contiguous host memory with a permanent
//! hypervisor virtual mapping, so guests can map the host-physical address
//! directly into their stage-2 (F6) while trap-side writes (state-table
//! publish) keep working through the virtual view. Allocation goes through
//! the [`SharedBackingAllocator`] port injected at registry creation.

use alloc::sync::Arc;
#[cfg(test)]
use core::sync::atomic::AtomicBool;
use core::{
    mem::{align_of, size_of},
    ops::Range,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use ax_sync::RawSpinLock;

use super::error::IvshmemError;

/// Host memory behind one link's shared BAR2.
///
/// The allocation is physically contiguous and carries a permanent
/// hypervisor virtual mapping, so trap-side writes (state table publish)
/// keep working while guests map the HPA directly into stage-2.
pub struct BackingAllocation {
    hpa_base: u64,
    size: u64,
    virtual_base: *mut u8,
}

impl BackingAllocation {
    /// Creates a move-only allocation handle from its parts.
    ///
    /// Implementors of [`SharedBackingAllocator`] assemble the handle after
    /// reserving the frames.
    ///
    /// # Safety
    ///
    /// `virtual_base` must be a non-null, writable, permanent hypervisor
    /// mapping of `size` physically contiguous, 4 KiB aligned, guest-mappable
    /// bytes. `hpa_base` must be the host-physical base of that exact
    /// allocation. The caller must transfer the only ownership of those frames
    /// to the returned value; no other owner may free or remap them.
    pub unsafe fn from_raw_parts(hpa_base: u64, size: u64, virtual_base: *mut u8) -> Self {
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

    /// Returns the hypervisor virtual base owned by this token.
    ///
    /// The pointer remains valid until the token is passed to its allocator's
    /// [`SharedBackingAllocator::release`](crate::SharedBackingAllocator::release).
    /// Dereferencing it requires the caller to uphold the token's allocation
    /// range and exclusive ownership.
    pub const fn virtual_base(&self) -> *mut u8 {
        self.virtual_base
    }
}

/// Host memory provider for link backing, injected at registry creation.
///
/// # Safety
///
/// Every returned [`BackingAllocation`] must own a unique physically
/// contiguous RAM allocation that is 4 KiB aligned, guest-mappable, and
/// backed by a permanent writable hypervisor virtual mapping for exactly
/// `size` bytes. The allocation must remain live until [`Self::release`] is
/// called with that exact token after all host and guest accesses have ceased.
/// Implementations must not return aliases of the same allocation.
pub unsafe trait SharedBackingAllocator: Send + Sync {
    /// Allocates `size` bytes of physically contiguous, guest-mappable RAM
    /// with a permanent hypervisor virtual mapping. The memory is not
    /// guaranteed zeroed; callers clear it through the virtual view.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::AllocationFailed`] when the request cannot be
    /// satisfied or `size` is not 4 KiB aligned.
    fn allocate(&self, size: u64) -> Result<BackingAllocation, IvshmemError>;

    /// Releases a previously returned allocation.
    ///
    /// # Safety
    ///
    /// `allocation` must be the exact move-only token returned by this
    /// allocator, must still be live, and must not have any outstanding host
    /// or guest access. This method consumes the token and must be called at
    /// most once for it.
    unsafe fn release(&self, allocation: BackingAllocation);
}

/// Shared BAR2 backing behind every peer BAR2 of one link.
///
/// The lock only serializes single accesses; guest-visible ordering is a
/// protocol contract (write data, publish state, then ring the doorbell), not
/// an implementation detail of this lock.
pub struct SharedBarBacking {
    size: u64,
    allocation: Option<BackingAllocation>,
    allocator: Arc<dyn SharedBackingAllocator>,
    /// Serializes hypervisor-side multi-byte accesses. Guest accesses are
    /// outside this lock and follow the section/protocol ownership rules.
    lock: RawSpinLock<()>,
}

// SAFETY: the move-only allocation token is the unique owner of the mapped
// frames. No method creates a Rust reference into the shared memory: exact
// ranges are accessed through raw volatile pointers, and host-owned words are
// atomic. The host lock only orders hypervisor accesses; guest direct accesses
// are external MMU accesses to the section-mapped ranges, so no Rust `&mut`
// alias is claimed over them. Teardown revokes direct mappings before the
// endpoint releases its final plan/backing reference.
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
        if size == 0 || !size.is_multiple_of(0x1000) || usize::try_from(size).is_err() {
            return Err(IvshmemError::AllocationFailed {
                operation: "allocate ivshmem shared memory (size must be a non-zero, addressable \
                            4 KiB multiple)",
            });
        }
        let allocation = allocator.allocate(size)?;
        // Clear the freshly reserved frames through the permanent virtual
        // view so the shared region starts zeroed. No guest mapping can exist
        // before this allocation is attached to a link, so a raw bulk zero is
        // both alias-safe and substantially faster for large BAR2 profiles.
        // SAFETY: the allocation contract guarantees the pointer is valid and
        // writable for exactly `size` bytes; the backing exclusively owns it
        // until release and has not published it to a guest yet.
        let base = allocation.virtual_base();
        unsafe { core::ptr::write_bytes(base, 0, size as usize) };
        Ok(Self {
            size,
            allocation: Some(allocation),
            allocator,
            lock: RawSpinLock::new(()),
        })
    }

    /// Returns the host-physical base of the backing.
    pub fn hpa_base(&self) -> u64 {
        self.allocation().hpa_base()
    }

    fn allocation(&self) -> &BackingAllocation {
        self.allocation
            .as_ref()
            .expect("shared backing allocation exists until drop")
    }

    /// Clears every byte of the backing through the permanent virtual view.
    ///
    /// Called when a link lifecycle ends so a later reactivation cannot
    /// observe state from the previous lifecycle. The caller must have
    /// retired this allocation first and must not hold the link lifecycle
    /// lock. Pages are serialized against every previously issued `Arc` one
    /// at a time, keeping each IRQ-off critical section bounded to 4 KiB.
    pub fn zero(&self) {
        const PAGE_SIZE: usize = 0x1000;
        debug_assert!(self.size.is_multiple_of(PAGE_SIZE as u64));
        let base = self.allocation().virtual_base();
        for offset in (0..self.size as usize).step_by(PAGE_SIZE) {
            let _lock = self.lock.lock_irqsave();
            // SAFETY: `try_new` validated the complete allocation extent and
            // the loop advances in whole pages without leaving it. Volatile
            // byte stores avoid forming a Rust slice while guest mappings are
            // being retired; the host lock orders other host-side accesses.
            for byte in offset..offset + PAGE_SIZE {
                unsafe { base.add(byte).write_volatile(0) };
            }
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

    /// Publishes one aligned, little-endian host-owned dword with release
    /// ordering.
    ///
    /// State-table entries use this path so a guest observing the published
    /// value also observes the payload writes that preceded it.
    pub(crate) fn write_u32_release(&self, offset: u64, value: u32) -> Result<(), IvshmemError> {
        let range = self.byte_range(offset, size_of::<u32>() as u64)?;
        if !offset.is_multiple_of(align_of::<u32>() as u64) {
            return Err(IvshmemError::UnalignedSharedMemoryAccess {
                offset,
                width: size_of::<u32>(),
            });
        }
        let _lock = self.lock.lock_irqsave();
        // SAFETY: `byte_range` proved the dword extent and `offset` was
        // checked for atomic alignment. The allocation base is page aligned.
        // Host-owned state words are only written atomically; the volatile
        // guest readers are external to the Rust alias model.
        let state = unsafe {
            AtomicU32::from_ptr(
                self.allocation()
                    .virtual_base()
                    .add(range.start)
                    .cast::<u32>(),
            )
        };
        state.store(value.to_le(), Ordering::Release);
        Ok(())
    }

    /// Publishes one aligned, little-endian host-owned epoch with release
    /// ordering.
    ///
    /// Guests observe the same word through an acquire load. The value is
    /// encoded explicitly to remain little-endian on every supported host.
    pub(crate) fn write_u64_release(&self, offset: u64, value: u64) -> Result<(), IvshmemError> {
        let range = self.byte_range(offset, size_of::<u64>() as u64)?;
        if !offset.is_multiple_of(align_of::<u64>() as u64) {
            return Err(IvshmemError::UnalignedSharedMemoryAccess {
                offset,
                width: size_of::<u64>(),
            });
        }
        let _lock = self.lock.lock_irqsave();
        // SAFETY: `byte_range` proved the byte extent, `offset` was checked
        // for u64 alignment, and allocation bases are page aligned. The
        // backing lock excludes every host-side non-atomic access to this
        // word while the release store executes.
        let epoch = unsafe {
            AtomicU64::from_ptr(
                self.allocation()
                    .virtual_base()
                    .add(range.start)
                    .cast::<u64>(),
            )
        };
        epoch.store(value.to_le(), Ordering::Release);
        Ok(())
    }

    /// Acquires one aligned host-owned epoch word.
    #[cfg(test)]
    pub(crate) fn read_u64_acquire(&self, offset: u64) -> Result<u64, IvshmemError> {
        let range = self.byte_range(offset, size_of::<u64>() as u64)?;
        if !offset.is_multiple_of(align_of::<u64>() as u64) {
            return Err(IvshmemError::UnalignedSharedMemoryAccess {
                offset,
                width: size_of::<u64>(),
            });
        }
        let _lock = self.lock.lock_irqsave();
        // SAFETY: the same alignment, extent, base, and lock proof as the
        // release store applies; this path only observes the atomic word.
        let epoch = unsafe {
            AtomicU64::from_ptr(
                self.allocation()
                    .virtual_base()
                    .add(range.start)
                    .cast::<u64>(),
            )
        };
        Ok(u64::from_le(epoch.load(Ordering::Acquire)))
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
        let base = self.allocation().virtual_base();
        if WRITE {
            let byte_count = range.len();
            buffer[..byte_count].copy_from_slice(&value.unwrap_or(0).to_le_bytes()[..byte_count]);
            // SAFETY: `access_range` proved the exact range fits the live
            // allocation. Raw volatile stores avoid a Rust alias over the
            // guest-visible BAR.
            for (index, byte) in buffer[..byte_count].iter().enumerate() {
                unsafe { base.add(range.start + index).write_volatile(*byte) };
            }
        } else {
            // SAFETY: the same extent proof applies; each byte is read with a
            // volatile raw load rather than through a shared Rust slice.
            for (index, byte) in buffer[..range.len()].iter_mut().enumerate() {
                *byte = unsafe { base.add(range.start + index).read_volatile() };
            }
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
        let allocation = self
            .allocation
            .take()
            .expect("shared backing allocation is released exactly once");
        // SAFETY: this object owns the exact token returned by the allocator
        // stored beside it, and dropping the backing transfers the final host
        // access. The endpoint revokes its direct plan during stop and VM reset
        // clears stage-2 before returning the device runtime for release.
        unsafe { self.allocator.release(allocation) };
    }
}

/// Page-aligned heap allocator for in-crate tests: the returned HPA is the
/// virtual address, which pure derivation paths never dereference.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestAllocator {
    live: RawSpinLock<alloc::vec::Vec<usize>>,
    requested_sizes: RawSpinLock<alloc::vec::Vec<u64>>,
    fail_next: AtomicBool,
}

#[cfg(test)]
impl TestAllocator {
    pub(crate) fn allocated_sizes(&self) -> alloc::vec::Vec<u64> {
        self.requested_sizes.lock_irqsave().clone()
    }

    pub(crate) fn fail_next_allocation(&self) {
        self.fail_next.store(true, Ordering::Release);
    }
}

#[cfg(test)]
unsafe impl SharedBackingAllocator for TestAllocator {
    fn allocate(&self, size: u64) -> Result<BackingAllocation, IvshmemError> {
        if self.fail_next.swap(false, Ordering::AcqRel) {
            return Err(IvshmemError::AllocationFailed {
                operation: "test allocator forced failure",
            });
        }
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
        self.requested_sizes.lock_irqsave().push(size);
        // SAFETY: `alloc_zeroed` returned the unique allocation represented by
        // this token, with the requested size and page alignment.
        Ok(unsafe { BackingAllocation::from_raw_parts(pointer as usize as u64, size, pointer) })
    }

    unsafe fn release(&self, allocation: BackingAllocation) {
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
        assert!(backing.hpa_base() != 0);
        assert_eq!(backing.size(), 0x1000);
    }

    #[test]
    fn rejects_unaligned_sizes() {
        assert!(SharedBarBacking::try_new(0, test_allocator()).is_err());
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
        backing.write_u32_release(0, 0x1234_5678).unwrap();
        assert_eq!(backing.read(0, 4).unwrap(), 0x1234_5678);
        // Out-of-range privileged writes fail without corrupting memory.
        assert!(backing.write_u32_release(0x1000, 0xaaaa_aaaa).is_err());
        assert_eq!(backing.read(0, 4).unwrap(), 0x1234_5678);
    }

    #[test]
    fn release_epoch_store_requires_alignment_and_publishes_le() {
        let backing = backing();
        backing
            .write_u64_release(0x100, 0x1122_3344_5566_7788)
            .unwrap();
        assert_eq!(
            backing.read_u64_acquire(0x100).unwrap(),
            0x1122_3344_5566_7788
        );
        assert_eq!(
            backing.write_u64_release(0x104, 1),
            Err(IvshmemError::UnalignedSharedMemoryAccess {
                offset: 0x104,
                width: 8,
            })
        );
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
