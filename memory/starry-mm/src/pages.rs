//! Shared-page ownership independent of StarryOS kernel adapters.

use alloc::{sync::Arc, vec::Vec};
use core::{any::Any, ops::Deref};

use ax_errno::{AxError, AxResult};
use ax_memory_addr::PhysAddr;
use ax_page_table::common::PageSize;

use crate::{CommitCharge, reserve_commit};

/// Initialization required for a newly allocated physical page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageInitialization {
    /// The caller will initialize every byte before it can be observed.
    Uninitialized,
    /// The page must be filled with zero before it is returned.
    Zeroed,
}

/// Physical-page allocation capability supplied by the kernel adapter.
pub trait PageSource: Send + Sync {
    /// Allocates one page with the requested size and initialization policy.
    fn alloc_page(&self, initialization: PageInitialization, size: PageSize) -> AxResult<PhysAddr>;

    /// Returns a page previously allocated through this source.
    fn dealloc_page(&self, paddr: PhysAddr, size: PageSize);
}

enum SharedPagesOwner {
    Allocated {
        source: &'static dyn PageSource,
        _commit: CommitCharge<'static>,
    },
    Borrowed {
        _retainer: Option<Arc<dyn Any + Send + Sync>>,
    },
}

/// Physical pages shared by anonymous mappings, futexes, or imported memory.
pub struct SharedPages {
    phys_pages: Vec<PhysAddr>,
    page_size: PageSize,
    owner: SharedPagesOwner,
}

impl SharedPages {
    /// Allocates and zeroes all pages needed for `size` bytes.
    pub fn new(
        size: usize,
        page_size: PageSize,
        source: &'static dyn PageSource,
    ) -> AxResult<Self> {
        let page_bytes = page_size as usize;
        if size == 0 || !size.is_multiple_of(page_bytes) {
            return Err(AxError::InvalidInput);
        }
        let commit = reserve_commit(u64::try_from(size).map_err(|_| AxError::InvalidInput)?)
            .map_err(|_| AxError::NoMemory)?;
        let count = size / page_bytes;
        let mut pages = Vec::with_capacity(count);
        for _ in 0..count {
            match source.alloc_page(PageInitialization::Zeroed, page_size) {
                Ok(paddr) => pages.push(paddr),
                Err(err) => {
                    for paddr in pages.drain(..) {
                        source.dealloc_page(paddr, page_size);
                    }
                    return Err(err);
                }
            }
        }
        Ok(Self {
            phys_pages: pages,
            page_size,
            owner: SharedPagesOwner::Allocated {
                source,
                _commit: commit,
            },
        })
    }

    /// Adopts pages whose lifetime remains owned by `retain`.
    pub fn borrowed(
        phys_pages: Vec<PhysAddr>,
        page_size: PageSize,
        retain: Option<Arc<dyn Any + Send + Sync>>,
    ) -> AxResult<Self> {
        if phys_pages.is_empty() {
            return Err(AxError::InvalidInput);
        }
        Ok(Self {
            phys_pages,
            page_size,
            owner: SharedPagesOwner::Borrowed { _retainer: retain },
        })
    }

    /// Page-table size used by every page in this owner.
    pub const fn page_size(&self) -> PageSize {
        self.page_size
    }

    /// Returns the number of physical pages.
    pub fn len(&self) -> usize {
        self.phys_pages.len()
    }

    /// Returns whether the owner contains no pages.
    pub fn is_empty(&self) -> bool {
        self.phys_pages.is_empty()
    }
}

impl Deref for SharedPages {
    type Target = [PhysAddr];

    fn deref(&self) -> &Self::Target {
        &self.phys_pages
    }
}

impl Drop for SharedPages {
    fn drop(&mut self) {
        if let SharedPagesOwner::Allocated { source, .. } = &self.owner {
            for &paddr in &self.phys_pages {
                source.dealloc_page(paddr, self.page_size);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, sync::Arc};
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct TestPageSource {
        allocations: AtomicUsize,
        deallocations: AtomicUsize,
        fail_after: usize,
    }

    impl PageSource for TestPageSource {
        fn alloc_page(
            &self,
            _initialization: PageInitialization,
            _size: PageSize,
        ) -> AxResult<PhysAddr> {
            let index = self.allocations.fetch_add(1, Ordering::Relaxed);
            if index == self.fail_after {
                return Err(AxError::NoMemory);
            }
            Ok(PhysAddr::from_usize(0x1000 * (index + 1)))
        }

        fn dealloc_page(&self, _paddr: PhysAddr, _size: PageSize) {
            self.deallocations.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn allocation_failure_rolls_back_every_owned_page() {
        let _guard = crate::policy::GLOBAL_COMMIT_TEST_LOCK.lock().unwrap();
        crate::configure_commit_limit(u64::MAX);
        let source = Box::leak(Box::new(TestPageSource {
            allocations: AtomicUsize::new(0),
            deallocations: AtomicUsize::new(0),
            fail_after: 2,
        }));

        assert!(matches!(
            SharedPages::new(4 * 4096, PageSize::Size4K, source),
            Err(AxError::NoMemory)
        ));
        assert_eq!(source.deallocations.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn shared_anonymous_owner_holds_one_commit_charge_across_arc_clones() {
        let _guard = crate::policy::GLOBAL_COMMIT_TEST_LOCK.lock().unwrap();
        crate::configure_commit_limit(u64::MAX);
        let source = Box::leak(Box::new(TestPageSource {
            allocations: AtomicUsize::new(0),
            deallocations: AtomicUsize::new(0),
            fail_after: usize::MAX,
        }));
        let before = crate::committed_bytes();
        let pages = Arc::new(SharedPages::new(4096, PageSize::Size4K, source).unwrap());

        assert_eq!(crate::committed_bytes(), before + 4096);
        let alias = pages.clone();
        assert_eq!(crate::committed_bytes(), before + 4096);
        drop(pages);
        assert_eq!(crate::committed_bytes(), before + 4096);
        drop(alias);
        assert_eq!(crate::committed_bytes(), before);
    }

    #[test]
    fn borrowed_pages_only_drop_the_lifetime_retainer() {
        let retainer = Arc::new(());
        let pages = SharedPages::borrowed(
            alloc::vec![PhysAddr::from_usize(0x1000)],
            PageSize::Size4K,
            Some(retainer.clone()),
        )
        .unwrap();

        assert_eq!(Arc::strong_count(&retainer), 2);
        drop(pages);
        assert_eq!(Arc::strong_count(&retainer), 1);
    }
}
