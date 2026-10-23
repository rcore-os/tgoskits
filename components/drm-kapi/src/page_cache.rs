// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Linux page-array cache-attribute operations delegated to the hosting kernel.

use core::ffi::{c_int, c_ulong, c_void};

use crate::{
    config::{EINVAL, ENOMEM, EOPNOTSUPP, GFP_KERNEL},
    kernel_services::ProviderSlot,
    memory::{kfree, kmalloc},
    shmem::page_address,
    types::Page,
};

/// Cache attribute requested for a page array.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheAttribute {
    WriteBack,
    WriteCombine,
}

/// Kernel-owned page cache attribute operations.
pub struct PageCacheOps {
    /// Change the cache attribute of the supplied kernel virtual addresses.
    /// `Err` must be a Linux negative errno.
    pub set_pages_cache: fn(&[usize], CacheAttribute) -> Result<(), i32>,
    /// Apply the hosting architecture's write-combining bits to a Linux
    /// `pgprot_t` value.
    pub pgprot_writecombine: fn(c_ulong) -> c_ulong,
}

static PAGE_CACHE: ProviderSlot<PageCacheOps> = ProviderSlot::new();

/// Install the hosting kernel's page cache attribute operations.
pub fn install_page_cache_ops(ops: &'static PageCacheOps) -> Result<(), &'static str> {
    PAGE_CACHE.install(ops)
}

/// Apply the hosting kernel's write-combining protection bits.
///
/// # Panics
/// Panics if the hosting kernel has not installed the page-cache provider.
pub fn pgprot_writecombine(prot: c_ulong) -> c_ulong {
    (PAGE_CACHE
        .require("page cache attribute")
        .pgprot_writecombine)(prot)
}

/// # Safety
/// If `count > 0`, `pages` must be non-null, aligned, and valid for reading `count` contiguous
/// `*mut Page` pointers.
unsafe fn set_pages_array_cache(
    pages: *mut *mut Page,
    count: c_int,
    attribute: CacheAttribute,
) -> c_int {
    if count < 0 || (count != 0 && pages.is_null()) {
        return -EINVAL;
    }
    if count == 0 {
        return 0;
    }

    let Some(bytes) = (count as usize).checked_mul(core::mem::size_of::<usize>()) else {
        return -EINVAL;
    };
    let addresses = kmalloc(bytes, GFP_KERNEL).cast::<usize>();
    if addresses.is_null() {
        return -ENOMEM;
    }

    let mut result = 0;
    for index in 0..count as usize {
        // SAFETY: `index < count`, and caller guarantees `pages` points to `count` contiguous,
        // properly aligned, readable `*mut Page` elements.
        let page = unsafe { pages.add(index).read() };
        if page.is_null() {
            result = -EINVAL;
            break;
        }
        // SAFETY: `page` was verified non-null and points to a valid `Page` instance.
        let address = unsafe { page_address(page) };
        if address.is_null() {
            result = -EINVAL;
            break;
        }
        // SAFETY: `addresses` was allocated with capacity for `count` `usize` items, and `index < count`.
        unsafe { addresses.add(index).write(address as usize) };
    }

    if result == 0 {
        if let Some(ops) = PAGE_CACHE.get() {
            // Copy into a typed slice only after every input page has been
            // checked. The provider owns architecture-specific PAT/page-table
            // updates, cache/TLB maintenance, and transactional rollback.
            // SAFETY: `addresses` points to `count` valid, initialized `usize` addresses and
            // is not aliased mutably during the lifetime of this slice.
            let vaddrs = unsafe { core::slice::from_raw_parts(addresses, count as usize) };
            if let Err(error) = (ops.set_pages_cache)(vaddrs, attribute) {
                result = error;
            }
        } else {
            result = -EOPNOTSUPP;
        }
    }

    // SAFETY: `addresses` was allocated by `kmalloc` above and is freed here without reuse.
    unsafe { kfree(addresses.cast::<c_void>()) };
    result
}

/// Change a page array's direct-map alias to write-combining.
///
/// # Safety
/// `pages` must point to `count` live LinuxKPI pages.
pub unsafe fn set_pages_array_wc_kernel(pages: *mut *mut Page, count: c_int) -> c_int {
    // SAFETY: Propagates caller's guarantee that `pages` points to `count` live LinuxKPI pages.
    unsafe { set_pages_array_cache(pages, count, CacheAttribute::WriteCombine) }
}

/// Restore a page array's direct-map alias to write-back.
///
/// # Safety
/// `pages` must point to `count` live pages previously changed to WC.
pub unsafe fn set_pages_array_wb_kernel(pages: *mut *mut Page, count: c_int) -> c_int {
    // SAFETY: Propagates caller's guarantee that `pages` points to `count` live pages.
    unsafe { set_pages_array_cache(pages, count, CacheAttribute::WriteBack) }
}

/// Linux x86 `set_pages_array_wc()` ABI.
///
/// # Safety
/// `pages` must point to `count` live page pointers that can be changed to
/// write-combine cacheability by the installed VM provider.
#[unsafe(export_name = "set_pages_array_wc")]
pub unsafe extern "C" fn set_pages_array_wc(pages: *mut *mut Page, count: c_int) -> c_int {
    // SAFETY: Caller guarantees `pages` points to `count` live page pointers.
    unsafe { set_pages_array_wc_kernel(pages, count) }
}

/// Linux x86 `set_pages_array_wb()` ABI.
///
/// # Safety
/// `pages` must point to `count` live pages previously changed to WC whose
/// cacheability may be restored to write-back.
#[unsafe(export_name = "set_pages_array_wb")]
pub unsafe extern "C" fn set_pages_array_wb(pages: *mut *mut Page, count: c_int) -> c_int {
    // SAFETY: Caller guarantees `pages` points to `count` live page pointers.
    unsafe { set_pages_array_wb_kernel(pages, count) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_null_page_before_changing_direct_map_attributes() {
        let mut pages = [core::ptr::null_mut()];
        assert_eq!(
            // SAFETY: `pages` is a local stack array of 1 pointer, valid for reads of 1 pointer.
            unsafe { set_pages_array_wc_kernel(pages.as_mut_ptr(), 1) },
            -EINVAL
        );
    }
}
