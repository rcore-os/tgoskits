use core::sync::atomic::{AtomicBool, Ordering};

use ax_lazyinit::OnceLock;
use axfs_ng_vfs::{VfsError, VfsResult};

pub const PAGE_SIZE: usize = 4096;

pub trait FsPageProvider: Send + Sync {
    fn alloc_page(&self) -> VfsResult<FsPage>;
    fn dealloc_page(&self, page: FsPage);
    fn virt_to_phys(&self, vaddr: usize) -> Option<usize>;
}

#[derive(Debug)]
pub struct FsPage {
    addr: usize,
}

impl FsPage {
    /// # Safety
    ///
    /// `addr` must point to one writable, page-sized, page-aligned kernel
    /// mapping owned by the returned `FsPage`.
    pub const unsafe fn from_raw(addr: usize) -> Self {
        Self { addr }
    }

    pub const fn addr(&self) -> usize {
        self.addr
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.addr as *mut u8
    }

    /// Borrows the uniquely owned page as writable bytes.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `from_raw` requires a writable page-sized allocation owned
        // by this non-Clone token. Requiring `&mut self` prevents safe callers
        // from creating overlapping mutable slices from shared references.
        unsafe { core::slice::from_raw_parts_mut(self.as_mut_ptr(), PAGE_SIZE) }
    }
}

static PAGE_PROVIDER: OnceLock<&'static dyn FsPageProvider> = OnceLock::new();
static PAGE_PROVIDER_READY: AtomicBool = AtomicBool::new(false);

pub fn install_page_provider(provider: &'static dyn FsPageProvider) {
    PAGE_PROVIDER.call_once(|| provider);
    PAGE_PROVIDER_READY.store(true, Ordering::Release);
}

pub fn alloc_page() -> VfsResult<FsPage> {
    PAGE_PROVIDER.get().ok_or(VfsError::BadState)?.alloc_page()
}

pub fn dealloc_page(page: FsPage) {
    if let Some(provider) = PAGE_PROVIDER.get() {
        provider.dealloc_page(page);
    }
}

pub fn virt_to_phys(vaddr: usize) -> Option<usize> {
    PAGE_PROVIDER
        .get()
        .and_then(|provider| provider.virt_to_phys(vaddr))
}

pub fn has_page_provider() -> bool {
    PAGE_PROVIDER_READY.load(Ordering::Acquire)
}

#[cfg(test)]
pub mod test_support {
    use core::sync::atomic::AtomicUsize;
    use std::{
        alloc::{Layout, alloc_zeroed, dealloc},
        collections::BTreeMap,
        ptr::NonNull,
        sync::Mutex,
    };

    use super::*;

    pub struct TestPageProvider {
        translate: AtomicBool,
        // Cached-file registries may retain a page beyond one test scope. Keep
        // its allocation generation so delayed destruction is attributed to
        // the scope that allocated it instead of whichever test runs next.
        generation: AtomicUsize,
        alloc_count: AtomicUsize,
        dealloc_count: AtomicUsize,
        allocation_generations: Mutex<BTreeMap<usize, usize>>,
    }

    impl TestPageProvider {
        const fn new() -> Self {
            Self {
                translate: AtomicBool::new(true),
                generation: AtomicUsize::new(0),
                alloc_count: AtomicUsize::new(0),
                dealloc_count: AtomicUsize::new(0),
                allocation_generations: Mutex::new(BTreeMap::new()),
            }
        }

        pub fn alloc_count(&self) -> usize {
            self.alloc_count.load(Ordering::Acquire)
        }

        pub fn dealloc_count(&self) -> usize {
            self.dealloc_count.load(Ordering::Acquire)
        }

        fn reset(&self, translate: bool) {
            self.generation.fetch_add(1, Ordering::AcqRel);
            self.translate.store(translate, Ordering::Release);
            self.alloc_count.store(0, Ordering::Release);
            self.dealloc_count.store(0, Ordering::Release);
        }
    }

    impl FsPageProvider for TestPageProvider {
        fn alloc_page(&self) -> VfsResult<FsPage> {
            let layout = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap();
            // SAFETY: `layout` has non-zero size and page alignment. The
            // returned allocation is owned by `FsPage` and released with the
            // identical layout in `dealloc_page`.
            let page = NonNull::new(unsafe { alloc_zeroed(layout) }).ok_or(VfsError::NoMemory)?;
            let addr = page.as_ptr() as usize;
            let generation = self.generation.load(Ordering::Acquire);
            let replaced = self
                .allocation_generations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(addr, generation);
            assert!(replaced.is_none(), "test page address is still owned");
            self.alloc_count.fetch_add(1, Ordering::AcqRel);
            Ok(unsafe { FsPage::from_raw(addr) })
        }

        fn dealloc_page(&self, mut page: FsPage) {
            let allocation_generation = self
                .allocation_generations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&page.addr())
                .expect("test page must have a matching allocation");
            let layout = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap();
            // SAFETY: `page` was allocated by `alloc_page` with this exact
            // layout and is transferred here exactly once by `FsPage::drop`.
            unsafe { dealloc(page.as_mut_ptr(), layout) };
            if allocation_generation == self.generation.load(Ordering::Acquire) {
                self.dealloc_count.fetch_add(1, Ordering::AcqRel);
            }
        }

        fn virt_to_phys(&self, vaddr: usize) -> Option<usize> {
            self.translate
                .load(Ordering::Acquire)
                .then_some(vaddr + 0x1000_0000)
        }
    }

    static TEST_PAGE_PROVIDER: TestPageProvider = TestPageProvider::new();
    static TEST_PAGE_PROVIDER_LOCK: Mutex<()> = Mutex::new(());

    pub fn with_test_page_provider<R>(
        translate: bool,
        f: impl FnOnce(&TestPageProvider) -> R,
    ) -> R {
        let _guard = TEST_PAGE_PROVIDER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        install_page_provider(&TEST_PAGE_PROVIDER);
        TEST_PAGE_PROVIDER.reset(translate);
        let result = f(&TEST_PAGE_PROVIDER);
        TEST_PAGE_PROVIDER.translate.store(true, Ordering::Release);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::{test_support::with_test_page_provider, *};

    #[test]
    fn page_provider_allocates_and_deallocates_pages() {
        let previous_scope_page = with_test_page_provider(true, |_| {
            alloc_page().expect("allocate previous-scope page")
        });

        with_test_page_provider(true, |provider| {
            let page = alloc_page().unwrap();
            assert_ne!(page.addr(), 0);
            assert_eq!(page.addr() % PAGE_SIZE, 0);
            assert_eq!(virt_to_phys(page.addr()), Some(page.addr() + 0x1000_0000));
            dealloc_page(previous_scope_page);
            dealloc_page(page);
            assert_eq!(provider.alloc_count(), 1);
            assert_eq!(provider.dealloc_count(), 1);
        });
    }

    #[test]
    fn page_provider_reports_missing_physical_address() {
        with_test_page_provider(false, |_| {
            assert_eq!(virt_to_phys(0x1000), None);
        });
    }
}
