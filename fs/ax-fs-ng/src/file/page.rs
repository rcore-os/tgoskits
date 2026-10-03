use alloc::sync::Arc;
#[cfg(test)]
use core::sync::atomic::{AtomicUsize, Ordering};
use core::{
    fmt,
    io::BorrowedCursor,
    ops::{Deref, DerefMut},
};

use axfs_ng_vfs::{VfsError, VfsResult};

use crate::os::{
    memory::{FsPage, PAGE_SIZE},
    sync::{SleepMutex, SleepMutexGuard},
};

pub struct PageCache {
    frame: Arc<CachedPageFrame>,
    #[cfg(test)]
    dirty_drop_observer: Option<Arc<AtomicUsize>>,
    /// Transient users that hold the frame identity outside the cache-index
    /// lock while publishing or validating a PTE.
    pub(super) pins: usize,
    pub(super) dirty: bool,
    pub(super) dirty_generation: u64,
    writeback: WritebackState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WritebackState {
    Idle,
    Active,
    Redirtied,
}

struct CachedPageFrame {
    page: Option<FsPage>,
    bytes: SleepMutex<()>,
}

/// Retains initialized physical storage independently of cache membership.
///
/// This capability does not prove current EOF, file epoch, or permission to
/// publish a PTE. Private writers must copy before granting writable access.
#[derive(Clone)]
pub struct CachedPageBacking {
    frame: Arc<CachedPageFrame>,
}

/// Serializes kernel byte access to one retained allocation.
///
/// Do not retain a published page's guard across callbacks, mapping operations,
/// I/O, or faultable user access. Unpublished loading pages may hold it for I/O.
pub struct CachedPageBytes<'a> {
    frame: &'a CachedPageFrame,
    _guard: SleepMutexGuard<'a, ()>,
}

impl PageCache {
    pub(super) fn new() -> VfsResult<Self> {
        let page = crate::os::alloc_page().map_err(|err| {
            warn!("Failed to allocate page cache: {:?}", err);
            VfsError::NoMemory
        })?;
        Ok(Self {
            frame: Arc::new(CachedPageFrame {
                page: Some(page),
                bytes: SleepMutex::new(()),
            }),
            #[cfg(test)]
            dirty_drop_observer: None,
            pins: 0,
            dirty: false,
            dirty_generation: 0,
            writeback: WritebackState::Idle,
        })
    }

    #[cfg(all(test, feature = "vfs"))]
    pub(super) fn detached_for_test() -> Self {
        Self {
            frame: Arc::new(CachedPageFrame {
                page: None,
                bytes: SleepMutex::new(()),
            }),
            dirty_drop_observer: None,
            pins: 0,
            dirty: false,
            dirty_generation: 0,
            writeback: WritebackState::Idle,
        }
    }

    /// Returns the physical address of this page.
    pub fn paddr(&self) -> VfsResult<usize> {
        let page = self.frame.page.as_ref().ok_or(VfsError::BadState)?;
        crate::os::virt_to_phys(page.addr()).ok_or(VfsError::BadState)
    }

    /// Marks this page as dirty so it will be flushed on eviction.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        if self.writeback != WritebackState::Idle {
            self.writeback = WritebackState::Redirtied;
        }
        self.dirty_generation = self.dirty_generation.wrapping_add(1);
    }

    pub(super) fn begin_writeback(&mut self) -> VfsResult<()> {
        if self.writeback != WritebackState::Idle {
            return Err(VfsError::ResourceBusy);
        }
        self.writeback = WritebackState::Active;
        Ok(())
    }

    pub(super) fn complete_writeback(&mut self, generation: u64) {
        if self.writeback == WritebackState::Active && self.dirty_generation == generation {
            self.dirty = false;
        }
    }

    pub(super) fn finish_writeback(&mut self) {
        self.writeback = WritebackState::Idle;
    }

    /// Locks bytes against all retained physical backing readers.
    pub fn data(&mut self) -> CachedPageBytes<'_> {
        self.frame.lock_bytes()
    }

    pub(super) fn backing(&self) -> CachedPageBacking {
        CachedPageBacking {
            frame: self.frame.clone(),
        }
    }

    pub(super) fn matches_backing(&self, backing: &CachedPageBacking) -> bool {
        Arc::ptr_eq(&self.frame, &backing.frame)
    }

    /// Retires a page whose cached contents were invalidated by a file-layout
    /// change rather than persisted by writeback.
    ///
    /// The caller must first retire every mapping of this frame. Consuming the
    /// owner makes it impossible to accidentally restore invalidated dirty
    /// contents to the cache after this transition.
    pub(super) fn retire_invalidated(mut self) {
        self.dirty = false;
    }

    #[cfg(test)]
    pub(super) fn observe_dirty_drop(&mut self, observer: Arc<AtomicUsize>) {
        self.dirty_drop_observer = Some(observer);
    }
}

impl Drop for PageCache {
    fn drop(&mut self) {
        if self.dirty {
            #[cfg(test)]
            if let Some(observer) = &self.dirty_drop_observer {
                observer.fetch_add(1, Ordering::AcqRel);
            }
            warn!("dirty page dropped without flushing");
        }
    }
}

impl CachedPageBacking {
    /// Copies one complete page to exclusively borrowed resident storage.
    pub fn copy_to(&self, mut destination: BorrowedCursor<'_, u8>) -> VfsResult<()> {
        if destination.capacity() != PAGE_SIZE {
            return Err(VfsError::InvalidInput);
        }
        let _bytes = self.frame.bytes.lock();
        let source = self.frame.address() as *const u64;
        let mut snapshot = [0u8; 64];
        for start in (0..PAGE_SIZE).step_by(snapshot.len()) {
            for (index, word) in snapshot
                .as_chunks_mut::<{ size_of::<u64>() }>()
                .0
                .iter_mut()
                .enumerate()
            {
                // SAFETY: the retained allocation contains one initialized,
                // page-aligned page. These aligned word reads remain within
                // it. The byte lock excludes kernel mutations; MAP_SHARED
                // writers may still change bytes, so no Rust source reference
                // is created. Volatile access follows the mapped-memory
                // contract and does not promise an atomic page snapshot.
                let value = unsafe { source.add(start / size_of::<u64>() + index).read_volatile() };
                word.copy_from_slice(&value.to_ne_bytes());
            }
            destination.append(&snapshot);
        }
        Ok(())
    }
}

impl fmt::Debug for CachedPageBacking {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachedPageBacking")
            .field("address", &self.frame.address())
            .finish_non_exhaustive()
    }
}

impl CachedPageFrame {
    fn address(&self) -> usize {
        self.page
            .as_ref()
            .expect("live cache backing owns its allocation")
            .addr()
    }

    fn lock_bytes(&self) -> CachedPageBytes<'_> {
        CachedPageBytes {
            frame: self,
            _guard: self.bytes.lock(),
        }
    }
}

impl Drop for CachedPageFrame {
    fn drop(&mut self) {
        if let Some(page) = self.page.take() {
            crate::os::memory::dealloc_page(page);
        }
    }
}

impl Deref for CachedPageBytes<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        // SAFETY: the retained FsPage owns PAGE_SIZE initialized resident bytes.
        // This guard excludes other kernel byte borrowers. User mappings use
        // the OS raw mapped-memory contract, and private backing is read-only.
        unsafe { core::slice::from_raw_parts(self.frame.address() as *const u8, PAGE_SIZE) }
    }
}

impl DerefMut for CachedPageBytes<'_> {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: the unique byte guard excludes kernel aliases across the cache
        // entry and retained backings; the allocation remains live throughout.
        unsafe { core::slice::from_raw_parts_mut(self.frame.address() as *mut u8, PAGE_SIZE) }
    }
}

impl fmt::Debug for CachedPageBytes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

#[cfg(test)]
mod tests {
    use core::io::BorrowedBuf;

    use super::*;
    use crate::os::memory::test_support::with_test_page_provider;

    #[test]
    fn retained_backing_survives_eviction_and_releases_exactly_once() {
        with_test_page_provider(true, |provider| {
            let mut page = PageCache::new().unwrap();
            page.data().fill(0x5a);
            let backing = page.backing();
            let second = backing.clone();
            drop(page);
            assert_eq!(provider.dealloc_count(), 0);
            let mut bytes = [0; PAGE_SIZE];
            backing
                .copy_to(BorrowedBuf::from(&mut bytes[..]).unfilled())
                .unwrap();
            assert_eq!(bytes, [0x5a; PAGE_SIZE]);
            drop(backing);
            assert_eq!(provider.dealloc_count(), 0);
            drop(second);
            assert_eq!(provider.alloc_count(), 1);
            assert_eq!(provider.dealloc_count(), 1);
        });
    }

    #[test]
    fn backing_copy_checks_bounds_and_serializes_cache_writes() {
        with_test_page_provider(true, |_| {
            let mut page = PageCache::new().unwrap();
            page.data().fill(1);
            let backing = page.backing();
            let mut short = [0x44; 8];
            assert_eq!(
                backing.copy_to(BorrowedBuf::from(&mut short[..]).unfilled()),
                Err(VfsError::InvalidInput)
            );
            assert_eq!(short, [0x44; 8]);
            page.data().fill(2);
            let mut bytes = [0; PAGE_SIZE];
            backing
                .copy_to(BorrowedBuf::from(&mut bytes[..]).unfilled())
                .unwrap();
            assert_eq!(bytes, [2; PAGE_SIZE]);
            let guard = page.data();
            assert!(backing.frame.bytes.try_lock().is_none());
            drop(guard);
            assert!(backing.frame.bytes.try_lock().is_some());
        });
    }
}
