use core::sync::atomic::{AtomicBool, Ordering};

use axfs_ng_vfs::{VfsError, VfsResult};
use spin::Once;

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

    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.addr as *mut u8
    }
}

static PAGE_PROVIDER: Once<&'static dyn FsPageProvider> = Once::new();
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
    use std::sync::Mutex;

    use super::*;

    pub struct TestPageProvider {
        translate: AtomicBool,
        alloc_count: AtomicUsize,
        dealloc_count: AtomicUsize,
    }

    impl TestPageProvider {
        const fn new() -> Self {
            Self {
                translate: AtomicBool::new(true),
                alloc_count: AtomicUsize::new(0),
                dealloc_count: AtomicUsize::new(0),
            }
        }

        pub fn alloc_count(&self) -> usize {
            self.alloc_count.load(Ordering::Acquire)
        }

        pub fn dealloc_count(&self) -> usize {
            self.dealloc_count.load(Ordering::Acquire)
        }

        fn reset(&self, translate: bool) {
            self.translate.store(translate, Ordering::Release);
            self.alloc_count.store(0, Ordering::Release);
            self.dealloc_count.store(0, Ordering::Release);
        }
    }

    impl FsPageProvider for TestPageProvider {
        fn alloc_page(&self) -> VfsResult<FsPage> {
            self.alloc_count.fetch_add(1, Ordering::AcqRel);
            Ok(unsafe { FsPage::from_raw(0x1000) })
        }

        fn dealloc_page(&self, _page: FsPage) {
            self.dealloc_count.fetch_add(1, Ordering::AcqRel);
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
        let _guard = TEST_PAGE_PROVIDER_LOCK.lock().unwrap();
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
        with_test_page_provider(true, |provider| {
            let page = alloc_page().unwrap();
            assert_eq!(page.addr(), 0x1000);
            assert_eq!(virt_to_phys(page.addr()), Some(0x1000_1000));
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
