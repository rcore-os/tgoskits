// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors.
//! Device-managed resources.
//!
//! Every device keeps an ordered list of records. A record is either a release
//! action (a callback plus an opaque pointer, run when the device is torn down
//! or when the action is cancelled early) or a managed allocation whose payload
//! lives directly behind the record header. Records are released newest first
//! on teardown.

use core::{
    ffi::{CStr, c_char, c_void},
    mem::{offset_of, size_of},
    ptr::{addr_of_mut, null_mut},
};

use drm_kapi::{
    assert::warn_on,
    config::{ENOMEM, GFP_KERNEL},
    list::{list_add, list_del, list_del_init},
    locks::{spin_lock_irqsave_raw, spin_unlock_irqrestore_raw},
    memory::{kfree, kmalloc, kzalloc},
    mutex::mutex_destroy,
    print::{DrmLogLevel, drm_log_at},
    types::{ListHead, Mutex},
    workqueue::destroy_workqueue,
};

use crate::drm_device_types::DrmDevice;

/// Release callback run for a managed record. It receives the device and the
/// data pointer that was registered with the action (null when none was given).
pub type DrmresReleaseT = Option<unsafe extern "C" fn(*mut DrmDevice, *mut c_void)>;

/// The `__GFP_ZERO` bit in allocation flags.
const GFP_ZERO_FLAG: u32 = 0x100;

const LOG_TAG: &str = "drm_managed";

/// Bookkeeping header placed in front of every managed record. Payload bytes,
/// when present, start at [`PAYLOAD_OFFSET`].
#[repr(C)]
struct ManagedRecord {
    link: ListHead,
    action: DrmresReleaseT,
    /// Heap copy of the caller's label; freed together with the record.
    label: *mut c_char,
    /// Size of the trailing payload in bytes (zero for action records).
    payload_len: usize,
    /// Pointer registered with an action record. Unused for allocations.
    data: *mut c_void,
    /// Zero-sized marker that starts the payload area.
    payload_start: [u64; 0],
}

/// Offset from a record header to its payload. The header is already a
/// multiple of 8 bytes, so the payload is correctly aligned for `u64`.
const PAYLOAD_OFFSET: usize = size_of::<ManagedRecord>();

fn debug_log(message: &str) {
    drm_log_at(DrmLogLevel::Debug, LOG_TAG, file!(), line!(), message);
}

/// Returns the record that contains the list node `link`.
///
/// # Safety
/// `link` must point at the `link` field of a live [`ManagedRecord`].
unsafe fn record_from_link(link: *mut ListHead) -> *mut ManagedRecord {
    // SAFETY: the caller guarantees `link` is the `link` field of a live record, so stepping back by the field offset stays inside that record's allocation.
    unsafe {
        link.cast::<u8>()
            .sub(offset_of!(ManagedRecord, link))
            .cast()
    }
}

/// Returns the payload address of a record.
///
/// # Safety
/// `record` must point at a live [`ManagedRecord`] allocated by [`new_record`].
unsafe fn payload_of(record: *mut ManagedRecord) -> *mut c_void {
    // SAFETY: `new_record` allocated `PAYLOAD_OFFSET + payload_len` bytes behind the header, so the offset is in bounds.
    unsafe { record.cast::<u8>().add(PAYLOAD_OFFSET).cast() }
}

/// Copies a NUL-terminated string into a heap buffer that [`free_record`]
/// releases. Returns null for a null input or when allocation fails.
///
/// # Safety
/// A non-null `text` must point at a readable NUL-terminated string.
unsafe fn duplicate_label(text: *const c_char) -> *mut c_char {
    if text.is_null() {
        return null_mut();
    }
    // SAFETY: the caller guarantees `text` is a readable NUL-terminated string.
    let bytes = unsafe { CStr::from_ptr(text) }.to_bytes_with_nul();
    let copy = kmalloc(bytes.len(), GFP_KERNEL).cast::<u8>();
    if copy.is_null() {
        return null_mut();
    }
    // SAFETY: `copy` is a fresh allocation of `bytes.len()` bytes and cannot overlap the source string.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), copy, bytes.len()) };
    copy.cast()
}

/// Allocates a record with `payload_len` trailing payload bytes. Returns null
/// on failure. The link is initialised as an empty list node, and the caller
/// owns the returned record until it is linked with [`insert_record`].
fn new_record(action: DrmresReleaseT, payload_len: usize, flags: u32) -> *mut ManagedRecord {
    let Some(total) = PAYLOAD_OFFSET.checked_add(payload_len) else {
        return null_mut();
    };
    let raw = if flags & GFP_ZERO_FLAG != 0 {
        kzalloc(total, flags)
    } else {
        kmalloc(total, flags)
    };
    let record = raw.cast::<ManagedRecord>();
    if record.is_null() {
        return null_mut();
    }
    // SAFETY: `record` is a fresh allocation of at least `PAYLOAD_OFFSET + payload_len` bytes with allocator alignment, which satisfies the header's alignment; the header is written in full before any field is read.
    unsafe {
        record.write(ManagedRecord {
            link: ListHead {
                next: null_mut(),
                prev: null_mut(),
            },
            action,
            label: null_mut(),
            payload_len,
            data: null_mut(),
            payload_start: [],
        });
        let link = addr_of_mut!((*record).link);
        (*link).next = link;
        (*link).prev = link;
    }
    record
}

/// Frees a record, its label, and its payload.
///
/// # Safety
/// `record` must be a record returned by [`new_record`] that is no longer
/// linked into any list and has not been freed before.
unsafe fn free_record(record: *mut ManagedRecord) {
    // SAFETY: the caller guarantees `record` is a live, unlinked record, so its label and header are valid to free exactly once.
    unsafe {
        kfree((*record).label);
        kfree(record);
    }
}

/// Describes a record for debug output.
///
/// # Safety
/// `record` must point at a live [`ManagedRecord`].
unsafe fn describe(verb: &str, record: *mut ManagedRecord) -> alloc::string::String {
    // SAFETY: the caller guarantees `record` is live.
    let (label, size) = unsafe {
        let label = (*record).label;
        let text = if label.is_null() {
            "(unnamed)"
        } else {
            CStr::from_ptr(label).to_str().unwrap_or("(invalid)")
        };
        (text, (*record).payload_len)
    };
    alloc::format!("{verb} {record:p} {label} ({size} bytes)")
}

/// Links a record at the front of the device's resource list under the lock.
///
/// # Safety
/// `dev` must point at a live device whose managed lock and list are
/// initialised, and `record` must be an unlinked record from [`new_record`].
unsafe fn insert_record(dev: *mut DrmDevice, record: *mut ManagedRecord) {
    let mut flags: u64 = 0;
    // SAFETY: `dev` is live, so its lock field is a valid spinlock; it is released below with the same flags.
    unsafe {
        let lock = addr_of_mut!((*dev).managed.lock);
        spin_lock_irqsave_raw(lock, &mut flags);
        list_add(
            addr_of_mut!((*record).link),
            addr_of_mut!((*dev).managed.resources),
        );
        spin_unlock_irqrestore_raw(lock, flags);
    }
    // SAFETY: `record` was just linked into the list and stays live until it is released by the device.
    debug_log(&unsafe { describe("ADD", record) });
}

/// Finds the first record matching `matches` (scanning from the tail when
/// `from_tail` is set, otherwise from the head), unlinks it under the lock, and
/// returns it. Returns null when nothing matches.
///
/// # Safety
/// `dev` must point at a live device with an initialised managed lock and list.
unsafe fn take_record(
    dev: *mut DrmDevice,
    from_tail: bool,
    matches: impl Fn(*mut ManagedRecord) -> bool,
) -> *mut ManagedRecord {
    let mut flags: u64 = 0;
    let mut found = null_mut();
    // SAFETY: `dev` is live; the list is only walked and modified while the managed lock is held, and every node visited is a live record linked into that list.
    unsafe {
        let lock = addr_of_mut!((*dev).managed.lock);
        let head = addr_of_mut!((*dev).managed.resources);
        spin_lock_irqsave_raw(lock, &mut flags);
        let mut cursor = if from_tail {
            (*head).prev
        } else {
            (*head).next
        };
        while cursor != head {
            let record = record_from_link(cursor);
            if matches(record) {
                list_del_init(addr_of_mut!((*record).link));
                found = record;
                break;
            }
            cursor = if from_tail {
                (*cursor).prev
            } else {
                (*cursor).next
            };
        }
        spin_unlock_irqrestore_raw(lock, flags);
    }
    found
}

/// Compares two release callbacks by address; two absent callbacks match.
fn same_action(a: DrmresReleaseT, b: DrmresReleaseT) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => core::ptr::fn_addr_eq(x, y),
        (None, None) => true,
        _ => false,
    }
}

/// Releases every record still attached to `dev`, newest first.
///
/// # Safety
/// `dev` must point at a live device. The caller must ensure no other thread
/// is adding or removing managed resources while teardown runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_managed_release(dev: *mut DrmDevice) {
    debug_log("drmres release begin");
    // SAFETY: `dev` is live per the caller contract; each record is visited once, unlinked before it is freed, and the successor is captured beforehand.
    unsafe {
        let head = addr_of_mut!((*dev).managed.resources);
        let mut cursor = (*head).next;
        while cursor != head {
            let next = (*cursor).next;
            let record = record_from_link(cursor);
            debug_log(&describe("REL", record));
            if let Some(action) = (*record).action {
                action(dev, (*record).data);
            }
            list_del(addr_of_mut!((*record).link));
            free_record(record);
            cursor = next;
        }
    }
    debug_log("drmres release end");
}

/// Records that `container` holds the device, so it is freed with the device.
///
/// # Safety
/// `dev` must point at a live device and `container` at the allocation that
/// embeds it. At most one container may be registered per device.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drmm_add_final_kfree(dev: *mut DrmDevice, container: *mut c_void) {
    // SAFETY: `dev` is live per the caller contract.
    unsafe {
        warn_on(!(*dev).managed.final_kfree.is_null());
        warn_on((dev as usize) < (container as usize));
        (*dev).managed.final_kfree = container;
    }
}

/// Registers `action` to run on device teardown, passing `data`.
///
/// Returns 0 on success or `-ENOMEM` when the record cannot be allocated.
///
/// # Safety
/// `dev` must point at a live device. `data` is passed back to `action`
/// verbatim and must stay valid until the action runs or is cancelled.
/// `name`, when non-null, must be a readable NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drmm_add_action(
    dev: *mut DrmDevice,
    action: DrmresReleaseT,
    data: *mut c_void,
    name: *const c_char,
) -> i32 {
    // SAFETY: `dev` is live and `name` meets the caller contract above; the record is unlinked until `insert_record` publishes it.
    unsafe {
        let record = new_record(action, 0, GFP_KERNEL | GFP_ZERO_FLAG);
        if record.is_null() {
            debug_log(&alloc::format!(
                "failed to add action {} for {:p}",
                if name.is_null() {
                    "(unnamed)"
                } else {
                    CStr::from_ptr(name).to_str().unwrap_or("(invalid)")
                },
                data
            ));
            return -ENOMEM;
        }
        (*record).label = duplicate_label(name);
        (*record).data = data;
        insert_record(dev, record);
    }
    0
}

/// Like [`__drmm_add_action`], but runs `action` immediately when registration
/// fails so that the caller's cleanup still happens.
///
/// # Safety
/// Same requirements as [`__drmm_add_action`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drmm_add_action_or_reset(
    dev: *mut DrmDevice,
    action: DrmresReleaseT,
    data: *mut c_void,
    name: *const c_char,
) -> i32 {
    // SAFETY: the caller upholds the contract of `__drmm_add_action`, which is forwarded unchanged.
    let ret = unsafe { __drmm_add_action(dev, action, data, name) };
    if ret != 0
        && let Some(action) = action
    {
        // SAFETY: registration failed, so the action was never recorded and runs exactly once here, with the caller's data; the caller contract guarantees that data is valid.
        unsafe { action(dev, data) };
    }
    ret
}

/// Cancels a previously registered action and runs it now.
///
/// The most recently registered matching record is chosen. A null `data`
/// matches any record with the same action.
///
/// # Safety
/// `dev` must point at a live device. `data` must be the pointer used at
/// registration (or null) and must remain valid for the callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drmm_release_action(
    dev: *mut DrmDevice,
    action: DrmresReleaseT,
    data: *mut c_void,
) {
    // SAFETY: `dev` is live per the caller contract; the closure only reads records that are linked while the lock is held.
    let record = unsafe {
        take_record(dev, true, |candidate| {
            same_action((*candidate).action, action)
                && (data.is_null() || (*candidate).data == data)
        })
    };
    if warn_on(record.is_null()) {
        return;
    }
    // SAFETY: `record` was unlinked above, so this path is its only owner; the callback receives the caller's `data` as documented.
    unsafe {
        if let Some(action) = action {
            action(dev, data);
        }
        free_record(record);
    }
}

/// Allocates `size` bytes whose lifetime is tied to `dev`. `gfp` controls
/// zeroing through `__GFP_ZERO`. Returns null on failure.
///
/// # Safety
/// `dev` must point at a live device. The memory must not be passed to
/// `kfree`; it is released through [`drmm_kfree`] or on device teardown.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drmm_kmalloc(dev: *mut DrmDevice, size: usize, gfp: u32) -> *mut c_void {
    // SAFETY: the new record is fully owned here until it is linked; its label is a fresh copy and `dev` is live per the caller contract.
    unsafe {
        let record = new_record(None, size, gfp);
        if record.is_null() {
            debug_log(&alloc::format!("failed to allocate {size} bytes"));
            return null_mut();
        }
        (*record).label = duplicate_label(c"kmalloc".as_ptr());
        insert_record(dev, record);
        payload_of(record)
    }
}

/// Duplicates `s` into device-managed memory. Returns null when `s` is null
/// or allocation fails.
///
/// # Safety
/// `dev` must point at a live device and `s`, when non-null, must be a
/// readable NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drmm_kstrdup(
    dev: *mut DrmDevice,
    s: *const c_char,
    gfp: u32,
) -> *mut c_char {
    if s.is_null() {
        return null_mut();
    }
    // SAFETY: `s` is a readable NUL-terminated string per the caller contract, so the byte count includes the terminator and fits the copy.
    unsafe {
        let bytes = CStr::from_ptr(s).to_bytes_with_nul();
        let copy = drmm_kmalloc(dev, bytes.len(), gfp).cast::<u8>();
        if !copy.is_null() {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), copy, bytes.len());
        }
        copy.cast()
    }
}

/// Frees memory obtained from [`drmm_kmalloc`] or [`drmm_kstrdup`] before
/// device teardown. Null is ignored.
///
/// # Safety
/// `dev` must point at a live device and `data` must be a pointer returned by
/// one of those helpers for this device that has not been freed yet.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drmm_kfree(dev: *mut DrmDevice, data: *mut c_void) {
    if data.is_null() {
        return;
    }
    // SAFETY: `dev` is live per the caller contract; the closure compares payload addresses and never reads payload bytes.
    let record = unsafe { take_record(dev, false, |candidate| payload_of(candidate) == data) };
    if warn_on(record.is_null()) {
        return;
    }
    // SAFETY: `record` is unlinked and owned by this call, so it is freed exactly once.
    unsafe { free_record(record) };
}

/// Teardown hook for a managed mutex.
///
/// # Safety
/// `res` must point at a live `Mutex` that is not locked.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drmm_mutex_release(_dev: *mut DrmDevice, res: *mut c_void) {
    // SAFETY: `res` points at a live mutex per the caller contract, and no other reference to it exists during teardown.
    unsafe { mutex_destroy(&mut *res.cast::<Mutex>()) };
}

/// Teardown hook for a managed workqueue.
///
/// # Safety
/// `res` must be a workqueue handle that has not been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drmm_workqueue_release(_device: *mut DrmDevice, res: *mut c_void) {
    // SAFETY: `res` is the workqueue handle registered by the caller and has not been destroyed, per the contract above.
    unsafe { destroy_workqueue(res) };
}
