// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Linux scatter-gather DMA API over the hosting kernel's device DMA service.
//!
//! The KAPI owns Linux's scatterlist traversal and transactional rollback. The
//! host owns device identity, DMA-domain validation, address translation and
//! mapping lifetime. In particular, an absent host provider never means
//! identity DMA is safe.

use core::ffi::{c_int, c_void};

use crate::{
    config::{EINVAL, EIO, EOPNOTSUPP},
    kernel_services::ProviderSlot,
    scatterlist::{sg_next, sg_page},
    shmem::try_page_to_phys,
    types::{Scatterlist, SgTable},
};

/// Linux `enum dma_data_direction` values are passed through unchanged.
pub const DMA_BIDIRECTIONAL: u32 = 0;
pub const DMA_TO_DEVICE: u32 = 1;
pub const DMA_FROM_DEVICE: u32 = 2;
pub const DMA_NONE: u32 = 3;

/// Per-host device-DMA operations.
///
/// The `dev` pointer is an opaque Linux device identity. The provider must
/// validate that it is one of its live devices before mapping or unmapping;
/// it must not dereference an untrusted pointer or silently fall back to
/// identity mapping. `Err` values must be Linux negative errno values.
pub struct DmaOps {
    /// Map one physical scatterlist extent for this device and direction.
    pub map: fn(
        dev: *mut c_void,
        physical: u64,
        size: usize,
        direction: u32,
        attrs: u64,
    ) -> Result<u64, i32>,
    /// Retire an extent previously admitted by `map`.
    pub unmap: fn(
        dev: *mut c_void,
        dma_address: u64,
        size: usize,
        direction: u32,
        attrs: u64,
    ) -> Result<(), i32>,
    /// Report whether the device uses a translated IOMMU DMA domain.
    pub iommu_mapped: fn(dev: *mut c_void) -> bool,
}

static DMA_OPS: ProviderSlot<DmaOps> = ProviderSlot::new();

/// Install the host's device-DMA operations before publishing DRM devices.
pub fn install_dma_ops(ops: &'static DmaOps) -> Result<(), &'static str> {
    DMA_OPS.install(ops)
}

fn normalize_errno(errno: i32) -> i32 {
    if errno < 0 { errno } else { -EIO }
}

/// Linux `device_iommu_mapped()`.
#[unsafe(export_name = "device_iommu_mapped")]
pub extern "C" fn device_iommu_mapped(dev: *mut c_void) -> bool {
    DMA_OPS.get().is_some_and(|ops| (ops.iommu_mapped)(dev))
}

/// Map every original scatterlist segment, rolling back the completed prefix
/// if any segment is invalid or rejected by the host.
///
/// # Safety
/// `sg` must be non-null, properly aligned, and point to a live chain of at
/// least `nents` initialized [`Scatterlist`] entries whose backing pages remain
/// valid and unaliased for DMA throughout the mapping. Exclusive write access is
/// required to write `dma_address` and `dma_length`. `dev` must be a valid device
/// handle accepted by the installed provider.
pub unsafe fn try_dma_map_sg_attrs(
    dev: *mut c_void,
    mut sg: *mut Scatterlist,
    nents: u32,
    direction: u32,
    attrs: u64,
) -> Result<u32, i32> {
    let Some(ops) = DMA_OPS.get() else {
        return Err(-EOPNOTSUPP);
    };
    if dev.is_null() || sg.is_null() || nents == 0 || direction > DMA_FROM_DEVICE {
        return Err(-EINVAL);
    }

    let first = sg;
    let mut done = 0;
    while done < nents {
        if sg.is_null() {
            // SAFETY: `first` was verified non-null and valid for at least `done` mapped entries,
            // which were successfully mapped by `ops.map` earlier in this loop.
            unsafe { rollback_mapped(ops, dev, first, done, direction, attrs) };
            return Err(-EINVAL);
        }

        // SAFETY: `sg` is non-null and points to a valid initialized `Scatterlist` in the chain.
        let page = unsafe { sg_page(sg) };
        // SAFETY: `page` is obtained from `sg_page(sg)` and represents the backing page pointer.
        let Some(physical) = (unsafe { try_page_to_phys(page) })
            // SAFETY: `sg` is non-null, properly aligned, and points to a valid `Scatterlist`.
            .and_then(|base| (base as u64).checked_add(unsafe { (*sg).offset } as u64))
        else {
            // SAFETY: `first` was verified non-null and valid for at least `done` mapped entries.
            unsafe { rollback_mapped(ops, dev, first, done, direction, attrs) };
            return Err(-EINVAL);
        };
        // SAFETY: `sg` is non-null, properly aligned, and points to a valid `Scatterlist`.
        let size = unsafe { (*sg).length as usize };
        if size == 0 || physical.checked_add(size as u64).is_none() {
            // SAFETY: `first` was verified non-null and valid for at least `done` mapped entries.
            unsafe { rollback_mapped(ops, dev, first, done, direction, attrs) };
            return Err(-EINVAL);
        }

        match (ops.map)(dev, physical, size, direction, attrs) {
            // SAFETY: `sg` is non-null, aligned, and exclusively accessed to store the returned DMA address and length.
            Ok(address) => unsafe {
                (*sg).dma_address = address;
                (*sg).dma_length = size as u32;
            },
            Err(error) => {
                // SAFETY: `first` was verified non-null and valid for at least `done` mapped entries.
                unsafe { rollback_mapped(ops, dev, first, done, direction, attrs) };
                return Err(normalize_errno(error));
            }
        }

        done += 1;
        // SAFETY: `sg` is a valid entry in the scatterlist chain of length at least `nents`.
        sg = unsafe { sg_next(sg) };
    }
    Ok(done)
}

/// Roll back previously mapped scatterlist segments.
///
/// # Safety
/// `sg` must be non-null, properly aligned, and point to a live scatterlist chain
/// whose first `nents` entries have been mapped with matching `direction` and `attrs`
/// for `dev`. The caller must grant exclusive access to clear mapping fields.
unsafe fn rollback_mapped(
    ops: &DmaOps,
    dev: *mut c_void,
    mut sg: *mut Scatterlist,
    nents: u32,
    direction: u32,
    attrs: u64,
) {
    for _ in 0..nents {
        assert!(!sg.is_null());
        // SAFETY: `sg` is non-null, aligned, and points to a valid mapped `Scatterlist` entry.
        let (address, size) = unsafe { ((*sg).dma_address, (*sg).dma_length as usize) };
        (ops.unmap)(dev, address, size, direction, attrs)
            .unwrap_or_else(|_| panic!("DMA rollback failed; retain backing pages"));
        // SAFETY: `sg` is valid for writes to clear mapping info, and `sg_next` advances to the next entry.
        unsafe {
            (*sg).dma_address = 0;
            (*sg).dma_length = 0;
            sg = sg_next(sg);
        }
    }
}

/// Linux `dma_map_sg_attrs()`. Returns zero on failure, as the Linux API does.
/// The errno-preserving KAPI entry point is [`try_dma_map_sg_attrs`].
///
/// # Safety
/// `sg` must be non-null, aligned, and point to a live chain of at least `nents`
/// initialized [`Scatterlist`] entries whose backing pages remain valid and
/// unaliased for DMA. The caller must provide exclusive write access to `sg`.
pub unsafe fn dma_map_sg_attrs(
    dev: *mut c_void,
    sg: *mut Scatterlist,
    nents: u32,
    direction: u32,
    attrs: u64,
) -> u32 {
    // SAFETY: `sg` points to a live chain of at least `nents` initialized entries with valid
    // backing pages and exclusive write access, satisfying `try_dma_map_sg_attrs` preconditions.
    unsafe { try_dma_map_sg_attrs(dev, sg, nents, direction, attrs) }.unwrap_or(0)
}

/// Linux `dma_unmap_sg_attrs()`.
///
/// # Safety
/// `sg` must be non-null, aligned, and point to the same live scatterlist chain
/// previously mapped for `nents` entries by [`dma_map_sg_attrs`] with matching
/// `dev`, `direction`, and `attrs`. The caller must grant exclusive write access.
pub unsafe fn dma_unmap_sg_attrs(
    dev: *mut c_void,
    mut sg: *mut Scatterlist,
    nents: u32,
    direction: u32,
    attrs: u64,
) {
    let ops = DMA_OPS
        .get()
        .expect("DMA unmap called without an installed host provider");
    for _ in 0..nents {
        assert!(!sg.is_null());
        // SAFETY: `sg` is non-null, aligned, and points to an entry in the live chain mapped with matching device attributes.
        let (address, size) = unsafe { ((*sg).dma_address, (*sg).dma_length as usize) };
        (ops.unmap)(dev, address, size, direction, attrs)
            .unwrap_or_else(|_| panic!("DMA unmap failed; retain backing pages"));
        // SAFETY: `sg` is valid for writes to clear mapping fields, and `sg_next` steps through the chain.
        unsafe {
            (*sg).dma_address = 0;
            (*sg).dma_length = 0;
            sg = sg_next(sg);
        }
    }
}

/// Linux `dma_map_sgtable(dev, sgt, dir, attrs)`: maps original entries and
/// publishes the mapped entry count only after the full mapping succeeds.
///
/// # Safety
/// `sgt` must point to a live writable scatter-gather table whose original
/// entries and backing pages remain valid throughout the provider mapping.
/// `dev` must be null or a device token accepted by the installed provider.
#[unsafe(export_name = "dma_map_sgtable")]
pub unsafe extern "C" fn dma_map_sgtable(
    dev: *mut c_void,
    sgt: *mut SgTable,
    direction: c_int,
    attrs: u64,
) -> c_int {
    if sgt.is_null() {
        return -EINVAL;
    }
    // SAFETY: `sgt` is checked non-null and points to an aligned, live `SgTable` with valid `sgl` and `orig_nents`.
    let (sgl, orig_nents) = unsafe { ((*sgt).sgl, (*sgt).orig_nents) };
    if sgl.is_null() || orig_nents == 0 {
        return -EINVAL;
    }
    // SAFETY: `sgl` is non-null and points to `orig_nents` initialized scatterlist entries in `sgt`.
    match unsafe { try_dma_map_sg_attrs(dev, sgl, orig_nents, direction as u32, attrs) } {
        Ok(mapped) => {
            // SAFETY: `sgt` is a valid, writable `SgTable` exclusively accessed to store the mapped count.
            unsafe { (*sgt).nents = mapped };
            0
        }
        Err(error) => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_callback_errno_sign() {
        assert_eq!(normalize_errno(-EINVAL), -EINVAL);
        assert_eq!(normalize_errno(0), -EIO);
        assert_eq!(normalize_errno(EINVAL), -EIO);
    }
}
