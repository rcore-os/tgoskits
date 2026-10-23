// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// sync_file: an anonymous file that carries one dma-fence. Userspace merges
// two of them with SYNC_IOC_MERGE and inspects the fences they contain with
// SYNC_IOC_FILE_INFO; SYNC_IOC_SET_DEADLINE forwards a deadline hint.
//
// Descriptor and file-table work (anon inode creation, descriptor reservation,
// fd_install, fget) goes through the `kernel_services` provider. Poll and ioctl
// are typed callbacks in the file's FileOperations table.

use alloc::{format, vec::Vec};
use core::{
    ffi::{CStr, c_char, c_int, c_long, c_uint, c_void},
    mem::{offset_of, size_of},
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{
    config::{ENOTTY, is_err},
    dma_fence::{
        dma_fence_add_callback, dma_fence_driver_name, dma_fence_get, dma_fence_get_status,
        dma_fence_is_signaled, dma_fence_put, dma_fence_remove_callback, dma_fence_set_deadline,
        dma_fence_timeline_name, dma_fence_timestamp,
    },
    dma_fence_unwrap::{
        DmaFenceUnwrap, dma_fence_unwrap_first, dma_fence_unwrap_merge, dma_fence_unwrap_next,
    },
    kernel_services::{anon_inode_getfile, fd_install, fget, get_unused_fd_flags, put_unused_fd},
    list::init_list_head,
    memory::{kfree, kzalloc_obj},
    mm_native::{copy_from_user, copy_to_user},
    shmem::{File, FileOperations, Inode, fput_file},
    types::{DmaFence, DmaFenceCb, WaitQueueHead},
    wait::{PollTable, init_waitqueue_head, poll_wait, wake_up_all},
};

const EFAULT: c_int = 14;
const ENOMEM: c_int = 12;
const ENOENT: c_int = 2;
const EINVAL: c_int = 22;
/// The only poll event a sync_file reports: readable once its fence signaled.
const EPOLLIN: c_uint = 0x001;
const O_CLOEXEC: c_int = 0o2000000;

/// Bit in `SyncFile::flags` that is set once the fence wakeup callback has
/// been armed for pollers.
const POLL_ENABLED: u32 = 0;

/// Kernel-side state of one sync_file. `file` is the first field because the
/// DRM syncobj and driver paths read it to install the descriptor.
#[repr(C)]
pub struct SyncFile {
    pub file: *mut File,
    pub user_name: [c_char; 32],
    pub fence: *mut DmaFence,
    pub cb: DmaFenceCb,
    pub wq: WaitQueueHead,
    pub flags: AtomicUsize,
}

/// File operations shared by every sync_file. Poll and ioctl are dispatched
/// by this module; `release` runs when the last reference is dropped.
pub static SYNC_FILE_FOPS: FileOperations = FileOperations {
    poll: Some(sync_file_poll),
    unlocked_ioctl: Some(sync_file_ioctl),
    release: Some(sync_file_fop_release),
    ..FileOperations::empty()
};

/// Argument of `SYNC_IOC_MERGE`. The caller supplies the second descriptor and
/// a name; the kernel fills in the new descriptor.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SyncMergeData {
    pub name: [u8; 32],
    pub fd2: i32,
    pub fence: i32,
    pub flags: u32,
    pub pad: u32,
}

/// One entry of the member array written by `SYNC_IOC_FILE_INFO`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SyncFenceInfo {
    pub obj_name: [u8; 32],
    pub driver_name: [u8; 32],
    pub status: i32,
    pub flags: u32,
    pub timestamp_ns: u64,
}

/// Argument of `SYNC_IOC_FILE_INFO`. When `num_fences` is non-zero the kernel
/// writes that many [`SyncFenceInfo`] records to `sync_fence_info`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SyncFileInfo {
    pub name: [u8; 32],
    pub status: i32,
    pub flags: u32,
    pub num_fences: u32,
    pub pad: u32,
    pub sync_fence_info: u64,
}

/// Argument of `SYNC_IOC_SET_DEADLINE`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SyncSetDeadline {
    pub deadline_ns: u64,
    pub pad: u64,
}

const SYNC_IOC_MAGIC: u32 = b'>' as u32;

/// Encode an ioctl number from its direction bits, command number and
/// argument size, following the generic `_IOC` layout.
const fn ioctl_number(dir: u32, nr: u32, size: usize) -> u32 {
    (dir << 30) | ((size as u32) << 16) | (SYNC_IOC_MAGIC << 8) | nr
}

const IOC_WRITE: u32 = 1;
const IOC_READ_WRITE: u32 = 3;

pub const SYNC_IOC_MERGE: u32 = ioctl_number(IOC_READ_WRITE, 3, size_of::<SyncMergeData>());
pub const SYNC_IOC_FILE_INFO: u32 = ioctl_number(IOC_READ_WRITE, 4, size_of::<SyncFileInfo>());
pub const SYNC_IOC_SET_DEADLINE: u32 = ioctl_number(IOC_WRITE, 5, size_of::<SyncSetDeadline>());

const _: () = {
    assert!(size_of::<SyncMergeData>() == 48);
    assert!(size_of::<SyncFenceInfo>() == 80);
    assert!(size_of::<SyncFileInfo>() == 56);
    assert!(size_of::<SyncSetDeadline>() == 16);
    assert!(SYNC_IOC_MERGE == 0xc030_3e03);
    assert!(SYNC_IOC_FILE_INFO == 0xc038_3e04);
    assert!(SYNC_IOC_SET_DEADLINE == 0x4010_3e05);
};

/// Whether `file` was produced by [`sync_file_create`].
///
/// # Safety
/// `file` must be null or a live file.
pub unsafe fn is_sync_file_file(file: *const File) -> bool {
    // SAFETY: `file` is live or null, so the null check guards the read.
    !file.is_null() && ptr::eq(unsafe { (*file).f_op }, ptr::addr_of!(SYNC_FILE_FOPS))
}

/// The sync_file stored in a sync_file file's private data.
///
/// # Safety
/// `file` must be a live sync_file file.
pub unsafe fn sync_file_of(file: *mut File) -> *mut SyncFile {
    // SAFETY: the caller guarantees `file` is a live sync_file file, whose
    // private data is the SyncFile allocated by `allocate_sync_file`.
    unsafe { (*file)._private_data.cast() }
}

/// Wait queue woken by `sync_file_poll` and the fence callback. A poll
/// provider adds its own entry here.
///
/// # Safety
/// `file` must be a live sync_file file.
pub unsafe fn sync_file_wait_queue(file: *mut File) -> *mut WaitQueueHead {
    // SAFETY: `sync_file_of` returns the live SyncFile of this file.
    unsafe { ptr::addr_of_mut!((*sync_file_of(file)).wq) }
}

/// Allocate a zeroed SyncFile together with its anonymous file. Returns null
/// when either allocation fails.
/// # Safety
/// Callers must own the returned allocation or release it with `sync_file_release` / `fput`.
unsafe fn allocate_sync_file() -> *mut SyncFile {
    let sync_file = kzalloc_obj::<SyncFile>();
    if sync_file.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `sync_file` is a fresh allocation whose address is handed to the
    // file as its private data; the static name and ops table outlive it.
    let file = unsafe {
        anon_inode_getfile(
            c"sync_file".as_ptr(),
            ptr::addr_of!(SYNC_FILE_FOPS).cast(),
            sync_file.cast(),
            0,
        )
    };
    if is_err(file) {
        // SAFETY: nothing else references the allocation yet.
        unsafe { kfree(sync_file) };
        return ptr::null_mut();
    }
    // SAFETY: `sync_file` is exclusively ours until it is returned.
    unsafe {
        (*sync_file).file = file.cast();
        init_waitqueue_head(&mut (*sync_file).wq);
        init_list_head(ptr::addr_of_mut!((*sync_file).cb.node));
    }
    sync_file
}

/// Fence callback armed by `sync_file_poll`: the fence signaled, so wake the
/// pollers of the owning sync_file.
///
/// # Safety
/// `cb` must be the `cb` field of a live SyncFile. Only `sync_file_poll`
/// registers this callback, and it always passes that field.
unsafe extern "C" fn wake_pollers_on_signal(_fence: *mut DmaFence, cb: *mut DmaFenceCb) {
    // SAFETY: `cb` points at the `cb` field of a live SyncFile, so stepping
    // back by the field offset yields the owning SyncFile.
    let sync_file = unsafe { cb.cast::<u8>().sub(offset_of!(SyncFile, cb)) }.cast::<SyncFile>();
    // SAFETY: the SyncFile is live, and its wait queue is only touched here and
    // in the poll path.
    unsafe { wake_up_all(core::ptr::addr_of_mut!((*sync_file).wq)) };
}

/// Create a sync_file that holds a new reference to `fence`. Release it with
/// `fput` on its `file`. Returns null on allocation failure.
///
/// # Safety
/// `fence` must be a live fence.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sync_file_create(fence: *mut DmaFence) -> *mut SyncFile {
    // SAFETY: plain allocation; no preconditions beyond the module's own.
    let sync_file = unsafe { allocate_sync_file() };
    if !sync_file.is_null() {
        // SAFETY: `sync_file` is exclusively ours; `fence` is live per the caller.
        unsafe { (*sync_file).fence = dma_fence_get(fence) };
    }
    sync_file
}

/// Resolve `fd` to a sync_file file and return an owned reference to it, or
/// null when `fd` is not a sync_file.
/// # Safety
/// The result, when non-null, is an owned reference the caller must drop.
unsafe fn sync_file_fdget(fd: c_int) -> *mut File {
    // SAFETY: `fget` returns an owned reference or null.
    let file = unsafe { fget(fd) };
    if file.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `file` is a live file from `fget`.
    if unsafe { is_sync_file_file(file) } {
        return file;
    }
    // SAFETY: the reference from `fget` is ours to drop on the mismatch path.
    unsafe { fput_file(file) };
    ptr::null_mut()
}

/// Return an owned reference to the fence of the sync_file behind `fd`, or
/// null when `fd` does not name a sync_file.
///
/// # Safety
/// The caller must release the result with `dma_fence_put`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sync_file_get_fence(fd: c_int) -> *mut DmaFence {
    // SAFETY: `sync_file_fdget` yields an owned, verified sync_file file.
    let file = unsafe { sync_file_fdget(fd) };
    if file.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: the file is live and its private data is its SyncFile.
    let fence = unsafe { dma_fence_get((*sync_file_of(file)).fence) };
    // SAFETY: drops the reference `sync_file_fdget` took.
    unsafe { fput_file(file) };
    fence
}

/// Copy `src` into `dst`, stopping at the first NUL byte or when `dst` is
/// full. A non-empty `dst` always ends up NUL-terminated.
fn copy_name(dst: &mut [u8], src: &[u8]) {
    let Some(room) = dst.len().checked_sub(1) else {
        return;
    };
    let len = src
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(src.len())
        .min(room);
    dst[..len].copy_from_slice(&src[..len]);
    dst[len] = 0;
}

/// Borrow a NUL-terminated name returned by a fence operation as `&str`.
/// Invalid UTF-8 yields an empty string.
///
/// # Safety
/// `name` must point at a NUL-terminated string that outlives the result.
unsafe fn fence_label<'a>(name: *const c_char) -> &'a str {
    // SAFETY: the caller guarantees `name` is a valid NUL-terminated string.
    unsafe { CStr::from_ptr(name) }.to_str().unwrap_or("")
}

/// Write the name of `sync_file` into `buf`. A name given at merge time wins;
/// otherwise the name is `<driver>-<timeline><context>-<seqno>` of the fence.
///
/// # Safety
/// `sync_file` must be live, and `buf` must be writable for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sync_file_get_name(
    sync_file: *mut SyncFile,
    buf: *mut c_char,
    len: c_int,
) -> *mut c_char {
    let Ok(len) = usize::try_from(len) else {
        return buf;
    };
    if len == 0 {
        return buf;
    }
    // SAFETY: the caller guarantees `buf` is writable for `len` bytes.
    let dst = unsafe { core::slice::from_raw_parts_mut(buf.cast::<u8>(), len) };
    // SAFETY: `sync_file` is live per the caller.
    let user_name = unsafe { &(*sync_file).user_name };
    if user_name[0] != 0 {
        // SAFETY: `[c_char; 32]` and `[u8; 32]` have identical layout, and the
        // slice stays inside the field.
        let bytes = unsafe {
            core::slice::from_raw_parts(user_name.as_ptr().cast::<u8>(), user_name.len())
        };
        copy_name(dst, bytes);
    } else {
        // SAFETY: the fence of a live sync_file is live.
        let fence = unsafe { (*sync_file).fence };
        // SAFETY: fence ops return NUL-terminated names for live fences.
        let driver = unsafe { fence_label(dma_fence_driver_name(fence)) };
        // SAFETY: as above.
        let timeline = unsafe { fence_label(dma_fence_timeline_name(fence)) };
        // SAFETY: plain field reads of a live fence.
        let (context, seqno) = unsafe { ((*fence).context, (*fence).seqno as i64) };
        copy_name(
            dst,
            format!("{driver}-{timeline}{context}-{seqno}").as_bytes(),
        );
    }
    buf
}

/// Build a new sync_file that waits on the pending fences of `a` and `b`,
/// named `name`. The inputs are not consumed. Returns null on failure.
///
/// # Safety
/// `a` and `b` must be live sync_files.
pub unsafe fn sync_file_merge(name: &[u8], a: *mut SyncFile, b: *mut SyncFile) -> *mut SyncFile {
    // SAFETY: plain allocation.
    let sync_file = unsafe { allocate_sync_file() };
    if sync_file.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `a` and `b` are live per the caller.
    let inputs = unsafe { [(*a).fence, (*b).fence] };
    // SAFETY: both fences are live; the merge borrows them.
    let merged = unsafe { dma_fence_unwrap_merge(&inputs) };
    if merged.is_null() {
        // SAFETY: the new sync_file is unreachable; dropping its file frees it.
        unsafe { fput_file((*sync_file).file) };
        return ptr::null_mut();
    }
    // SAFETY: `sync_file` is exclusively ours; `merged` is an owned reference
    // that the sync_file keeps.
    unsafe {
        (*sync_file).fence = merged;
        let dst =
            core::slice::from_raw_parts_mut((*sync_file).user_name.as_mut_ptr().cast::<u8>(), 32);
        copy_name(dst, name);
    }
    sync_file
}

/// Release a sync_file whose last file reference is being dropped.
///
/// # Safety
/// `file` must be a sync_file file whose last reference is being dropped.
pub unsafe fn sync_file_release(file: *mut File) {
    // SAFETY: the file is live, so its SyncFile is too.
    let sync_file = unsafe { sync_file_of(file) };
    // SAFETY: the last reference is being dropped, so nobody else can reach
    // the SyncFile while it is torn down.
    unsafe {
        if (*sync_file).flags.load(Ordering::Acquire) & (1 << POLL_ENABLED) != 0 {
            dma_fence_remove_callback((*sync_file).fence, ptr::addr_of_mut!((*sync_file).cb));
        }
        dma_fence_put((*sync_file).fence);
        kfree(sync_file);
    }
}

/// `release` entry of [`SYNC_FILE_FOPS`].
/// # Safety
/// `file` must be a sync_file file whose last reference is being dropped.
unsafe extern "C" fn sync_file_fop_release(_inode: *mut Inode, file: *mut File) -> c_int {
    // SAFETY: the VFS calls this only on the last reference to a sync_file.
    unsafe { sync_file_release(file) };
    0
}

/// Poll entry of [`SYNC_FILE_FOPS`]. Registers the caller on the wait queue,
/// arms the fence wakeup on first use, and returns `EPOLLIN` once the fence
/// has signaled.
///
/// # Safety
/// `file` must be a live sync_file file; `pt` must be null or live.
pub unsafe extern "C" fn sync_file_poll(file: *mut File, pt: *mut PollTable) -> c_uint {
    // SAFETY: the file is live, so its SyncFile is too.
    let sync_file = unsafe { sync_file_of(file) };
    // SAFETY: the wait queue lives inside the live SyncFile.
    unsafe { poll_wait(file.cast(), ptr::addr_of_mut!((*sync_file).wq), pt) };
    // The atomic flag alone elects the callback registrar. Reading cb.node
    // here would race the fence callback-list mutation under its own lock.
    {
        let bit: usize = 1 << POLL_ENABLED;
        // SAFETY: The file reference retains this real atomic registration flag.
        let flags = unsafe { &(*sync_file).flags };
        // Only the first poller arms the callback.
        if flags.fetch_or(bit, Ordering::AcqRel) & bit == 0 {
            // SAFETY: the fence is live, and `cb` is registered once and owned
            // by this SyncFile.
            let armed = unsafe {
                dma_fence_add_callback(
                    (*sync_file).fence,
                    ptr::addr_of_mut!((*sync_file).cb),
                    wake_pollers_on_signal,
                )
            };
            // A negative result means the fence already signaled, so wake now.
            if armed < 0 {
                // SAFETY: the wait queue lives inside the live SyncFile.
                unsafe { wake_up_all(core::ptr::addr_of_mut!((*sync_file).wq)) };
            }
        }
    }
    // SAFETY: the fence is live for the lifetime of the SyncFile.
    if unsafe { dma_fence_is_signaled((*sync_file).fence) } {
        EPOLLIN
    } else {
        0
    }
}

/// `SYNC_IOC_MERGE`: create a descriptor for the merge of `sync_file` and the
/// descriptor named in the request.
/// # Safety
/// `sync_file` must be live; `arg` is a userspace address.
unsafe fn ioctl_merge(sync_file: *mut SyncFile, arg: usize) -> c_long {
    // SAFETY: descriptor reservation has no memory preconditions.
    let fd = unsafe { get_unused_fd_flags(O_CLOEXEC) };
    if fd < 0 {
        return fd as c_long;
    }
    let mut request = SyncMergeData::default();
    // SAFETY: `request` is a local of the right size; `arg` is the user pointer.
    if unsafe {
        copy_from_user(
            ptr::addr_of_mut!(request).cast(),
            arg as *const c_void,
            size_of::<SyncMergeData>(),
        )
    } != 0
    {
        // SAFETY: `fd` was reserved above and is not installed yet.
        unsafe { put_unused_fd(fd) };
        return -(EFAULT as c_long);
    }
    if request.flags != 0 || request.pad != 0 {
        // SAFETY: as above.
        unsafe { put_unused_fd(fd) };
        return -(EINVAL as c_long);
    }
    // SAFETY: resolves a user descriptor to an owned sync_file reference.
    let other = unsafe { sync_file_fdget(request.fd2) };
    if other.is_null() {
        // SAFETY: as above.
        unsafe { put_unused_fd(fd) };
        return -(ENOENT as c_long);
    }
    // SAFETY: `sync_file` and the other file's SyncFile are live.
    let merged = unsafe { sync_file_merge(&request.name, sync_file, sync_file_of(other)) };
    if merged.is_null() {
        // SAFETY: release the reference and descriptor taken above.
        unsafe {
            fput_file(other);
            put_unused_fd(fd);
        }
        return -(ENOMEM as c_long);
    }
    request.fence = fd;
    // SAFETY: `request` is a local of the right size; `arg` is the user pointer.
    if unsafe {
        copy_to_user(
            arg as *mut c_void,
            ptr::addr_of!(request).cast(),
            size_of::<SyncMergeData>(),
        )
    } != 0
    {
        // SAFETY: undo every reference and reservation made on this path.
        unsafe {
            fput_file((*merged).file);
            fput_file(other);
            put_unused_fd(fd);
        }
        return -(EFAULT as c_long);
    }
    // SAFETY: installs the merged file under the reserved descriptor, which
    // takes over the reference; the other file's reference is then dropped.
    unsafe {
        fd_install(fd, (*merged).file.cast());
        fput_file(other);
    }
    0
}

/// Number of leaf fences reachable from `head`.
/// # Safety
/// `head` must be null or live.
unsafe fn count_leaves(head: *mut DmaFence) -> u32 {
    let mut cursor = DmaFenceUnwrap::empty();
    let mut count = 0u32;
    // SAFETY: `head` is live per the caller.
    let mut leaf = unsafe { dma_fence_unwrap_first(head, &mut cursor) };
    while !leaf.is_null() {
        count += 1;
        // SAFETY: the cursor was started above and has not finished.
        leaf = unsafe { dma_fence_unwrap_next(&mut cursor) };
    }
    count
}

/// Describe one leaf fence in `info` and return its status.
///
/// # Safety
/// `fence` must be live.
unsafe fn describe_leaf(fence: *mut DmaFence, info: &mut SyncFenceInfo) -> i32 {
    // SAFETY: the fence is live; its ops return NUL-terminated names.
    let timeline = unsafe { fence_label(dma_fence_timeline_name(fence)) };
    // SAFETY: as above.
    let driver = unsafe { fence_label(dma_fence_driver_name(fence)) };
    copy_name(&mut info.obj_name, timeline.as_bytes());
    copy_name(&mut info.driver_name, driver.as_bytes());
    // SAFETY: the fence is live.
    let status = unsafe { dma_fence_get_status(fence) };
    info.status = status;
    // SAFETY: as above.
    info.timestamp_ns = if unsafe { dma_fence_is_signaled(fence) } {
        unsafe { dma_fence_timestamp(fence) as u64 }
    } else {
        0
    };
    status
}

/// Result of a `SYNC_IOC_FILE_INFO` query, ready to be copied to userspace.
pub struct FileInfo {
    pub status: i32,
    pub num_fences: u32,
    pub name: [u8; 32],
    /// Filled only when the caller asked for members (`num_fences != 0`).
    pub fences: Vec<SyncFenceInfo>,
}

/// Build the answer to a `SYNC_IOC_FILE_INFO` query. `requested` is the
/// caller's `num_fences`: zero asks only for the summary, and a non-zero value
/// must be at least the member count.
///
/// # Safety
/// `sync_file` must be live.
pub unsafe fn sync_file_fence_info(
    sync_file: *mut SyncFile,
    requested: u32,
) -> Result<FileInfo, c_long> {
    // SAFETY: `sync_file` is live per the caller.
    let fence = unsafe { (*sync_file).fence };
    // SAFETY: the fence is live.
    let num_fences = unsafe { count_leaves(fence) };
    let mut name = [0u8; 32];
    // SAFETY: `name` is a local of the advertised size.
    unsafe { sync_file_get_name(sync_file, name.as_mut_ptr().cast(), name.len() as c_int) };
    if requested == 0 {
        // SAFETY: the fence is live.
        let status = unsafe { dma_fence_get_status(fence) };
        return Ok(FileInfo {
            status,
            num_fences,
            name,
            fences: Vec::new(),
        });
    }
    if requested < num_fences {
        return Err(-(EINVAL as c_long));
    }
    let mut fences: Vec<SyncFenceInfo> = Vec::new();
    if fences.try_reserve_exact(num_fences as usize).is_err() {
        return Err(-(ENOMEM as c_long));
    }
    fences.resize(num_fences as usize, SyncFenceInfo::default());

    // Summary status: starts at signaled, and the first non-positive member
    // status is kept.
    let mut status = 1;
    let mut slot = 0usize;
    let mut cursor = DmaFenceUnwrap::empty();
    // SAFETY: the fence is live.
    let mut leaf = unsafe { dma_fence_unwrap_first(fence, &mut cursor) };
    while !leaf.is_null() {
        if let Some(entry) = fences.get_mut(slot) {
            // SAFETY: the leaf comes from a started cursor, so it is live.
            let member_status = unsafe { describe_leaf(leaf, entry) };
            if status > 0 {
                status = member_status;
            }
            slot += 1;
        }
        // SAFETY: the cursor was started above and has not finished.
        leaf = unsafe { dma_fence_unwrap_next(&mut cursor) };
    }
    Ok(FileInfo {
        status,
        num_fences,
        name,
        fences,
    })
}

/// `SYNC_IOC_FILE_INFO`: copy the summary and, when requested, the members out
/// to the caller.
/// # Safety
/// `sync_file` must be live; `arg` is a userspace address.
unsafe fn ioctl_file_info(sync_file: *mut SyncFile, arg: usize) -> c_long {
    let mut info = SyncFileInfo::default();
    // SAFETY: `info` is a local of the right size; `arg` is the user pointer.
    if unsafe {
        copy_from_user(
            ptr::addr_of_mut!(info).cast(),
            arg as *const c_void,
            size_of::<SyncFileInfo>(),
        )
    } != 0
    {
        return -(EFAULT as c_long);
    }
    if info.flags != 0 || info.pad != 0 {
        return -(EINVAL as c_long);
    }
    let report = match unsafe { sync_file_fence_info(sync_file, info.num_fences) } {
        Ok(report) => report,
        Err(error) => return error,
    };
    if !report.fences.is_empty() {
        // SAFETY: the buffer holds `report.fences` records; the user pointer is
        // checked by `copy_to_user`.
        if unsafe {
            copy_to_user(
                info.sync_fence_info as *mut c_void,
                report.fences.as_ptr().cast(),
                report.fences.len() * size_of::<SyncFenceInfo>(),
            )
        } != 0
        {
            return -(EFAULT as c_long);
        }
    }
    info.status = report.status;
    info.name = report.name;
    info.num_fences = report.num_fences;
    // SAFETY: `info` is a local of the right size; `arg` is the user pointer.
    if unsafe {
        copy_to_user(
            arg as *mut c_void,
            ptr::addr_of!(info).cast(),
            size_of::<SyncFileInfo>(),
        )
    } != 0
    {
        return -(EFAULT as c_long);
    }
    0
}

/// `SYNC_IOC_SET_DEADLINE`: forward the deadline hint to the fence.
/// # Safety
/// `sync_file` must be live; `arg` is a userspace address.
unsafe fn ioctl_set_deadline(sync_file: *mut SyncFile, arg: usize) -> c_long {
    let mut request = SyncSetDeadline::default();
    // SAFETY: `request` is a local of the right size; `arg` is the user pointer.
    if unsafe {
        copy_from_user(
            ptr::addr_of_mut!(request).cast(),
            arg as *const c_void,
            size_of::<SyncSetDeadline>(),
        )
    } != 0
    {
        return -(EFAULT as c_long);
    }
    if request.pad != 0 {
        return -(EINVAL as c_long);
    }
    // SAFETY: the fence of a live sync_file is live.
    unsafe { dma_fence_set_deadline((*sync_file).fence, request.deadline_ns as i64) };
    0
}

/// Ioctl entry of [`SYNC_FILE_FOPS`]. Returns the command's result, or a
/// negative errno.
///
/// # Safety
/// `file` must be a live sync_file file. `arg` is treated as a user pointer by
/// the commands that take one.
pub unsafe extern "C" fn sync_file_ioctl(file: *mut File, cmd: u32, arg: usize) -> c_long {
    // SAFETY: the file is live, so its SyncFile is too.
    let sync_file = unsafe { sync_file_of(file) };
    match cmd {
        // SAFETY: the sync_file is live; `arg` is validated by the copy helpers.
        SYNC_IOC_MERGE => unsafe { ioctl_merge(sync_file, arg) },
        // SAFETY: as above.
        SYNC_IOC_FILE_INFO => unsafe { ioctl_file_info(sync_file, arg) },
        // SAFETY: as above.
        SYNC_IOC_SET_DEADLINE => unsafe { ioctl_set_deadline(sync_file, arg) },
        _ => -(ENOTTY as c_long),
    }
}

#[cfg(test)]
mod tests {
    // Host tests keep the fences they create: the final `dma_fence_put` runs
    // the RCU release, which needs the task scheduler these tests do not start.
    use core::{ffi::c_void, ptr};

    use super::*;
    use crate::{
        dma_fence::{dma_fence_init, dma_fence_signal},
        dma_fence_unwrap::dma_fence_dedup_array,
        kernel_services::{FileOps, install_file_ops},
        shmem::{fget_file, shmem_file_setup},
        types::{DmaFenceOps, Spinlock},
    };

    const FD_BASE: c_int = 300;

    /// Fake descriptor table: `fd_install` takes the file's reference.
    static FD_TABLE: spin::Mutex<Vec<(c_int, usize)>> = spin::Mutex::new(Vec::new());
    static NEXT_FD: AtomicUsize = AtomicUsize::new(0);

    /// # Safety
    /// Test-only shim: the arguments follow the anon-inode contract.
    unsafe extern "C" fn test_anon_inode_getfile(
        name: *const c_char,
        fops: *const c_void,
        private_data: *mut c_void,
        flags: c_int,
    ) -> *mut File {
        let file = unsafe { shmem_file_setup(name, 0, 0) };
        unsafe {
            (*file).f_op = fops.cast();
            (*file)._private_data = private_data;
            (*file).f_flags = flags as u32;
        }
        file
    }

    /// # Safety
    /// Test-only shim with no pointer arguments.
    unsafe extern "C" fn test_get_unused_fd_flags(_flags: c_int) -> c_int {
        FD_BASE + NEXT_FD.fetch_add(1, Ordering::Relaxed) as c_int
    }

    /// # Safety
    /// Test-only shim with no pointer arguments.
    unsafe extern "C" fn test_put_unused_fd(_fd: c_int) {}

    /// # Safety
    /// `file` must be a live file; the table takes the reference.
    unsafe extern "C" fn test_fd_install(fd: c_int, file: *mut File) {
        FD_TABLE.lock().push((fd, file as usize));
    }

    /// # Safety
    /// Entries in the test table must refer to live files.
    unsafe extern "C" fn test_fget(fd: c_int) -> *mut File {
        let table = FD_TABLE.lock();
        match table.iter().find(|(entry, _)| *entry == fd) {
            // Take the reference the caller will drop with `fput_file`.
            Some((_, file)) => unsafe { fget_file(*file as *mut File) },
            None => ptr::null_mut(),
        }
    }

    static TEST_FILE_OPS: FileOps = FileOps {
        anon_inode_getfile: test_anon_inode_getfile,
        get_unused_fd_flags: test_get_unused_fd_flags,
        put_unused_fd: test_put_unused_fd,
        fd_install: test_fd_install,
        fget: test_fget,
    };

    fn install_test_files() {
        // Idempotent: a repeated install of the same table is accepted.
        let _ = install_file_ops(&TEST_FILE_OPS);
    }

    /// Close a test descriptor: drop the table's reference.
    fn close_fd(fd: c_int) {
        let mut table = FD_TABLE.lock();
        if let Some(index) = table.iter().position(|(entry, _)| *entry == fd) {
            let (_, file) = table.remove(index);
            unsafe { fput_file(file as *mut File) };
        }
    }

    /// # Safety
    /// `fence` is unused by the test driver.
    unsafe extern "C" fn test_fence_driver(_: *mut DmaFence) -> *const c_char {
        c"test-driver".as_ptr()
    }

    static TEST_FENCE_OPS: DmaFenceOps = DmaFenceOps {
        get_driver_name: Some(test_fence_driver),
        get_timeline_name: Some(test_fence_driver),
        enable_signaling: None,
        signaled: None,
        wait: None,
        release: None,
        set_deadline: None,
    };

    /// A pending fence in its own context, released with `dma_fence_put`.
    /// # Safety
    /// The returned fence must be released with `dma_fence_put`.
    unsafe fn pending_fence(context: u64, seqno: u64) -> *mut DmaFence {
        let fence = kzalloc_obj::<DmaFence>();
        let lock: &'static mut Spinlock =
            alloc::boxed::Box::leak(alloc::boxed::Box::new(Spinlock::new()));
        unsafe {
            dma_fence_init(fence, ptr::addr_of!(TEST_FENCE_OPS), lock, context, seqno);
        }
        fence
    }

    /// Stub fence signaled at `timestamp` in the unbound context.
    /// # Safety
    /// The returned fence must be released with `dma_fence_put`.
    unsafe fn signaled_fence(timestamp: i64) -> *mut DmaFence {
        unsafe { crate::dma_fence::dma_fence_allocate_private_stub(timestamp) }
    }

    #[test]
    fn sync_file_ioctl_numbers_match_the_uapi_header() {
        assert_eq!(SYNC_IOC_MERGE, 0xc030_3e03);
        assert_eq!(SYNC_IOC_FILE_INFO, 0xc038_3e04);
        assert_eq!(SYNC_IOC_SET_DEADLINE, 0x4010_3e05);
    }

    #[test]
    fn sync_file_create_fd_install_and_get_fence_round_trip() {
        install_test_files();
        unsafe {
            let fence = signaled_fence(7);
            let sync_file = sync_file_create(fence);
            assert!(!sync_file.is_null());
            let fd = get_unused_fd_flags(O_CLOEXEC);
            fd_install(fd, (*sync_file).file.cast());
            assert!(is_sync_file_file((*sync_file).file));
            // The fence the caller gets back is the one the sync_file holds.
            let got = sync_file_get_fence(fd);
            assert_eq!(got, fence);
            dma_fence_put(got);
            // A descriptor that is not a sync_file has no fence.
            assert!(sync_file_get_fence(FD_BASE + 99).is_null());
            close_fd(fd);
        }
    }

    #[test]
    fn sync_file_get_name_uses_user_name_or_the_fence_identity() {
        install_test_files();
        unsafe {
            let fence = pending_fence(42, 9);
            let sync_file = sync_file_create(fence);
            let mut buf = [0 as c_char; 64];
            sync_file_get_name(sync_file, buf.as_mut_ptr(), buf.len() as c_int);
            assert_eq!(
                CStr::from_ptr(buf.as_ptr()).to_bytes(),
                b"test-driver-test-driver42-9"
            );

            let mut short = [0 as c_char; 8];
            sync_file_get_name(sync_file, short.as_mut_ptr(), short.len() as c_int);
            assert_eq!(CStr::from_ptr(short.as_ptr()).to_bytes(), b"test-dr");

            let merged = sync_file_merge(b"merged-name", sync_file, sync_file);
            assert!(!merged.is_null());
            sync_file_get_name(merged, buf.as_mut_ptr(), buf.len() as c_int);
            assert_eq!(CStr::from_ptr(buf.as_ptr()).to_bytes(), b"merged-name");
            fput_file((*merged).file);
            fput_file((*sync_file).file);
        }
    }

    #[test]
    fn sync_file_merge_keeps_pending_fence_and_drops_signaled_members() {
        install_test_files();
        unsafe {
            let pending = pending_fence(5, 1);
            let done = signaled_fence(1);
            let a = sync_file_create(pending);
            let b = sync_file_create(done);
            // One pending member: the merge returns that fence itself.
            let merged = sync_file_merge(b"", a, b);
            assert!(!merged.is_null());
            assert_eq!((*merged).fence, pending);
            assert!(!dma_fence_is_signaled((*merged).fence));
            fput_file((*merged).file);
            fput_file((*b).file);
            fput_file((*a).file);
        }
    }

    #[test]
    fn sync_file_merge_dedup_keeps_the_most_recent_fence_per_context() {
        unsafe {
            // Each array slot owns the reference the test created.
            let older = pending_fence(5, 1);
            let newer = pending_fence(5, 3);
            let mut fences = [dma_fence_get(older), dma_fence_get(newer)];
            assert_eq!(dma_fence_dedup_array(&mut fences), 1);
            assert_eq!(fences[0], newer);
        }
    }

    #[test]
    fn sync_file_poll_reports_readable_only_after_the_fence_signals() {
        install_test_files();
        unsafe {
            let fence = pending_fence(77, 1);
            let sync_file = sync_file_create(fence);
            let file = (*sync_file).file;
            let poll = SYNC_FILE_FOPS.poll.unwrap();
            assert_eq!(poll(file, ptr::null_mut()), 0);
            dma_fence_signal(fence);
            assert_eq!(poll(file, ptr::null_mut()), EPOLLIN);
            fput_file(file);
        }
    }

    #[test]
    fn sync_file_file_info_reports_status_and_members() {
        install_test_files();
        unsafe {
            let pending = pending_fence(11, 2);
            let done = signaled_fence(5);
            let a = sync_file_create(pending);
            let b = sync_file_create(done);
            let merged = sync_file_merge(b"pair", a, b);
            assert!(!merged.is_null());

            // Exercise SYNC_IOC_FILE_INFO through the typed FileOperations
            // callback used by the live file; detailed results are asserted
            // below through the same info helper without host-MM usercopy.
            let ioctl = SYNC_FILE_FOPS.unlocked_ioctl.unwrap();
            let mut ioctl_info = SyncFileInfo::default();
            assert_eq!(
                ioctl(
                    (*merged).file,
                    SYNC_IOC_FILE_INFO,
                    ptr::addr_of_mut!(ioctl_info) as usize,
                ),
                0
            );

            // Count-only query: no members copied, pending status.
            let count = sync_file_fence_info(merged, 0).ok().unwrap();
            assert_eq!(count.status, 0);
            assert_eq!(count.num_fences, 1);
            assert!(count.fences.is_empty());
            assert_eq!(&count.name[..5], b"pair\0");

            // Member query: the one pending member, reporting its driver.
            let info = sync_file_fence_info(merged, 1).ok().unwrap();
            assert_eq!(info.num_fences, 1);
            assert_eq!(info.fences.len(), 1);
            assert_eq!(&info.fences[0].driver_name[..11], b"test-driver");
            assert_eq!(info.fences[0].status, 0);
            assert_eq!(info.fences[0].timestamp_ns, 0);

            // A signaled sync_file reports its status and timestamp.
            let finished = sync_file_create(done);
            let finished_info = sync_file_fence_info(finished, 1).ok().unwrap();
            assert_eq!(finished_info.status, 1);
            assert_eq!(finished_info.fences[0].timestamp_ns, 5);

            fput_file((*finished).file);
            fput_file((*merged).file);
            fput_file((*b).file);
            fput_file((*a).file);
        }
    }
}
