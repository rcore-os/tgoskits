#![no_std]
#![no_main]

extern crate alloc;

use alloc::alloc::{Layout, alloc_zeroed, dealloc};
use core::ptr::NonNull;

use ax_fs_ng as _;
use ax_fs_ng::{
    VfsError, VfsResult,
    os::memory::{FsPage, FsPageProvider, PAGE_SIZE, install_page_provider},
};
use ax_std as _;
use axtest::prelude::*;

struct AxtestPageProvider;

impl FsPageProvider for AxtestPageProvider {
    fn alloc_page(&self) -> VfsResult<FsPage> {
        let layout = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap();
        // SAFETY: the page is allocated with the layout that `dealloc_page`
        // uses, and ownership is transferred to the returned `FsPage`.
        let page = NonNull::new(unsafe { alloc_zeroed(layout) }).ok_or(VfsError::NoMemory)?;
        Ok(unsafe { FsPage::from_raw(page.as_ptr() as usize) })
    }

    fn dealloc_page(&self, page: FsPage) {
        let layout = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap();
        // SAFETY: `page` came from `alloc_page` above and is transferred back
        // exactly once with the identical allocation layout.
        unsafe { dealloc(page.as_mut_ptr(), layout) };
    }

    fn virt_to_phys(&self, _vaddr: usize) -> Option<usize> {
        None
    }
}

static AXTEST_PAGE_PROVIDER: AxtestPageProvider = AxtestPageProvider;

#[axtest]
fn axfsng_block_irq_outcome_and_ready_hold() {
    #[cfg(feature = "axtest")]
    ax_assert!(ax_fs_ng::axtest_support::block_irq_outcome_and_ready_hold_for_test());
}

#[axtest]
fn page_reclaim_releases_registry_spin_lock_before_sleepable_file_locks() {
    #[cfg(all(feature = "axtest", feature = "vfs"))]
    {
        install_page_provider(&AXTEST_PAGE_PROVIDER);
        ax_assert!(ax_fs_ng::axtest_support::reclaim_releases_registry_spin_lock_for_test());
    }
}

#[axtest::tests]
mod tests {}
