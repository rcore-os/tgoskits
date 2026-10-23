// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Generic Linux memory-region-independent records: `struct notifier_block`,
// `struct shrinker` storage, x86_64 `pgprot_t`, `struct io_mapping`,
// `struct resource`, and `drm_mm_node_allocated()`.

use core::ffi::{c_char, c_int, c_ulong, c_void};

use crate::types::DrmMmNode;

// `cachemode2protval()` is an exported x86 helper. `pgprot_noncached()` is
// the x86 header macro selecting UC-minus (cache mode 2) on supported CPUs.
unsafe extern "C" {
    fn cachemode2protval(cache_mode: i32) -> c_ulong;
}

const DRM_MM_NODE_ALLOCATED_BIT: u32 = 0;

/// Source `drm_mm_node_allocated()` from include/drm/drm_mm.h.
///
/// # Safety
/// `node` must be non-null, properly aligned, and point to a valid, initialized `DrmMmNode`
/// that remains valid for reading throughout this function call.
#[inline]
pub unsafe fn drm_mm_node_allocated(node: *const DrmMmNode) -> bool {
    // SAFETY: The caller ensures `node` is non-null, aligned, points to an initialized `DrmMmNode`,
    // and is valid for read for the duration of this call.
    unsafe { (*node).flags & (1 << DRM_MM_NODE_ALLOCATED_BIT) != 0 }
}

/// `struct notifier_block` from `include/linux/notifier.h`.
#[repr(C)]
pub struct NotifierBlock {
    pub notifier_call:
        Option<unsafe extern "C" fn(*mut NotifierBlock, c_ulong, *mut c_void) -> c_int>,
    pub next: *mut NotifierBlock,
    pub priority: c_int,
}

/// Opaque target-configuration storage for `struct shrinker`. The enclosing
/// structure stores only its pointer; keep its source size/alignment for users that need
/// the complete enclosing layout without inventing shrinker behavior here.
#[repr(C, align(8))]
pub struct Shrinker {
    _opaque: [u8; 120],
}

/// x86_64 `pgprot_t` from `arch/x86/include/asm/pgtable_types.h`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgProt {
    pub pgprot: c_ulong,
}

/// x86 `pgprot_decrypted()` is an identity for this configured target:
/// `CONFIG_AMD_MEM_ENCRYPT` is disabled in the reference kernel configuration.
#[inline]
pub fn pgprot_decrypted(prot: PgProt) -> PgProt {
    prot
}

#[inline]
pub fn pgprot_noncached(prot: PgProt) -> PgProt {
    // SAFETY: `cachemode2protval` is an external architecture helper that maps a cache mode
    // enum value to page protection bits without dereferencing pointers or mutating state.
    // Argument 2 corresponds to `_PAGE_CACHE_MODE_UC_MINUS` on x86.
    let uc_minus = unsafe { cachemode2protval(2) };
    PgProt {
        pgprot: prot.pgprot | uc_minus,
    }
}

/// Linux `struct io_mapping` from `include/linux/io-mapping.h`.
#[repr(C)]
pub struct IoMapping {
    pub base: u64,
    pub size: c_ulong,
    pub prot: PgProt,
    pub iomem: *mut c_void,
}

/// Linux `struct resource` from `include/linux/ioport.h` on x86_64.
#[repr(C)]
pub struct Resource {
    pub start: u64,
    pub end: u64,
    pub name: *const c_char,
    pub flags: c_ulong,
    pub desc: c_ulong,
    pub parent: *mut Resource,
    pub sibling: *mut Resource,
    pub child: *mut Resource,
}
