// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Linux vmap/vunmap policy over the host's kernel virtual-memory service.

#![allow(non_camel_case_types, non_snake_case)]

use alloc::collections::BTreeMap;
use core::{
    ffi::{c_int, c_ulong, c_void},
    ptr,
};

use spin::Mutex;

use crate::{
    config::{EIO, EOPNOTSUPP, PAGE_SIZE},
    gem_memory::PgProt,
    kernel_services::ProviderSlot,
    locks::NoPreemptIrqSave,
    memory::{kfree, kvmalloc_objs},
    shmem::{put_page, try_page_to_phys},
    types::Page,
};

/// Logical cache policy for KAPI-managed kernel aliases. Values deliberately
/// are not architecture PTE encodings; the host maps them to its own policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VmapCacheMode {
    WriteBack,
    WriteCombine,
}

/// Host-owned mapping operations for Linux `vmap()` regions.
pub struct VmapOps {
    /// Map physical pages as a contiguous kernel virtual range. On `Err`, the
    /// provider must have rolled back every installed PTE and completed the
    /// required TLB synchronization; if it cannot prove that, it must fail
    /// stop rather than return an error that permits backing reuse.
    pub map_pages: fn(physical_pages: &[u64], mode: VmapCacheMode) -> Result<*mut c_void, i32>,
    /// Remove the range and complete synchronous TLB invalidation before
    /// returning `Ok`. An error leaves backing ownership with the caller.
    pub unmap: fn(base: *mut c_void, page_count: usize) -> Result<(), i32>,
    /// Translate the host VMA protection to its write-combining form, or
    /// return a Linux negative errno such as `-EOPNOTSUPP`.
    pub pgprot_writecombine: fn(prot: PgProt) -> Result<PgProt, i32>,
}

static VM_OPS: ProviderSlot<VmapOps> = ProviderSlot::new();

/// Install the host's virtual-memory provider before using Linux vmap APIs.
pub fn install_vmap_ops(ops: &'static VmapOps) -> Result<(), &'static str> {
    VM_OPS.install(ops)
}

fn vmap_cache_mode(prot: PgProt) -> Option<VmapCacheMode> {
    match prot.pgprot {
        value if value == PAGE_KERNEL.pgprot => Some(VmapCacheMode::WriteBack),
        value if value == PAGE_KERNEL_WC.pgprot => Some(VmapCacheMode::WriteCombine),
        _ => None,
    }
}

/// Linux `VM_MAP_PUT_PAGES`: successful `vmap` owns page references and the
/// page-pointer array until `vfree`.
pub const VM_MAP_PUT_PAGES: c_ulong = 0x0000_0200;

/// Internal sentinel values used only to request generic vmap cache modes.
/// They are not PTE values and must not be installed directly by a host.
pub const PAGE_KERNEL: PgProt = PgProt { pgprot: 0 };
pub const PAGE_KERNEL_WC: PgProt = PgProt { pgprot: 1 };

#[derive(Clone, Copy)]
struct VmapRecord {
    page_array: usize,
    page_count: usize,
    put_pages: bool,
    retiring: bool,
}

static VMAP_RECORDS: Mutex<BTreeMap<usize, VmapRecord>> = Mutex::new(BTreeMap::new());

fn with_vmap_records<T>(f: impl FnOnce(&mut BTreeMap<usize, VmapRecord>) -> T) -> T {
    let _irq_guard = NoPreemptIrqSave::new();
    let mut records = VMAP_RECORDS.lock();
    f(&mut records)
}

fn release_page_array(pages: *mut *mut Page, count: usize) {
    for index in 0..count {
        // SAFETY: `pages` was registered from a valid allocation of `count` `Page` pointers,
        // and `index < count`.
        let page = unsafe { pages.add(index).read() };
        // SAFETY: `page` is a live `Page` pointer whose reference was acquired during `vmap`.
        unsafe { put_page(page) };
    }
    // SAFETY: `pages` was allocated via `kmalloc`/`kvmalloc` and ownership was transferred to `vmap`.
    unsafe { kfree(pages) };
}

fn record_vmap_region(address: usize, count: usize, page_array: usize, put_pages: bool) {
    with_vmap_records(|records| {
        assert!(
            records
                .insert(
                    address,
                    VmapRecord {
                        page_array: if put_pages { page_array } else { 0 },
                        page_count: count,
                        put_pages,
                        retiring: false,
                    },
                )
                .is_none(),
            "vmap address reused while an earlier owner was registered"
        );
    });
}

/// Linux `vmap()` for pages allocated by the KAPI shmem allocator. A successful
/// host mapping is synchronously visible before this function returns.
///
/// # Safety
/// `pages` must point to `count` live page pointers that remain valid through
/// mapping. With `VM_MAP_PUT_PAGES`, ownership of the page array and its page
/// references transfers to this mapping until `vfree`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vmap(
    pages: *mut *mut Page,
    count: u32,
    flags: c_ulong,
    prot: PgProt,
) -> *mut c_void {
    let Some(mode) = vmap_cache_mode(prot) else {
        return ptr::null_mut();
    };
    if pages.is_null() || count == 0 || (flags != 0 && flags != VM_MAP_PUT_PAGES) {
        return ptr::null_mut();
    }
    let Some(ops) = VM_OPS.get() else {
        return ptr::null_mut();
    };

    let physical_pages = kvmalloc_objs::<u64, usize>(count as usize);
    if physical_pages.is_null() {
        return ptr::null_mut();
    }
    for index in 0..count as usize {
        // SAFETY: `index < count` and caller guarantees `pages` points to `count` live `Page` pointers.
        let page = unsafe { pages.add(index).read() };
        if page.is_null() || crate::config::IS_ERR(page) {
            // SAFETY: `physical_pages` was allocated via `kvmalloc_objs` and is freed on early exit.
            unsafe { kfree(physical_pages) };
            return ptr::null_mut();
        }
        // SAFETY: `page` is verified non-null and not an error pointer, pointing to a valid `Page`.
        let Some(physical) = (unsafe { try_page_to_phys(page) }) else {
            // SAFETY: `physical_pages` was allocated via `kvmalloc_objs` and is freed on early exit.
            unsafe { kfree(physical_pages) };
            return ptr::null_mut();
        };
        if physical & (PAGE_SIZE - 1) != 0 {
            // SAFETY: `physical_pages` was allocated via `kvmalloc_objs` and is freed on early exit.
            unsafe { kfree(physical_pages) };
            return ptr::null_mut();
        }
        // SAFETY: `physical_pages` was allocated for `count` `u64` items, and `index < count`.
        unsafe { physical_pages.add(index).write(physical as u64) };
    }

    // SAFETY: `physical_pages` points to `count` initialized `u64` values and is not aliased mutably.
    let frames = unsafe { core::slice::from_raw_parts(physical_pages, count as usize) };
    let address = match (ops.map_pages)(frames, mode) {
        Ok(address) if !address.is_null() => address,
        Ok(_) => panic!("vmap provider returned a null successful mapping"),
        Err(_) => {
            // SAFETY: `physical_pages` was allocated via `kvmalloc_objs` and is freed on error.
            unsafe { kfree(physical_pages) };
            return ptr::null_mut();
        }
    };
    // SAFETY: `physical_pages` was allocated via `kvmalloc_objs` and is freed after slice usage.
    unsafe { kfree(physical_pages) };
    record_vmap_region(
        address as usize,
        count as usize,
        pages as usize,
        flags == VM_MAP_PUT_PAGES,
    );
    address
}

/// Linux `vmap_pfn()` for page-frame numbers in the host's permitted mapping
/// domain. The provider rejects physical ranges it cannot safely map.
///
/// # Safety
/// `pfns` must point to `count` readable PFNs in the physical mapping domain
/// permitted by the installed provider.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vmap_pfn(pfns: *mut c_ulong, count: u32, prot: PgProt) -> *mut c_void {
    let Some(mode) = vmap_cache_mode(prot) else {
        return ptr::null_mut();
    };
    let Some(ops) = VM_OPS.get() else {
        return ptr::null_mut();
    };
    if pfns.is_null() || count == 0 {
        return ptr::null_mut();
    }
    let frames = kvmalloc_objs::<u64, usize>(count as usize);
    if frames.is_null() {
        return ptr::null_mut();
    }
    for index in 0..count as usize {
        // SAFETY: `index < count` and caller guarantees `pfns` points to `count` readable PFNs.
        let pfn = unsafe { pfns.add(index).read() };
        let Some(physical) = pfn.checked_mul(PAGE_SIZE as u64) else {
            // SAFETY: `frames` was allocated via `kvmalloc_objs` and is freed on early exit.
            unsafe { kfree(frames) };
            return ptr::null_mut();
        };
        // SAFETY: `frames` was allocated for `count` `u64` items, and `index < count`.
        unsafe { frames.add(index).write(physical) };
    }
    // SAFETY: `frames` points to `count` initialized `u64` entries without mutable aliases.
    let physical_pages = unsafe { core::slice::from_raw_parts(frames, count as usize) };
    let address = match (ops.map_pages)(physical_pages, mode) {
        Ok(address) if !address.is_null() => address,
        Ok(_) => panic!("vmap provider returned a null successful mapping"),
        Err(_) => {
            // SAFETY: `frames` was allocated via `kvmalloc_objs` and is freed on mapping failure.
            unsafe { kfree(frames) };
            return ptr::null_mut();
        }
    };
    // SAFETY: `frames` was allocated via `kvmalloc_objs` and is freed after slice usage.
    unsafe { kfree(frames) };
    record_vmap_region(address as usize, count as usize, 0, false);
    address
}

/// Linux `is_vmalloc_addr()` for live ranges created by this module.
///
/// # Safety
/// `pointer` is treated only as an address value and is never dereferenced;
/// any raw pointer value may be passed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn is_vmalloc_addr(pointer: *const c_void) -> bool {
    let address = pointer as usize;
    with_vmap_records(|records| {
        records
            .range(..=address)
            .next_back()
            .is_some_and(|(base, record)| {
                address < base.saturating_add(record.page_count.saturating_mul(PAGE_SIZE))
            })
    })
}

/// Apply the host's write-combining protection transformation.
pub fn try_pgprot_writecombine(prot: PgProt) -> Result<PgProt, i32> {
    let Some(ops) = VM_OPS.get() else {
        return Err(-EOPNOTSUPP);
    };
    (ops.pgprot_writecombine)(prot).map_err(|error| if error < 0 { error } else { -EIO })
}

/// Linux `pgprot_writecombine()` C entry point. Unsupported hosts return an
/// invalid all-ones protection value, which a subsequent VM operation rejects.
///
/// # Safety
/// `prot` must be a protection token constructed for use by this KAPI; it is
/// passed through the installed provider without dereferencing caller memory.
#[unsafe(export_name = "pgprot_writecombine")]
pub unsafe extern "C" fn pgprot_writecombine(prot: PgProt) -> PgProt {
    try_pgprot_writecombine(prot).unwrap_or(PgProt {
        pgprot: c_ulong::MAX,
    })
}

/// Linux `cachemode2protval()`. Portable KAPI does not invent a host cache
/// encoding; only WB/WC logical values are available without a provider.
///
/// # Safety
/// `cache_mode` must be a Linux cache mode supported by this KAPI; unsupported
/// values are returned as an invalid token and must not be installed in a VMA.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cachemode2protval(cache_mode: i32) -> c_ulong {
    match cache_mode {
        0 => PAGE_KERNEL.pgprot,
        1 => PAGE_KERNEL_WC.pgprot,
        _ => c_ulong::MAX,
    }
}

/// The portable KAPI has no universal PAT capability; the host provider must
/// expose optional WC through its own page-protection callback.
///
/// # Safety
/// This query does not access caller memory and has no additional safety
/// preconditions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pat_enabled() -> bool {
    false
}

/// MTRR/PAT reservation is platform policy and is unsupported by the shared
/// KAPI provider.
///
/// # Safety
/// This unsupported operation does not access caller memory and has no
/// additional safety preconditions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn arch_phys_wc_add(_base: u64, _size: u64) -> c_int {
    -EOPNOTSUPP
}

/// Linux `arch_phys_wc_del()` ABI; this KAPI never creates such reservations.
///
/// # Safety
/// This unsupported no-op does not access caller memory and has no additional
/// safety preconditions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn arch_phys_wc_del(_handle: c_int) {}

/// # Safety
/// If non-null, `address` must be the live base of a mapping returned by `vmap`.
/// The caller must ensure that no concurrent or subsequent accesses to `address` occur.
unsafe fn vunmap_inner(address: *mut c_void, put_pages_ok: bool) {
    if address.is_null() {
        return;
    }
    let base = address as usize;
    let record = with_vmap_records(|records| {
        let record = records
            .get_mut(&base)
            .expect("vfree received an address not owned by KAPI vmap");
        assert!(!record.retiring, "vmap address is already being freed");
        assert!(
            put_pages_ok || !record.put_pages,
            "vunmap cannot release VM_MAP_PUT_PAGES ownership; use vfree"
        );
        record.retiring = true;
        *record
    });

    let ops = VM_OPS
        .get()
        .expect("vmap provider disappeared while a mapping is live");
    (ops.unmap)(address, record.page_count)
        .unwrap_or_else(|_| panic!("vmap TLB retirement failed; retain backing pages"));

    let removed = with_vmap_records(|records| records.remove(&base));
    assert!(
        removed.is_some_and(|old| old.retiring),
        "vmap registry changed during unmap"
    );
    if record.put_pages {
        release_page_array(record.page_array as *mut *mut Page, record.page_count);
    }
}

/// Linux `vfree()` for a region returned by [`vmap`].
///
/// # Safety
/// `address` must be the live base of a mapping returned by `vmap`; it must
/// not be used or freed again after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vfree(address: *mut c_void) {
    // SAFETY: Propagates caller's guarantee that `address` is the live base of a mapping
    // returned by `vmap` and will not be accessed again.
    unsafe { vunmap_inner(address, true) };
}

/// Linux `vunmap()` for a region whose caller retains page ownership.
///
/// # Safety
/// `address` must be the live base of a mapping returned by `vmap` without
/// `VM_MAP_PUT_PAGES`; the backing pages remain the caller's responsibility.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vunmap(address: *mut c_void) {
    // SAFETY: Propagates caller's guarantee that `address` is the live base of a mapping
    // returned by `vmap` without `VM_MAP_PUT_PAGES`.
    unsafe { vunmap_inner(address, false) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_mode_uses_logical_tokens_not_pte_bits() {
        assert_eq!(vmap_cache_mode(PAGE_KERNEL), Some(VmapCacheMode::WriteBack));
        assert_eq!(
            vmap_cache_mode(PAGE_KERNEL_WC),
            Some(VmapCacheMode::WriteCombine)
        );
        assert_eq!(
            vmap_cache_mode(PgProt {
                pgprot: c_ulong::MAX
            }),
            None
        );
    }
}
