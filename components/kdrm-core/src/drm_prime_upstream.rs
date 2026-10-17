// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_prime.c`.
//!
//! PRIME buffer sharing between devices: handle <-> dma-buf / fd conversions,
//! prime lookup trees, import and export.

#![allow(non_snake_case)]

use core::ffi::{c_char, c_int, c_uint, c_ulong, c_void};

use drm_kapi::{
    config::{EINVAL, ENOMEM, EOVERFLOW, GFP_KERNEL, IS_ERR, PAGE_SHIFT, PAGE_SIZE},
    memory::{kfree, kmalloc},
    mutex::{mutex_init, mutex_lock, mutex_unlock},
    rbtree::{rb_erase, rb_insert_color, rb_link_node},
    scatterlist::{
        sg_alloc_table_from_pages_segment, sg_dma_address, sg_dma_len, sg_free_table, sg_next,
        sg_page,
    },
    shmem::{pfn_to_page, try_page_to_phys},
    types::{File, ListHead, Page, RbNode, SgTable, Spinlock},
};

use crate::{
    dma_resv_types::DmaResv, drm_device_types::DrmDevice, drm_file_types::DrmFile,
    drm_gem_types::DrmGemObject, drm_prime_types::DrmPrimeFilePrivate, iosys_map_types::IosysMap,
    uapi_drm_types::DrmPrimeHandle,
};

const ENOENT: i32 = 2;
const DRM_CLOEXEC: u32 = 0x80000;
const DRM_RDWR: u32 = 0x2;
// Linux include/linux/dma-mapping.h: GEM exports require coherent backing.
const DMA_ATTR_SKIP_CPU_SYNC: u64 = 1 << 5;

unsafe extern "C" {
    fn get_unused_fd_flags(flags: c_int) -> c_int;
    fn put_unused_fd(fd: c_int);
    fn fd_install(fd: c_int, file: *mut c_void);
    fn dma_buf_get(fd: c_int) -> *mut DmaBuf;
    fn dma_buf_put(dmabuf: *mut DmaBuf);
    fn dma_buf_export(exp_info: *const DmaBufExportInfo) -> *mut DmaBuf;
    fn dma_buf_attach(dmabuf: *mut DmaBuf, dev: *mut c_void) -> *mut DmaBufAttachment;
    fn dma_buf_detach(dmabuf: *mut DmaBuf, attach: *mut DmaBufAttachment);
    fn dma_buf_map_attachment_unlocked(
        attach: *mut DmaBufAttachment,
        direction: c_int,
    ) -> *mut SgTable;
    fn dma_buf_unmap_attachment_unlocked(
        attach: *mut DmaBufAttachment,
        sgt: *mut SgTable,
        direction: c_int,
    );
}

#[repr(C)]
pub struct DmaBufOps {
    pub attach: Option<unsafe extern "C" fn(*mut DmaBuf, *mut DmaBufAttachment) -> c_int>,
    pub detach: Option<unsafe extern "C" fn(*mut DmaBuf, *mut DmaBufAttachment)>,
    pub pin: Option<unsafe extern "C" fn(*mut DmaBufAttachment) -> c_int>,
    pub unpin: Option<unsafe extern "C" fn(*mut DmaBufAttachment)>,
    pub map_dma_buf: Option<unsafe extern "C" fn(*mut DmaBufAttachment, c_int) -> *mut SgTable>,
    pub unmap_dma_buf: Option<unsafe extern "C" fn(*mut DmaBufAttachment, *mut SgTable, c_int)>,
    pub release: Option<unsafe extern "C" fn(*mut DmaBuf)>,
    pub begin_cpu_access: Option<unsafe extern "C" fn(*mut DmaBuf, c_int) -> c_int>,
    pub end_cpu_access: Option<unsafe extern "C" fn(*mut DmaBuf, c_int) -> c_int>,
    pub mmap: Option<unsafe extern "C" fn(*mut DmaBuf, *mut c_void) -> c_int>,
    pub vmap: Option<unsafe extern "C" fn(*mut DmaBuf, *mut c_void) -> c_int>,
    pub vunmap: Option<unsafe extern "C" fn(*mut DmaBuf, *mut c_void)>,
}

#[repr(C)]
pub struct DmaBuf {
    pub size: usize,
    pub file: *mut File,
    pub attachments: ListHead,
    pub ops: *const DmaBufOps,
    pub vmapping_counter: u32,
    pub _vmapping_pad: u32,
    pub vmap_ptr: IosysMap,
    pub exp_name: *const c_char,
    pub name: *const c_char,
    pub name_lock: Spinlock,
    pub _name_lock_pad: u32,
    pub owner: *mut c_void,
    pub list_node: ListHead,
    pub r#priv: *mut c_void,
    pub resv: *mut DmaResv,
}

#[repr(C)]
pub struct DmaBufAttachment {
    pub dmabuf: *mut DmaBuf,
    pub dev: *mut c_void,
    pub node: ListHead,
    pub sg_table: *mut SgTable,
    pub r#priv: *mut c_void,
}

#[repr(C)]
pub struct DmaBufExportInfo {
    pub exp_name: *const c_char,
    pub owner: *mut c_void,
    pub ops: *const DmaBufOps,
    pub size: usize,
    pub flags: c_int,
    pub resv: *mut DmaResv,
    pub r#priv: *mut c_void,
}

// Linux v7.2.3 x86_64 `struct dma_buf` / `dma_buf_ops` / `dma_buf_export_info`.
const _: () = {
    assert!(core::mem::size_of::<DmaBufOps>() == 96);
    assert!(core::mem::offset_of!(DmaBuf, ops) == 32);
    assert!(core::mem::offset_of!(DmaBuf, vmap_ptr) == 48);
    assert!(core::mem::offset_of!(DmaBuf, name_lock) == 80);
    assert!(core::mem::offset_of!(DmaBuf, owner) == 88);
    assert!(core::mem::offset_of!(DmaBuf, r#priv) == 112);
    assert!(core::mem::offset_of!(DmaBuf, resv) == 120);
    assert!(core::mem::size_of::<DmaBufExportInfo>() == 56);
};

pub static DRM_GEM_PRIME_DMABUF_OPS: DmaBufOps = DmaBufOps {
    attach: Some(drm_gem_map_attach),
    detach: Some(drm_gem_map_detach),
    pin: None,
    unpin: None,
    map_dma_buf: Some(drm_gem_map_dma_buf),
    unmap_dma_buf: Some(drm_gem_unmap_dma_buf),
    release: Some(drm_gem_dmabuf_release),
    begin_cpu_access: None,
    end_cpu_access: None,
    mmap: Some(drm_gem_dmabuf_mmap),
    vmap: Some(drm_gem_dmabuf_vmap),
    vunmap: Some(drm_gem_dmabuf_vunmap),
};

#[repr(C)]
struct DrmPrimeMember {
    dma_buf: *mut DmaBuf,
    handle: u32,
    rb_buf: RbNode,
    rb_handle: RbNode,
}

// upstream: include/linux/dma-buf.h get_dma_buf()
#[inline]
/// # Safety
/// If `dmabuf` is non-null, it must be a live `DmaBuf` backed by a file reference that the caller
/// already holds. Calling this increments the file reference count, which caller must balance.
pub unsafe fn get_dma_buf(dmabuf: *mut DmaBuf) {
    // SAFETY: Forwarding `dmabuf` to `crate::drm_dma_buf_upstream::get_dma_buf`. If non-null,
    // caller holds an existing file reference and this call increments its reference count.
    unsafe { crate::drm_dma_buf_upstream::get_dma_buf(dmabuf) };
}

// upstream: drivers/gpu/drm/drm_prime.c drm_prime_init_file_private()
#[unsafe(no_mangle)]
/// # Safety
/// If `prime_fpriv` is non-null, it must point to exclusively owned, properly aligned, writable
/// memory for `DrmPrimeFilePrivate` that is not yet published to other threads.
pub unsafe extern "C" fn drm_prime_init_file_private(prime_fpriv: *mut DrmPrimeFilePrivate) {
    if prime_fpriv.is_null() {
        return;
    }
    // SAFETY: `prime_fpriv` is non-null and exclusively owned. Initializing `lock` mutex and
    // setting root pointers of `dmabufs` and `handles` RB trees to null creates an empty, valid state.
    unsafe {
        mutex_init(core::ptr::addr_of_mut!((*prime_fpriv).lock));
        (*prime_fpriv).dmabufs.node = core::ptr::null_mut();
        (*prime_fpriv).handles.node = core::ptr::null_mut();
    }
}

// upstream: drivers/gpu/drm/drm_prime.c drm_prime_destroy_file_private()
#[unsafe(no_mangle)]
/// # Safety
/// If `prime_fpriv` is non-null, it must point to an initialized `DrmPrimeFilePrivate`. The caller
/// must not already hold `prime_fpriv.lock`, and no concurrent thread may access this file's tables.
pub unsafe extern "C" fn drm_prime_destroy_file_private(prime_fpriv: *mut DrmPrimeFilePrivate) {
    if prime_fpriv.is_null() {
        return;
    }
    // SAFETY: `prime_fpriv` is non-null and points to an initialized `DrmPrimeFilePrivate`.
    // Acquiring `lock` serializes cleanup. Traversing `handles` tree, unlinking each node from
    // both trees, releasing its `dma_buf` reference, and freeing each `DrmPrimeMember` drains
    // all registered members without leaks.
    unsafe {
        mutex_lock(core::ptr::addr_of_mut!((*prime_fpriv).lock));
        while !(*prime_fpriv).handles.node.is_null() {
            let node = (*prime_fpriv).handles.node;
            let member = node
                .cast::<u8>()
                .wrapping_sub(core::mem::offset_of!(DrmPrimeMember, rb_handle))
                .cast::<DrmPrimeMember>();
            rb_erase(
                core::ptr::addr_of_mut!((*member).rb_handle),
                core::ptr::addr_of_mut!((*prime_fpriv).handles),
            );
            rb_erase(
                core::ptr::addr_of_mut!((*member).rb_buf),
                core::ptr::addr_of_mut!((*prime_fpriv).dmabufs),
            );
            dma_buf_put((*member).dma_buf);
            kfree(member);
        }
        mutex_unlock(core::ptr::addr_of_mut!((*prime_fpriv).lock));
    }
}

// upstream: drivers/gpu/drm/drm_prime.c drm_prime_add_buf_handle()
#[unsafe(no_mangle)]
/// # Safety
/// `prime_fpriv` must point to an initialized `DrmPrimeFilePrivate` whose `lock` mutex is held by
/// the caller. `dma_buf` must be a valid, live `DmaBuf` with a caller-owned reference. The handle
/// must not already exist in `prime_fpriv`.
pub unsafe extern "C" fn drm_prime_add_buf_handle(
    prime_fpriv: *mut DrmPrimeFilePrivate,
    dma_buf: *mut DmaBuf,
    handle: u32,
) -> i32 {
    if prime_fpriv.is_null() || dma_buf.is_null() {
        return -EINVAL;
    }
    let member =
        kmalloc(core::mem::size_of::<DrmPrimeMember>(), GFP_KERNEL).cast::<DrmPrimeMember>();
    if member.is_null() {
        return -ENOMEM;
    }
    // SAFETY: `member` points to `size_of::<DrmPrimeMember>()` bytes allocated by `kmalloc`,
    // which was checked non-null. Initializing fields, taking an additional reference on `dma_buf`,
    // and inserting `rb_buf` and `rb_handle` into `prime_fpriv`'s RB trees is safe because
    // the caller holds `prime_fpriv.lock` and owns `dma_buf`.
    unsafe {
        member.write(DrmPrimeMember {
            dma_buf,
            handle,
            rb_buf: core::mem::zeroed(),
            rb_handle: core::mem::zeroed(),
        });
        get_dma_buf(dma_buf);

        // Insert into dmabufs tree
        let mut parent: *mut RbNode = core::ptr::null_mut();
        let mut link = core::ptr::addr_of_mut!((*prime_fpriv).dmabufs.node);
        while !(*link).is_null() {
            parent = *link;
            let existing = parent
                .cast::<u8>()
                .wrapping_sub(core::mem::offset_of!(DrmPrimeMember, rb_buf))
                .cast::<DrmPrimeMember>();
            if (dma_buf as usize) < ((*existing).dma_buf as usize) {
                link = core::ptr::addr_of_mut!((*parent).left);
            } else {
                link = core::ptr::addr_of_mut!((*parent).right);
            }
        }
        rb_link_node(core::ptr::addr_of_mut!((*member).rb_buf), parent, link);
        rb_insert_color(
            core::ptr::addr_of_mut!((*member).rb_buf),
            core::ptr::addr_of_mut!((*prime_fpriv).dmabufs),
        );

        // Insert into handles tree
        parent = core::ptr::null_mut();
        link = core::ptr::addr_of_mut!((*prime_fpriv).handles.node);
        while !(*link).is_null() {
            parent = *link;
            let existing = parent
                .cast::<u8>()
                .wrapping_sub(core::mem::offset_of!(DrmPrimeMember, rb_handle))
                .cast::<DrmPrimeMember>();
            if handle < (*existing).handle {
                link = core::ptr::addr_of_mut!((*parent).left);
            } else {
                link = core::ptr::addr_of_mut!((*parent).right);
            }
        }
        rb_link_node(core::ptr::addr_of_mut!((*member).rb_handle), parent, link);
        rb_insert_color(
            core::ptr::addr_of_mut!((*member).rb_handle),
            core::ptr::addr_of_mut!((*prime_fpriv).handles),
        );

        0
    }
}

// upstream: drivers/gpu/drm/drm_prime.c drm_prime_lookup_buf_by_handle()
/// Lookup an exported DMA-BUF by its PRIME handle in the per-file table.
///
/// # Safety
/// `prime_fpriv` must point to an initialized per-file `DrmPrimeFilePrivate`. The caller
/// must hold its mutex lock to serialize `handles` tree traversal.
pub(crate) unsafe fn drm_prime_lookup_buf_by_handle(
    prime_fpriv: *mut DrmPrimeFilePrivate,
    handle: u32,
) -> *mut DmaBuf {
    let mut node = unsafe {
        // SAFETY: The caller holds `prime_fpriv`'s handle-table lock while traversing its RB nodes, and each matching DMA-BUF entry owns a reference.
        (*prime_fpriv).handles.node
    };
    while !node.is_null() {
        let member = node
            .cast::<u8>()
            .wrapping_sub(core::mem::offset_of!(DrmPrimeMember, rb_handle))
            .cast::<DrmPrimeMember>();
        if handle
            < unsafe {
                // SAFETY: The caller holds `prime_fpriv`'s handle-table lock while traversing its RB nodes, and each matching DMA-BUF entry owns a reference.
                (*member).handle
            }
        {
            node = unsafe {
                // SAFETY: The caller holds `prime_fpriv`'s handle-table lock while traversing its RB nodes, and each matching DMA-BUF entry owns a reference.
                (*node).left
            };
        } else if handle
            > unsafe {
                // SAFETY: The caller holds `prime_fpriv`'s handle-table lock while traversing its RB nodes, and each matching DMA-BUF entry owns a reference.
                (*member).handle
            }
        {
            node = unsafe {
                // SAFETY: The caller holds `prime_fpriv`'s handle-table lock while traversing its RB nodes, and each matching DMA-BUF entry owns a reference.
                (*node).right
            };
        } else {
            return unsafe {
                // SAFETY: The caller holds `prime_fpriv`'s handle-table lock while traversing its RB nodes, and each matching DMA-BUF entry owns a reference.
                (*member).dma_buf
            };
        }
    }
    core::ptr::null_mut()
}

// upstream: drivers/gpu/drm/drm_prime.c drm_prime_lookup_buf_handle()
/// Lookup a handle registered for `dma_buf` in the per-file table.
///
/// # Safety
/// `prime_fpriv` must point to an initialized `DrmPrimeFilePrivate` whose `lock` mutex is held
/// by the caller to serialize `dmabufs` tree traversal. If `handle` is non-null, it must point
/// to writable `u32` memory for storing the retrieved handle.
unsafe fn drm_prime_lookup_buf_handle(
    prime_fpriv: *mut DrmPrimeFilePrivate,
    dma_buf: *mut DmaBuf,
    handle: *mut u32,
) -> i32 {
    let mut node = unsafe {
        // SAFETY: The caller holds `prime_fpriv`'s handle-table lock while traversing its RB nodes, and each matching DMA-BUF entry owns a reference.
        (*prime_fpriv).dmabufs.node
    };
    while !node.is_null() {
        let member = node
            .cast::<u8>()
            .wrapping_sub(core::mem::offset_of!(DrmPrimeMember, rb_buf))
            .cast::<DrmPrimeMember>();
        if (dma_buf as usize)
            < (unsafe {
                // SAFETY: The caller holds `prime_fpriv`'s handle-table lock while traversing its RB nodes, and each matching DMA-BUF entry owns a reference.
                (*member).dma_buf
            } as usize)
        {
            node = unsafe {
                // SAFETY: The caller holds `prime_fpriv`'s handle-table lock while traversing its RB nodes, and each matching DMA-BUF entry owns a reference.
                (*node).left
            };
        } else if (dma_buf as usize)
            > (unsafe {
                // SAFETY: The caller holds `prime_fpriv`'s handle-table lock while traversing its RB nodes, and each matching DMA-BUF entry owns a reference.
                (*member).dma_buf
            } as usize)
        {
            node = unsafe {
                // SAFETY: The caller holds `prime_fpriv`'s handle-table lock while traversing its RB nodes, and each matching DMA-BUF entry owns a reference.
                (*node).right
            };
        } else {
            if !handle.is_null() {
                // SAFETY: Matching member found. `handle` is non-null and caller guarantees writable `u32`.
                unsafe {
                    *handle = (*member).handle;
                }
            }
            return 0;
        }
    }
    -ENOENT
}

// upstream: drivers/gpu/drm/drm_prime.c drm_prime_remove_buf_handle()
#[unsafe(no_mangle)]
/// # Safety
/// If `prime_fpriv` is non-null, it must point to an initialized `DrmPrimeFilePrivate` whose
/// mutex the caller holds. Calling this consumes the matching `DrmPrimeMember` and puts its
/// associated `DmaBuf` reference.
pub unsafe extern "C" fn drm_prime_remove_buf_handle(
    prime_fpriv: *mut DrmPrimeFilePrivate,
    handle: u32,
) {
    if prime_fpriv.is_null() {
        return;
    }
    // SAFETY: `prime_fpriv` is non-null and caller holds its mutex lock. Traversing `handles`
    // tree by pointer arithmetic to `DrmPrimeMember`, erasing from both `handles` and `dmabufs`
    // trees via `rb_erase`, putting `member.dma_buf` reference, and deallocating `member` via
    // `kfree` safely removes the handle without racing.
    unsafe {
        let mut node = (*prime_fpriv).handles.node;
        while !node.is_null() {
            let member = node
                .cast::<u8>()
                .wrapping_sub(core::mem::offset_of!(DrmPrimeMember, rb_handle))
                .cast::<DrmPrimeMember>();
            if handle < (*member).handle {
                node = (*node).left;
            } else if handle > (*member).handle {
                node = (*node).right;
            } else {
                rb_erase(
                    core::ptr::addr_of_mut!((*member).rb_handle),
                    core::ptr::addr_of_mut!((*prime_fpriv).handles),
                );
                rb_erase(
                    core::ptr::addr_of_mut!((*member).rb_buf),
                    core::ptr::addr_of_mut!((*prime_fpriv).dmabufs),
                );
                dma_buf_put((*member).dma_buf);
                kfree(member);
                break;
            }
        }
    }
}

// upstream: drivers/gpu/drm/drm_prime.c drm_gem_dmabuf_export()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must point to a live `DrmDevice`. `exp_info` must be non-null and point to an initialized
/// `DmaBufExportInfo` whose `r#priv` field points to a live `DrmGemObject` associated with `dev`.
/// On success, ownership of an extra device and GEM reference is transferred to the new DMA-BUF.
pub unsafe extern "C" fn drm_gem_dmabuf_export(
    dev: *mut DrmDevice,
    exp_info: *mut DmaBufExportInfo,
) -> *mut DmaBuf {
    let obj = unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        (*exp_info).r#priv
    }
    .cast::<DrmGemObject>();
    let dma_buf = unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        dma_buf_export(exp_info)
    };
    if (dma_buf as isize) < 0 && (dma_buf as isize) > -4096 {
        return dma_buf;
    }
    unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        crate::drm_drv_upstream::drm_dev_get(dev)
    };
    unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        crate::drm_gem_upstream::drm_gem_object_get(obj)
    };
    dma_buf
}

// upstream: drivers/gpu/drm/drm_prime.c drm_gem_dmabuf_release()
#[unsafe(no_mangle)]
/// # Safety
/// If `dma_buf` is non-null, it must be a `DmaBuf` created by `drm_gem_dmabuf_export` undergoing its
/// final release. Calling this consumes the device and GEM object references taken at export time.
pub unsafe extern "C" fn drm_gem_dmabuf_release(dma_buf: *mut DmaBuf) {
    if dma_buf.is_null() {
        return;
    }
    let obj = unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        (*dma_buf).r#priv
    }
    .cast::<DrmGemObject>();
    let dev = unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        (*obj).dev
    };
    unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        crate::drm_gem_upstream::drm_gem_object_put(obj)
    };
    unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        crate::drm_drv_upstream::drm_dev_put(dev)
    };
}

// upstream: drivers/gpu/drm/drm_prime.c drm_gem_prime_fd_to_handle()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` and `file_priv` must be live and associated. `handle` must be non-null and point to writable
/// `u32` memory. Must be called from task context without holding `file_priv.prime.lock` or `dev.object_name_lock`.
pub unsafe extern "C" fn drm_gem_prime_fd_to_handle(
    dev: *mut DrmDevice,
    file_priv: *mut DrmFile,
    prime_fd: i32,
    handle: *mut u32,
) -> i32 {
    let dmabuf = unsafe {
        // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
        dma_buf_get(prime_fd)
    };
    if (dmabuf as isize) < 0 && (dmabuf as isize) > -4096 {
        return dmabuf as isize as i32;
    }
    unsafe {
        // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
        mutex_lock(core::ptr::addr_of_mut!((*file_priv).prime.lock))
    };
    let mut ret = unsafe {
        drm_prime_lookup_buf_handle(core::ptr::addr_of_mut!((*file_priv).prime), dmabuf, handle)
    };
    if ret == 0 {
        unsafe {
            // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
            mutex_unlock(core::ptr::addr_of_mut!((*file_priv).prime.lock))
        };
        unsafe {
            // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
            dma_buf_put(dmabuf)
        };
        return 0;
    }
    unsafe {
        // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
        mutex_lock(core::ptr::addr_of_mut!((*dev).object_name_lock))
    };
    let obj = if let Some(driver) = unsafe {
        // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
        (*dev).driver.as_ref()
    } {
        if let Some(import) = driver.gem_prime_import {
            unsafe {
                // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
                import(dev, dmabuf.cast())
            }
        } else {
            unsafe {
                // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
                drm_gem_prime_import(dev, dmabuf)
            }
        }
    } else {
        unsafe {
            // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
            drm_gem_prime_import(dev, dmabuf)
        }
    };
    if (obj as isize) < 0 && (obj as isize) > -4096 {
        unsafe {
            // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
            mutex_unlock(core::ptr::addr_of_mut!((*dev).object_name_lock))
        };
        unsafe {
            // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
            mutex_unlock(core::ptr::addr_of_mut!((*file_priv).prime.lock))
        };
        unsafe {
            // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
            dma_buf_put(dmabuf)
        };
        return obj as isize as i32;
    }
    if unsafe {
        // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
        (*obj).dma_buf
    }
    .is_null()
    {
        // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
        unsafe {
            (*obj).dma_buf = dmabuf.cast();
        }
        unsafe {
            // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
            get_dma_buf(dmabuf)
        };
    }
    ret = unsafe {
        // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
        crate::drm_gem_upstream::drm_gem_handle_create_tail(file_priv, obj, handle)
    };
    unsafe {
        // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
        crate::drm_gem_upstream::drm_gem_object_put(obj)
    };
    if ret != 0 {
        unsafe {
            // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
            mutex_unlock(core::ptr::addr_of_mut!((*file_priv).prime.lock))
        };
        unsafe {
            // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
            dma_buf_put(dmabuf)
        };
        return ret;
    }
    // SAFETY: Registering the `(dmabuf, handle)` pair into `file_priv.prime` under `prime.lock`.
    ret = unsafe {
        drm_prime_add_buf_handle(core::ptr::addr_of_mut!((*file_priv).prime), dmabuf, *handle)
    };
    unsafe {
        // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
        mutex_unlock(core::ptr::addr_of_mut!((*file_priv).prime.lock))
    };
    if ret != 0 {
        unsafe {
            // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
            crate::drm_gem_upstream::drm_gem_handle_delete(file_priv, *handle)
        };
        unsafe {
            // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
            dma_buf_put(dmabuf)
        };
        return ret;
    }
    unsafe {
        // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
        dma_buf_put(dmabuf)
    };
    0
}

// upstream: drivers/gpu/drm/drm_prime.c drm_prime_fd_to_handle_ioctl()
#[unsafe(no_mangle)]
/// # Safety
/// If `dev`, `file_priv`, or `data` is null, returns `-EINVAL`. Otherwise, `dev` and `file_priv`
/// must be live and associated, and `data` must point to an initialized, kernel-owned `DrmPrimeHandle`.
pub unsafe extern "C" fn drm_prime_fd_to_handle_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    let args = data.cast::<DrmPrimeHandle>();
    if dev.is_null() || file_priv.is_null() || args.is_null() {
        return -EINVAL;
    }
    if let Some(driver) = unsafe {
        // SAFETY: `dev` and `file_priv` remain referenced for the ioctl, and `dma_buf_get` supplies a live DMA-BUF reference until import and handle publication finish.
        (*dev).driver.as_ref()
    } && let Some(fd2p) = driver.prime_fd_to_handle
    {
        // SAFETY: Invoking driver-specific `fd2p` hook with validated pointers and in-kernel `args.handle`.
        return unsafe {
            fd2p(
                dev,
                file_priv,
                (*args).fd,
                core::ptr::addr_of_mut!((*args).handle),
            )
        };
    }
    // SAFETY: Forwarding to standard `drm_gem_prime_fd_to_handle` with validated pointers and `args.handle`.
    unsafe {
        drm_gem_prime_fd_to_handle(
            dev,
            file_priv,
            (*args).fd,
            core::ptr::addr_of_mut!((*args).handle),
        )
    }
}

// upstream: drivers/gpu/drm/drm_prime.c export_and_register_object()
/// Export a GEM object to a DMA-BUF and register it in the file's prime tables.
///
/// # Safety
/// `_dev` and `obj` must be live, properly aligned pointers. `obj` must have a non-zero
/// handle count and initialized funcs. `flags` must contain valid export flags.
unsafe fn export_and_register_object(
    _dev: *mut DrmDevice,
    obj: *mut DrmGemObject,
    flags: u32,
) -> *mut DmaBuf {
    if unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        (*obj).handle_count
    } == 0
    {
        return (-ENOENT as isize) as *mut DmaBuf;
    }
    let dmabuf = if let Some(funcs) = unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        (*obj).funcs.as_ref()
    } {
        if let Some(export) = funcs.export {
            unsafe {
                // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
                export(obj, flags as i32)
            }
            .cast::<DmaBuf>()
        } else {
            unsafe {
                // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
                drm_gem_prime_export(obj, flags as i32)
            }
        }
    } else {
        unsafe {
            // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
            drm_gem_prime_export(obj, flags as i32)
        }
    };
    if (dmabuf as isize) < 0 && (dmabuf as isize) > -4096 {
        return dmabuf;
    }
    // SAFETY: Caching the exported `dmabuf` on `obj.dma_buf`.
    unsafe {
        (*obj).dma_buf = dmabuf.cast();
    }
    unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        get_dma_buf((*obj).dma_buf.cast())
    };
    dmabuf
}

// upstream: drivers/gpu/drm/drm_prime.c drm_gem_prime_handle_to_dmabuf()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` and `file_priv` must be live and associated. Call from blockable task context
/// without holding `file_priv.prime.lock` or `dev.object_name_lock`. A successful result
/// returns a referenced `DmaBuf`.
pub unsafe extern "C" fn drm_gem_prime_handle_to_dmabuf(
    dev: *mut DrmDevice,
    file_priv: *mut DrmFile,
    handle: u32,
    flags: u32,
) -> *mut DmaBuf {
    unsafe {
        // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
        mutex_lock(core::ptr::addr_of_mut!((*file_priv).prime.lock))
    };
    let obj = unsafe {
        // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
        crate::drm_gem_upstream::drm_gem_object_lookup(file_priv, handle)
    };
    if obj.is_null() {
        unsafe {
            // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
            mutex_unlock(core::ptr::addr_of_mut!((*file_priv).prime.lock))
        };
        return (-ENOENT as isize) as *mut DmaBuf;
    }
    // SAFETY: Checking whether `handle` was already exported and cached in `file_priv.prime`.
    let cached = unsafe {
        drm_prime_lookup_buf_by_handle(core::ptr::addr_of_mut!((*file_priv).prime), handle)
    };
    if !cached.is_null() {
        unsafe {
            // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
            get_dma_buf(cached)
        };
        unsafe {
            // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
            crate::drm_gem_upstream::drm_gem_object_put(obj)
        };
        unsafe {
            // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
            mutex_unlock(core::ptr::addr_of_mut!((*file_priv).prime.lock))
        };
        return cached;
    }
    unsafe {
        // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
        mutex_lock(core::ptr::addr_of_mut!((*dev).object_name_lock))
    };
    let mut dmabuf: *mut DmaBuf;
    if !unsafe {
        // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
        (*obj).import_attach
    }
    .is_null()
    {
        dmabuf = unsafe {
            // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
            (*(*obj).import_attach.cast::<DmaBufAttachment>()).dmabuf
        };
        unsafe {
            // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
            get_dma_buf(dmabuf)
        };
    } else if !unsafe {
        // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
        (*obj).dma_buf
    }
    .is_null()
    {
        unsafe {
            // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
            get_dma_buf((*obj).dma_buf.cast())
        };
        dmabuf = unsafe {
            // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
            (*obj).dma_buf
        }
        .cast();
    } else {
        dmabuf = unsafe {
            // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
            export_and_register_object(dev, obj, flags)
        };
        if (dmabuf as isize) < 0 && (dmabuf as isize) > -4096 {
            unsafe {
                // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
                mutex_unlock(core::ptr::addr_of_mut!((*dev).object_name_lock))
            };
            unsafe {
                // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
                crate::drm_gem_upstream::drm_gem_object_put(obj)
            };
            unsafe {
                // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
                mutex_unlock(core::ptr::addr_of_mut!((*file_priv).prime.lock))
            };
            return dmabuf;
        }
    }
    // SAFETY: Recording `(dmabuf, handle)` mapping in `file_priv.prime` under `prime.lock`.
    let ret = unsafe {
        drm_prime_add_buf_handle(core::ptr::addr_of_mut!((*file_priv).prime), dmabuf, handle)
    };
    unsafe {
        // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
        mutex_unlock(core::ptr::addr_of_mut!((*dev).object_name_lock))
    };
    if ret != 0 {
        unsafe {
            // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
            dma_buf_put(dmabuf)
        };
        dmabuf = (ret as isize) as *mut DmaBuf;
    }
    unsafe {
        // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
        crate::drm_gem_upstream::drm_gem_object_put(obj)
    };
    unsafe {
        // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
        mutex_unlock(core::ptr::addr_of_mut!((*file_priv).prime.lock))
    };
    dmabuf
}

// upstream: drivers/gpu/drm/drm_prime.c drm_gem_prime_handle_to_fd()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` and `file_priv` must be live and associated. If non-null, `prime_fd` must point to writable
/// `i32` memory. Must be called from task context with a valid file descriptor table.
pub unsafe extern "C" fn drm_gem_prime_handle_to_fd(
    dev: *mut DrmDevice,
    file_priv: *mut DrmFile,
    handle: u32,
    flags: u32,
    prime_fd: *mut i32,
) -> i32 {
    let fd = unsafe {
        // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
        get_unused_fd_flags(flags as i32)
    };
    if fd < 0 {
        return fd;
    }
    let dmabuf = unsafe {
        // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
        drm_gem_prime_handle_to_dmabuf(dev, file_priv, handle, flags)
    };
    if (dmabuf as isize) < 0 && (dmabuf as isize) > -4096 {
        unsafe {
            // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
            put_unused_fd(fd)
        };
        return dmabuf as isize as i32;
    }
    if !unsafe {
        // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
        (*dmabuf).file
    }
    .is_null()
    {
        unsafe {
            // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
            fd_install(fd, (*dmabuf).file.cast())
        };
    }
    if !prime_fd.is_null() {
        // SAFETY: `prime_fd` is non-null and caller guarantees writable `i32`; writing `fd`.
        unsafe {
            *prime_fd = fd;
        }
    }
    0
}

// upstream: drivers/gpu/drm/drm_prime.c drm_prime_handle_to_fd_ioctl()
#[unsafe(no_mangle)]
/// # Safety
/// If `dev`, `file_priv`, or `data` is null, returns `-EINVAL`. Otherwise `dev` and `file_priv`
/// must be live and associated, and `data` must point to a kernel-copied, writable `DrmPrimeHandle`.
pub unsafe extern "C" fn drm_prime_handle_to_fd_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    let args = data.cast::<DrmPrimeHandle>();
    if dev.is_null() || file_priv.is_null() || args.is_null() {
        return -EINVAL;
    }
    if (unsafe {
        // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
        (*args).flags
    } & !(DRM_CLOEXEC | DRM_RDWR))
        != 0
    {
        return -EINVAL;
    }
    if let Some(driver) = unsafe {
        // SAFETY: The file handle lookup holds the GEM object reference across export; the returned DMA-BUF owns the matching exported references.
        (*dev).driver.as_ref()
    } && let Some(p2fd) = driver.prime_handle_to_fd
    {
        // SAFETY: Calling driver-specific `p2fd` hook with validated pointers and in-kernel `args.fd` out-pointer.
        return unsafe {
            p2fd(
                dev,
                file_priv,
                (*args).handle,
                (*args).flags,
                core::ptr::addr_of_mut!((*args).fd),
            )
        };
    }
    // SAFETY: Calling default `drm_gem_prime_handle_to_fd` with validated pointers and in-kernel `args.fd`.
    unsafe {
        drm_gem_prime_handle_to_fd(
            dev,
            file_priv,
            (*args).handle,
            (*args).flags,
            core::ptr::addr_of_mut!((*args).fd),
        )
    }
}

// upstream: drivers/gpu/drm/drm_prime.c drm_gem_map_attach()
#[unsafe(no_mangle)]
/// # Safety
/// `dma_buf` must be null or a GEM-backed export retained by the DMA-BUF core. Its object and
/// immutable callback table must remain live through the pin callback.
pub unsafe extern "C" fn drm_gem_map_attach(
    dma_buf: *mut DmaBuf,
    _attach: *mut DmaBufAttachment,
) -> i32 {
    if dma_buf.is_null() {
        return -EINVAL;
    }
    let obj = unsafe {
        // SAFETY: The DMA-BUF attachment retains its exporter and SG-table lifetime; map/unmap and attach/detach are paired by the DMA-BUF core contract.
        (*dma_buf).r#priv
    }
    .cast::<DrmGemObject>();
    if let Some(funcs) = unsafe {
        // SAFETY: The DMA-BUF attachment retains its exporter and SG-table lifetime; map/unmap and attach/detach are paired by the DMA-BUF core contract.
        (*obj).funcs.as_ref()
    } && let Some(pin) = funcs.pin
    {
        return unsafe {
            // SAFETY: The DMA-BUF attachment retains its exporter and SG-table lifetime; map/unmap and attach/detach are paired by the DMA-BUF core contract.
            pin(obj)
        };
    }
    0
}

// upstream: drivers/gpu/drm/drm_prime.c drm_gem_map_detach()
#[unsafe(no_mangle)]
/// # Safety
/// If `dma_buf` is non-null, it must be a live `DmaBuf` created by `drm_gem_dmabuf_export`.
/// Calling this pairs with `drm_gem_map_attach` to release any pin held on the buffer.
pub unsafe extern "C" fn drm_gem_map_detach(dma_buf: *mut DmaBuf, _attach: *mut DmaBufAttachment) {
    if dma_buf.is_null() {
        return;
    }
    let obj = unsafe {
        // SAFETY: The DMA-BUF attachment retains its exporter and SG-table lifetime; map/unmap and attach/detach are paired by the DMA-BUF core contract.
        (*dma_buf).r#priv
    }
    .cast::<DrmGemObject>();
    if let Some(funcs) = unsafe {
        // SAFETY: The DMA-BUF attachment retains its exporter and SG-table lifetime; map/unmap and attach/detach are paired by the DMA-BUF core contract.
        (*obj).funcs.as_ref()
    } && let Some(unpin) = funcs.unpin
    {
        unsafe {
            // SAFETY: The DMA-BUF attachment retains its exporter and SG-table lifetime; map/unmap and attach/detach are paired by the DMA-BUF core contract.
            unpin(obj)
        };
    }
}

// upstream: drivers/gpu/drm/drm_prime.c drm_gem_map_dma_buf()
#[unsafe(no_mangle)]
/// # Safety
/// `attach` must be null or a live attachment retaining a GEM-backed export. The caller must
/// serialize mapping against detach and obey the exporter callback contract.
pub unsafe extern "C" fn drm_gem_map_dma_buf(
    attach: *mut DmaBufAttachment,
    direction: c_int,
) -> *mut SgTable {
    if attach.is_null() || !(0..=2).contains(&direction) {
        return drm_kapi::config::ERR_PTR(-EINVAL);
    }
    // SAFETY: The retained attachment owns its exporter throughout mapping.
    let (dmabuf, device) = unsafe { ((*attach).dmabuf, (*attach).dev) };
    if dmabuf.is_null() {
        return drm_kapi::config::ERR_PTR(-EINVAL);
    }
    // SAFETY: This callback belongs to GEM exports; reading `r#priv` gives the retained GEM object.
    let object = unsafe { (*dmabuf).r#priv.cast::<DrmGemObject>() };
    // SAFETY: `object` is valid; reading `(*object).funcs` to inspect the driver SG table hook.
    let callback = unsafe { (*object).funcs.as_ref() }.and_then(|ops| ops.get_sg_table);
    let Some(get_sg) = callback else {
        return drm_kapi::config::ERR_PTR(-38); // Linux ENOSYS
    };
    // SAFETY: The object stays referenced; the callback transfers a fresh SG
    // table with physical page identities, not pre-mapped bus addresses.
    let table = unsafe { get_sg(object) };
    if IS_ERR(table) {
        return table;
    }
    if table.is_null() {
        return drm_kapi::config::ERR_PTR(-ENOMEM);
    }
    // SAFETY: The newly owned table remains live until unmap. The DMA provider
    // validates the importer device and rolls back partial mapping failures.
    let result =
        unsafe { drm_kapi::dma::dma_map_sgtable(device, table, direction, DMA_ATTR_SKIP_CPU_SYNC) };
    if result != 0 {
        // SAFETY: Mapping failed and rolled back; only this caller owns table.
        unsafe {
            sg_free_table(table);
            kfree(table);
        }
        return drm_kapi::config::ERR_PTR(result);
    }
    table
}

// upstream: drivers/gpu/drm/drm_prime.c drm_gem_unmap_dma_buf()
#[unsafe(no_mangle)]
/// # Safety
/// `sgt` must be null or the uniquely owned table returned by the matching GEM map callback.
/// No device or CPU user may access it after unmap; call once per mapping.
pub unsafe extern "C" fn drm_gem_unmap_dma_buf(
    attach: *mut DmaBufAttachment,
    sgt: *mut SgTable,
    direction: c_int,
) {
    if sgt.is_null() {
        return;
    }
    // SAFETY: The attachment and its SG table match the successful map call.
    // Retire DMA access before freeing the table; provider failures fail-stop
    // and retain storage rather than releasing an uncertain device mapping.
    unsafe {
        drm_kapi::dma::dma_unmap_sg_attrs(
            (*attach).dev,
            (*sgt).sgl,
            (*sgt).orig_nents,
            direction as u32,
            DMA_ATTR_SKIP_CPU_SYNC,
        );
        sg_free_table(sgt);
        kfree(sgt);
    }
}

// upstream: drivers/gpu/drm/drm_prime.c drm_gem_dmabuf_vmap()
#[unsafe(no_mangle)]
/// # Safety
/// If `dma_buf` is non-null, it must be a referenced GEM export. `map` must point to writable
/// `IosysMap` storage, with reservation serialization required by `drm_gem_vmap`.
pub unsafe extern "C" fn drm_gem_dmabuf_vmap(dma_buf: *mut DmaBuf, map: *mut c_void) -> i32 {
    if dma_buf.is_null() {
        return -EINVAL;
    }
    let obj = unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        (*dma_buf).r#priv
    }
    .cast::<DrmGemObject>();
    unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        crate::drm_gem_upstream::drm_gem_vmap(obj, map)
    }
}

// upstream: drivers/gpu/drm/drm_prime.c drm_gem_dmabuf_vunmap()
#[unsafe(no_mangle)]
/// # Safety
/// If `dma_buf` is non-null, it must be a referenced GEM export. `map` must hold a successful
/// matching vmap; all users of that mapping must have stopped before this call.
pub unsafe extern "C" fn drm_gem_dmabuf_vunmap(dma_buf: *mut DmaBuf, map: *mut c_void) {
    if !dma_buf.is_null() {
        let obj = unsafe {
            // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
            (*dma_buf).r#priv
        }
        .cast::<DrmGemObject>();
        unsafe {
            // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
            crate::drm_gem_upstream::drm_gem_vunmap(obj, map)
        };
    }
}

// upstream: drivers/gpu/drm/drm_prime.c drm_gem_prime_mmap()
#[unsafe(no_mangle)]
/// # Safety
/// `obj` must be null or a live referenced GEM object. `vma` must satisfy the driver mmap
/// callback contract and remain exclusively accessible during setup.
pub unsafe extern "C" fn drm_gem_prime_mmap(obj: *mut DrmGemObject, vma: *mut c_void) -> i32 {
    if obj.is_null() {
        return -EINVAL;
    }
    unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        crate::drm_gem_upstream::drm_gem_mmap_obj(obj, (*obj).size as c_ulong, vma)
    }
}

// upstream: drivers/gpu/drm/drm_prime.c drm_gem_dmabuf_mmap()
#[unsafe(no_mangle)]
/// # Safety
/// If `dma_buf` is non-null, it must be a live GEM export. `vma` must satisfy the exported GEM driver
/// mmap callback contract and remain valid throughout setup.
pub unsafe extern "C" fn drm_gem_dmabuf_mmap(dma_buf: *mut DmaBuf, vma: *mut c_void) -> i32 {
    if dma_buf.is_null() {
        return -EINVAL;
    }
    let obj = unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        (*dma_buf).r#priv
    }
    .cast::<DrmGemObject>();
    unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        drm_gem_prime_mmap(obj, vma)
    }
}

// upstream: drivers/gpu/drm/drm_prime.c drm_prime_pages_to_sg()
#[unsafe(no_mangle)]
/// # Safety
/// If `_pages` is non-null and `nr_pages > 0`, `_pages` must describe `nr_pages` valid page
/// pointers. Those pages must remain valid and pinned for the lifetime of the returned `SgTable`.
pub unsafe extern "C" fn drm_prime_pages_to_sg(
    _dev: *mut DrmDevice,
    _pages: *mut *mut c_void,
    nr_pages: c_uint,
) -> *mut SgTable {
    if _pages.is_null() || nr_pages == 0 {
        return (-EINVAL as isize) as *mut SgTable;
    }
    let Some(size) = (nr_pages as usize).checked_mul(PAGE_SIZE) else {
        return (-EOVERFLOW as isize) as *mut SgTable;
    };
    let sgt = kmalloc(core::mem::size_of::<SgTable>(), GFP_KERNEL).cast::<SgTable>();
    if sgt.is_null() {
        return (-ENOMEM as isize) as *mut SgTable;
    }
    // SAFETY: kmalloc returned valid writable storage of size_of::<SgTable>(). Zeroing it is valid initialization.
    unsafe { core::ptr::write_bytes(sgt, 0, 1) };
    // SAFETY: `sgt` is newly allocated and zeroed; `_pages` points to `nr_pages` valid page pointers per caller preconditions.
    let ret = unsafe {
        sg_alloc_table_from_pages_segment(
            sgt,
            _pages.cast::<*mut Page>(),
            nr_pages,
            0,
            size as c_ulong,
            u32::MAX,
            GFP_KERNEL,
        )
    };
    if ret != 0 {
        // SAFETY: `sgt` was allocated by kmalloc above and is exclusively owned.
        unsafe { kfree(sgt) };
        return (ret as isize) as *mut SgTable;
    }
    sgt
}

// upstream: drivers/gpu/drm/drm_prime.c drm_prime_get_contiguous_size()
#[unsafe(no_mangle)]
/// # Safety
/// If `sgt` is non-null and not an error pointer, its `sgl` chain must remain valid and unaltered
/// for traversal up to `nents` entries.
pub unsafe extern "C" fn drm_prime_get_contiguous_size(sgt: *mut SgTable) -> c_ulong {
    // SAFETY: `sgt` is checked non-null and not an error pointer; reading `(*sgt).sgl`.
    if sgt.is_null() || IS_ERR(sgt) || unsafe { (*sgt).sgl.is_null() } {
        return 0;
    }
    // SAFETY: `sgt` is valid; reading `(*sgt).nents` total count of scatterlist segments.
    let nents = unsafe { (*sgt).nents };
    // SAFETY: `sgt` is valid; reading `(*sgt).sgl` head pointer.
    let mut sg = unsafe { (*sgt).sgl };
    let mut contiguous = 0u64;
    let mut next_address = 0u64;
    for index in 0..nents {
        // SAFETY: Reading DMA bus address from valid scatterlist entry `sg`.
        let address = unsafe { sg_dma_address(sg) };
        // SAFETY: Reading mapped DMA length from valid scatterlist entry `sg`.
        let length = unsafe { sg_dma_len(sg) } as u64;
        if length == 0 || (index != 0 && address != next_address) {
            break;
        }
        let Some(end) = address.checked_add(length) else {
            break;
        };
        contiguous = contiguous.saturating_add(length);
        next_address = end;
        if index + 1 < nents {
            // SAFETY: Stepping to next scatterlist entry in the chain via `sg_next`.
            sg = unsafe { sg_next(sg) };
            if sg.is_null() {
                break;
            }
        }
    }
    contiguous as c_ulong
}

// upstream: drivers/gpu/drm/drm_prime.c drm_gem_prime_export()
#[unsafe(no_mangle)]
/// # Safety
/// `obj` must be a live referenced GEM object with an initialized reservation and device. The
/// caller must serialize export metadata through the device object-name mutex.
pub unsafe extern "C" fn drm_gem_prime_export(obj: *mut DrmGemObject, flags: c_int) -> *mut DmaBuf {
    let dev = unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        (*obj).dev
    };
    let mut exp_info = DmaBufExportInfo {
        exp_name: c"drm_prime".as_ptr(),
        owner: core::ptr::null_mut(),
        ops: &DRM_GEM_PRIME_DMABUF_OPS,
        size: unsafe {
            // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
            (*obj).size
        },
        flags,
        resv: unsafe {
            // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
            (*obj).resv
        },
        r#priv: obj.cast(),
    };
    unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        drm_gem_dmabuf_export(dev, &mut exp_info)
    }
}

// upstream: drivers/gpu/drm/drm_prime.c drm_gem_is_prime_exported_dma_buf()
#[unsafe(no_mangle)]
/// # Safety
/// If `dev` or `dma_buf` is null, returns false. Otherwise both must be live objects.
/// If `dma_buf` was exported by this module, its `r#priv` must point to an initialized `DrmGemObject`.
pub unsafe extern "C" fn drm_gem_is_prime_exported_dma_buf(
    dev: *mut DrmDevice,
    dma_buf: *mut DmaBuf,
) -> bool {
    if dma_buf.is_null() {
        return false;
    }
    let obj = unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        (*dma_buf).r#priv
    }
    .cast::<DrmGemObject>();
    (unsafe {
        // SAFETY: The caller retains the device/file/DMA-BUF reference required by this PRIME operation until all imports, exports, and transfers finish.
        (*dma_buf).ops
    }) == &DRM_GEM_PRIME_DMABUF_OPS
        && unsafe {
            // SAFETY: Ops match `DRM_GEM_PRIME_DMABUF_OPS`, so `obj` is a valid `DrmGemObject`; reading `(*obj).dev`.
            (*obj).dev
        } == dev
}

// upstream: drivers/gpu/drm/drm_prime.c drm_gem_prime_import_dev()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` and `dma_buf` must be live referenced objects. `attach_dev` must identify a valid
/// DMA device accepted by the exporter. Call from blockable task context; the returned GEM
/// owns any successful attachment.
pub unsafe extern "C" fn drm_gem_prime_import_dev(
    dev: *mut DrmDevice,
    dma_buf: *mut DmaBuf,
    attach_dev: *mut c_void,
) -> *mut DrmGemObject {
    if unsafe {
        // SAFETY: The caller owns a live DMA-BUF reference for the import; its attachment and mapped SG table remain valid until unmap and detach complete.
        drm_gem_is_prime_exported_dma_buf(dev, dma_buf)
    } {
        let obj = unsafe {
            // SAFETY: The caller owns a live DMA-BUF reference for the import; its attachment and mapped SG table remain valid until unmap and detach complete.
            (*dma_buf).r#priv
        }
        .cast::<DrmGemObject>();
        unsafe {
            // SAFETY: The caller owns a live DMA-BUF reference for the import; its attachment and mapped SG table remain valid until unmap and detach complete.
            crate::drm_gem_upstream::drm_gem_object_get(obj)
        };
        return obj;
    }
    if let Some(driver) = unsafe {
        // SAFETY: The caller owns a live DMA-BUF reference for the import; its attachment and mapped SG table remain valid until unmap and detach complete.
        (*dev).driver.as_ref()
    } && let Some(import_sg) = driver.gem_prime_import_sg_table
    {
        let attach = unsafe {
            // SAFETY: The caller owns a live DMA-BUF reference for the import; its attachment and mapped SG table remain valid until unmap and detach complete.
            dma_buf_attach(dma_buf, attach_dev)
        };
        if (attach as isize) < 0 && (attach as isize) > -4096 {
            return attach.cast();
        }
        // SAFETY: Taking an additional reference on `dma_buf` to keep buffer alive while attached.
        unsafe {
            // SAFETY: The caller owns a live DMA-BUF reference for the import; its attachment and mapped SG table remain valid until unmap and detach complete.
            get_dma_buf(dma_buf)
        };
        let sgt = unsafe {
            // SAFETY: The caller owns a live DMA-BUF reference for the import; its attachment and mapped SG table remain valid until unmap and detach complete.
            dma_buf_map_attachment_unlocked(attach, 0)
        };
        if (sgt as isize) < 0 && (sgt as isize) > -4096 {
            // SAFETY: Detaching attachment on mapping failure.
            unsafe {
                // SAFETY: The caller owns a live DMA-BUF reference for the import; its attachment and mapped SG table remain valid until unmap and detach complete.
                dma_buf_detach(dma_buf, attach)
            };
            unsafe {
                // SAFETY: The caller owns a live DMA-BUF reference for the import; its attachment and mapped SG table remain valid until unmap and detach complete.
                dma_buf_put(dma_buf)
            };
            return sgt.cast();
        }
        let obj = unsafe {
            // SAFETY: The caller owns a live DMA-BUF reference for the import; its attachment and mapped SG table remain valid until unmap and detach complete.
            import_sg(dev, attach.cast(), sgt)
        };
        if (obj as isize) < 0 && (obj as isize) > -4096 {
            // SAFETY: Unmapping attachment on driver import error.
            unsafe {
                // SAFETY: The caller owns a live DMA-BUF reference for the import; its attachment and mapped SG table remain valid until unmap and detach complete.
                dma_buf_unmap_attachment_unlocked(attach, sgt, 0)
            };
            unsafe {
                // SAFETY: The caller owns a live DMA-BUF reference for the import; its attachment and mapped SG table remain valid until unmap and detach complete.
                dma_buf_detach(dma_buf, attach)
            };
            unsafe {
                // SAFETY: The caller owns a live DMA-BUF reference for the import; its attachment and mapped SG table remain valid until unmap and detach complete.
                dma_buf_put(dma_buf)
            };
            return obj;
        }
        // SAFETY: Storing `attach` into `obj.import_attach` so `drm_prime_gem_destroy` can unmap and detach on cleanup.
        unsafe {
            (*obj).import_attach = attach.cast();
        }
        // SAFETY: Sharing reservation object between `dma_buf` and imported `obj`.
        unsafe {
            (*obj).resv = (*dma_buf).resv;
        }
        return obj;
    }
    (-EINVAL as isize) as *mut DrmGemObject
}

// upstream: drivers/gpu/drm/drm_prime.c drm_gem_prime_import()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` and `dma_buf` must be live referenced objects. The device DMA owner must remain
/// available during import; the returned GEM owns a successful attachment.
pub unsafe extern "C" fn drm_gem_prime_import(
    dev: *mut DrmDevice,
    dma_buf: *mut DmaBuf,
) -> *mut DrmGemObject {
    unsafe {
        // SAFETY: The caller owns a live DMA-BUF reference for the import; its attachment and mapped SG table remain valid until unmap and detach complete.
        drm_gem_prime_import_dev(dev, dma_buf, (*dev).dma_dev)
    }
}

// upstream: drivers/gpu/drm/drm_prime.c drm_prime_sg_to_page_array()
#[unsafe(no_mangle)]
/// # Safety
/// If `sgt` or `pages` is null, returns `-EINVAL`. Otherwise `sgt` must be a valid, live scatterlist
/// table, and `pages` must be writable for `max_entries` pointers.
pub unsafe extern "C" fn drm_prime_sg_to_page_array(
    sgt: *mut SgTable,
    pages: *mut *mut c_void,
    max_entries: c_int,
) -> c_int {
    if sgt.is_null() || IS_ERR(sgt) || pages.is_null() || max_entries < 0 {
        return -EINVAL;
    }
    // SAFETY: `sgt` is non-null and not an error pointer; reading head of page-backed scatterlist chain.
    let mut sg = unsafe { (*sgt).sgl };
    if sg.is_null() {
        // SAFETY: `sgt` is valid; reading `(*sgt).orig_nents`.
        return if unsafe { (*sgt).orig_nents } == 0 {
            0
        } else {
            -EINVAL
        };
    }
    let mut written = 0usize;
    // SAFETY: `sgt` is valid; reading `orig_nents` to bound iteration.
    for _ in 0..unsafe { (*sgt).orig_nents } {
        if sg.is_null() {
            return -EINVAL;
        }
        // SAFETY: `sg` is checked non-null and points to a valid `Scatterlist` entry.
        let page = unsafe { sg_page(sg) };
        // SAFETY: `page` is a valid page pointer obtained from `sg`.
        let Some(physical) = (unsafe { try_page_to_phys(page) }) else {
            return -EINVAL;
        };
        // SAFETY: `sg` points to a valid `Scatterlist` entry; reading offset and length.
        let offset = unsafe { (*sg).offset as usize };
        // SAFETY: `sg` points to a valid `Scatterlist` entry; reading length.
        let length = unsafe { (*sg).length as usize };
        let Some(span) = offset.checked_add(length) else {
            return -EINVAL;
        };
        let Some(rounded_span) = span.checked_add(PAGE_SIZE - 1) else {
            return -EINVAL;
        };
        let page_count = rounded_span >> PAGE_SHIFT;
        let Some(end) = written.checked_add(page_count) else {
            return -EINVAL;
        };
        if end > max_entries as usize {
            return -EINVAL;
        }
        let first_pfn = physical >> PAGE_SHIFT;
        for page_index in 0..page_count {
            let Some(pfn) = first_pfn.checked_add(page_index) else {
                return -EINVAL;
            };
            let page = pfn_to_page(pfn as c_ulong);
            if page.is_null() {
                return -EINVAL;
            }
            // SAFETY: `pages` is writable for `max_entries`, and `written` is verified < max_entries.
            unsafe { pages.add(written).write(page.cast()) };
            written += 1;
        }
        // SAFETY: Advancing to next scatterlist segment in page-backed `sgt`.
        sg = unsafe { sg_next(sg) };
    }
    written as c_int
}

// upstream: drivers/gpu/drm/drm_prime.c drm_prime_sg_to_dma_addr_array()
#[unsafe(no_mangle)]
/// # Safety
/// If `sgt` or `addrs` is null, returns `-EINVAL`. Otherwise `sgt` must be a valid DMA-mapped
/// scatterlist table, and `addrs` must be writable for `max_entries` `u64` addresses.
pub unsafe extern "C" fn drm_prime_sg_to_dma_addr_array(
    sgt: *mut SgTable,
    addrs: *mut u64,
    max_entries: c_int,
) -> c_int {
    if sgt.is_null() || IS_ERR(sgt) || addrs.is_null() || max_entries < 0 {
        return -EINVAL;
    }
    // SAFETY: `sgt` is non-null and not an error pointer; reading head of DMA-mapped scatterlist chain.
    let mut sg = unsafe { (*sgt).sgl };
    if sg.is_null() {
        // SAFETY: `sgt` is valid; reading `(*sgt).nents`.
        return if unsafe { (*sgt).nents } == 0 {
            0
        } else {
            -EINVAL
        };
    }
    let mut written = 0usize;
    // SAFETY: `sgt` is valid; reading `nents` to bound iteration.
    for _ in 0..unsafe { (*sgt).nents } {
        if sg.is_null() {
            return -EINVAL;
        }
        // SAFETY: `sg` is checked non-null and points to a valid mapped `Scatterlist` entry.
        let address = unsafe { sg_dma_address(sg) };
        // SAFETY: `sg` points to a valid mapped `Scatterlist` entry; reading mapped length.
        let length = unsafe { sg_dma_len(sg) } as usize;
        if length == 0 || (length & (PAGE_SIZE - 1)) != 0 {
            return -EINVAL;
        }
        let page_count = length >> PAGE_SHIFT;
        let Some(end) = written.checked_add(page_count) else {
            return -EINVAL;
        };
        if end > max_entries as usize {
            return -EINVAL;
        }
        for page_index in 0..page_count {
            let Some(offset) = page_index.checked_mul(PAGE_SIZE) else {
                return -EINVAL;
            };
            let Some(page_address) = address.checked_add(offset as u64) else {
                return -EINVAL;
            };
            // SAFETY: `addrs` is writable for `max_entries`, and `written` is verified < max_entries.
            unsafe { addrs.add(written).write(page_address) };
            written += 1;
        }
        // SAFETY: Advancing to next scatterlist segment in DMA-mapped `sgt`.
        sg = unsafe { sg_next(sg) };
    }
    written as c_int
}

// upstream: drivers/gpu/drm/drm_prime.c drm_prime_gem_destroy()
#[unsafe(no_mangle)]
/// # Safety
/// If `obj` is non-null, it must be a `DrmGemObject` in its final-release path. Its import attachment
/// must be valid, and `sg` must be null or the matching imported mapping.
pub unsafe extern "C" fn drm_prime_gem_destroy(obj: *mut DrmGemObject, sg: *mut SgTable) {
    if obj.is_null() {
        return;
    }
    // SAFETY: `obj` is non-null; reading `(*obj).import_attach` to locate import attachment.
    let attach = unsafe { (*obj).import_attach.cast::<DmaBufAttachment>() };
    if attach.is_null() {
        return;
    }
    // SAFETY: `attach` is non-null; reading `(*attach).dmabuf` before detaching.
    let dmabuf = unsafe { (*attach).dmabuf };
    if !sg.is_null() {
        // SAFETY: `attach` and `sg` are valid imported mapping; unmapping DMA attachment.
        unsafe { dma_buf_unmap_attachment_unlocked(attach, sg, 0) };
    }
    // SAFETY: Clearing `import_attach`, detaching attachment from DMA-BUF, and putting DMA-BUF reference.
    unsafe {
        (*obj).import_attach = core::ptr::null_mut();
        dma_buf_detach(dmabuf, attach);
        dma_buf_put(dmabuf);
    }
}

#[cfg(test)]
mod tests {
    use drm_kapi::{scatterlist::SG_END, types::Scatterlist};

    use super::*;
    use crate::{
        drm_drv_types::DrmDriver,
        drm_drv_upstream::{drm_dev_alloc, drm_dev_put},
        drm_file_upstream::{drm_file_alloc, drm_file_free},
        drm_gem_upstream::{
            drm_gem_handle_create, drm_gem_object_lookup, drm_gem_object_put,
            drm_gem_private_object_init,
        },
        drm_syncobj_upstream::test_file_ops,
    };

    #[test]
    fn prime_dma_helpers_walk_dma_mapped_sg_segments() {
        let mut entries = [
            Scatterlist {
                page_link: 0,
                offset: 0,
                length: (2 * PAGE_SIZE) as u32,
                dma_address: 0x1000,
                dma_length: (2 * PAGE_SIZE) as u32,
                _dma_flags_or_padding: 0,
            },
            Scatterlist {
                page_link: SG_END,
                offset: 0,
                length: (2 * PAGE_SIZE) as u32,
                dma_address: 0x8000,
                dma_length: (2 * PAGE_SIZE) as u32,
                _dma_flags_or_padding: 0,
            },
        ];
        let mut table = SgTable {
            sgl: entries.as_mut_ptr(),
            nents: 2,
            orig_nents: 2,
        };
        let mut addresses = [0u64; 4];

        assert_eq!(
            // SAFETY: `table` is initialized with valid `entries` of length 2; `addresses`
            // is a valid 4-element array matching the total page count.
            unsafe { drm_prime_sg_to_dma_addr_array(&mut table, addresses.as_mut_ptr(), 4) },
            4
        );
        assert_eq!(addresses, [0x1000, 0x2000, 0x8000, 0x9000]);
        assert_eq!(
            // SAFETY: `table` is a valid scatterlist table terminating at SG_END.
            unsafe { drm_prime_get_contiguous_size(&mut table) },
            (2 * PAGE_SIZE) as c_ulong
        );
        assert_eq!(
            // SAFETY: `table` is valid; capacity 3 is intentionally smaller than required
            // to verify bounds checking returns -EINVAL.
            unsafe { drm_prime_sg_to_dma_addr_array(&mut table, addresses.as_mut_ptr(), 3) },
            -EINVAL
        );
    }

    #[test]
    fn test_prime_cache_and_roundtrip() {
        let _serial = test_file_ops::serial();
        test_file_ops::install();
        crate::test_support::init_runtime();
        // SAFETY: Test harness setup initializes mock file operations and allocates test DRM
        // device, file, and GEM objects. All allocated resources are properly torn down and freed.
        unsafe {
            // Real exported objects exercise the cache and reference lifecycle;
            // detached fake DMA-BUF records cannot supply the required file pin.
            let driver: DrmDriver = core::mem::zeroed();
            let dev = drm_dev_alloc(&driver, core::ptr::null_mut());
            let file_priv = drm_file_alloc((*dev).primary);

            // Create a real GEM object and handle
            let obj = drm_kapi::memory::kzalloc(core::mem::size_of::<DrmGemObject>(), GFP_KERNEL)
                .cast::<DrmGemObject>();
            assert!(!obj.is_null());
            drm_gem_private_object_init(dev, obj, 4096);
            let mut orig_handle: u32 = 0;
            assert_eq!(drm_gem_handle_create(file_priv, obj, &mut orig_handle), 0);
            assert_ne!(orig_handle, 0);

            let mut args = DrmPrimeHandle {
                handle: orig_handle,
                flags: 0,
                fd: -1,
            };
            assert_eq!(
                drm_prime_handle_to_fd_ioctl(dev, &mut args as *mut _ as *mut _, file_priv),
                0
            );
            assert!(args.fd >= 0);

            // Second export returns the same cached dma-buf
            let mut args_cached = DrmPrimeHandle {
                handle: orig_handle,
                flags: 0,
                fd: -1,
            };
            assert_eq!(
                drm_prime_handle_to_fd_ioctl(dev, &mut args_cached as *mut _ as *mut _, file_priv),
                0
            );
            assert!(args_cached.fd >= 0);

            // Import fd in second file
            let file_priv2 = drm_file_alloc((*dev).primary);
            let mut args2 = DrmPrimeHandle {
                handle: 0,
                flags: 0,
                fd: args.fd,
            };
            assert_eq!(
                drm_prime_fd_to_handle_ioctl(dev, &mut args2 as *mut _ as *mut _, file_priv2),
                0
            );
            assert_ne!(args2.handle, 0);

            // Look up imported object and verify it matches original
            let imported_obj = drm_gem_object_lookup(file_priv2, args2.handle);
            assert_eq!(imported_obj, obj);
            drm_gem_object_put(imported_obj);

            drm_file_free(file_priv2);
            drm_file_free(file_priv);
            // The two exported fd references retain the GEM object after all
            // handles are gone. Close both, then consume the original object pin.
            test_file_ops::close_fd(args.fd);
            test_file_ops::close_fd(args_cached.fd);
            drm_gem_object_put(obj);
            drm_dev_put(dev);
        }
    }
}
