// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Narrow OS-owned services used by the portable LinuxKPI DRM core.
//!
//! The provider tables contain process credentials/queries, LinuxKPI file
//! descriptors, and eventfd operations. Scheduler services live in
//! `task_runtime`; hardware, memory mapping, and other kernel policy stay
//! outside this component.

use core::{
    ffi::{c_char, c_int, c_void},
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

use crate::{
    config::{ENODEV, err_ptr},
    shmem::File,
};

/// Install-once slot for one cross-thread provider table.
///
/// Non-`Sync` tables cannot be published through the safe interface:
/// ```compile_fail
/// use core::cell::Cell;
/// use drm_kapi::kernel_services::ProviderSlot;
/// let _ = ProviderSlot::<Cell<u32>>::new();
/// ```
pub struct ProviderSlot<T: Sync + 'static> {
    table: AtomicPtr<T>,
}

impl<T: Sync + 'static> ProviderSlot<T> {
    pub const fn new() -> Self {
        Self {
            table: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// Install the table. Reinstalling the same table is idempotent; a
    /// different table is refused.
    pub fn install(&self, table: &'static T) -> Result<(), &'static str> {
        let candidate = (table as *const T).cast_mut();
        match self.table.compare_exchange(
            ptr::null_mut(),
            candidate,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(existing) if existing == candidate => Ok(()),
            Err(_) => Err("kernel provider table is already installed"),
        }
    }

    pub fn get(&self) -> Option<&'static T> {
        // SAFETY: Only references with `'static` lifetime and proper alignment are stored via
        // `install`. The pointer is either null or points to an immutable, valid `T` that lives
        // for the entire program lifetime without mutable aliasing.
        unsafe { self.table.load(Ordering::Acquire).as_ref() }
    }

    pub fn require(&self, what: &str) -> &'static T {
        self.get()
            .unwrap_or_else(|| panic!("{} provider is not installed", what))
    }
}

impl<T: Sync + 'static> Default for ProviderSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Current-task data and credential checks exposed by the hosting kernel.
#[repr(C)]
pub struct TaskOps {
    /// Copy the fixed-size Linux `TASK_COMM_LEN` name of the calling task.
    /// The provider must NUL-pad the 16-byte destination.
    pub copy_current_comm: unsafe extern "C" fn(buf: *mut c_char),
    /// `task_pid_vnr(current)`: PID of the calling task in its virtual PID namespace.
    pub current_pid_vnr: unsafe extern "C" fn() -> c_int,
    /// `capable(cap)` for the calling task.
    pub capable: unsafe extern "C" fn(capability: c_int) -> bool,
    /// `perfmon_capable()` for the calling task.
    pub perfmon_capable: unsafe extern "C" fn() -> bool,
}

/// Linux file-descriptor operations needed by DRM and LinuxKPI.
#[repr(C)]
pub struct FileOps {
    /// Linux `anon_inode_getfile()`: a new LinuxKPI shmem file, or an
    /// `err_ptr` on failure. Ownership of the returned file belongs to the
    /// caller until it is transferred to `fd_install` or released with
    /// `shmem::fput_file`.
    pub anon_inode_getfile: unsafe extern "C" fn(
        name: *const c_char,
        fops: *const c_void,
        private_data: *mut c_void,
        flags: c_int,
    ) -> *mut File,
    /// Linux `get_unused_fd_flags()`: a descriptor, or a negative errno.
    pub get_unused_fd_flags: unsafe extern "C" fn(flags: c_int) -> c_int,
    pub put_unused_fd: unsafe extern "C" fn(fd: c_int),
    /// Transfer the caller's owned LinuxKPI shmem-file reference into `fd`.
    pub fd_install: unsafe extern "C" fn(fd: c_int, file: *mut File),
    /// Linux `fget()`: a new reference to the LinuxKPI shmem file behind `fd`,
    /// or null when `fd` is not open or is not backed by such a file. The
    /// caller must release the returned reference with `shmem::fput_file`.
    pub fget: unsafe extern "C" fn(fd: c_int) -> *mut File,
}

/// Eventfd context operations used for DRM syncobj notifications.
#[repr(C)]
pub struct EventfdOps {
    /// Linux `eventfd_ctx_fdget()`: referenced context or `err_ptr(-errno)`.
    pub ctx_fdget: unsafe extern "C" fn(fd: c_int) -> *mut c_void,
    /// Linux eventfd signal operation. `count` is added to the counter.
    pub signal: unsafe extern "C" fn(ctx: *mut c_void, count: u64) -> c_int,
    /// Drop the reference returned by `ctx_fdget()`.
    pub ctx_put: unsafe extern "C" fn(ctx: *mut c_void),
}

pub static TASK: ProviderSlot<TaskOps> = ProviderSlot::new();
pub static FILES: ProviderSlot<FileOps> = ProviderSlot::new();
pub static EVENTFD: ProviderSlot<EventfdOps> = ProviderSlot::new();

// ---------------------------------------------------------------------------
// Task identity and credentials
// ---------------------------------------------------------------------------

/// Copy the calling task's Linux `comm` field into a caller-owned buffer.
///
/// # Safety
/// `buf` must point to at least 16 writable bytes.
/// The installed task provider must remain live and obey the `TaskOps`
/// callback contracts.
#[unsafe(export_name = "copy_current_comm")]
pub unsafe extern "C" fn copy_current_comm(buf: *mut c_char) {
    // SAFETY: `buf` satisfies the caller precondition (at least 16 writable bytes).
    // The installed `TaskOps` provider is `'static` and its `copy_current_comm` function pointer is valid.
    unsafe { (TASK.require("task").copy_current_comm)(buf) };
}

/// Linux `task_pid_vnr(current)` for the calling task.
///
/// # Safety
/// The installed task provider must remain live and obey the `TaskOps`
/// callback contract.
#[unsafe(export_name = "current_pid_vnr")]
pub unsafe extern "C" fn current_pid_vnr() -> c_int {
    // SAFETY: The installed `TaskOps` table is `'static` and `current_pid_vnr` is a valid C function pointer.
    unsafe { (TASK.require("task").current_pid_vnr)() }
}

/// Linux `capable(cap)`. Denies when no credential provider is installed.
///
/// # Safety
/// Any installed task provider must remain live and obey the `TaskOps`
/// callback contract.
#[unsafe(export_name = "capable")]
pub unsafe extern "C" fn capable(capability: c_int) -> bool {
    TASK.get().is_some_and(|ops| {
        // SAFETY: `ops` is an installed static `TaskOps` table whose `capable` function pointer is valid.
        unsafe { (ops.capable)(capability) }
    })
}

/// Linux `perfmon_capable()`. Denies when no credential provider is installed.
///
/// # Safety
/// Any installed task provider must remain live and obey the `TaskOps`
/// callback contract.
#[unsafe(export_name = "perfmon_capable")]
pub unsafe extern "C" fn perfmon_capable() -> bool {
    TASK.get().is_some_and(|ops| {
        // SAFETY: `ops` is an installed static `TaskOps` table whose `perfmon_capable` function pointer is valid.
        unsafe { (ops.perfmon_capable)() }
    })
}

/// Return the hosting kernel's current task identity, if one is available.
pub fn current_task_id() -> Option<u64> {
    crate::task_runtime::current_task_id()
}

// ---------------------------------------------------------------------------
// LinuxKPI file descriptors
// ---------------------------------------------------------------------------

/// Linux `anon_inode_getfile()`. Without a file table the result is
/// `err_ptr(-ENODEV)`, which callers check with `is_err`.
///
/// # Safety
/// `name` must be a valid NUL-terminated string and `fops` a live file-ops
/// table accepted by the provider. `private_data` must remain valid for the
/// returned file's lifetime.
#[unsafe(export_name = "anon_inode_getfile")]
pub unsafe extern "C" fn anon_inode_getfile(
    name: *const c_char,
    fops: *const c_void,
    private_data: *mut c_void,
    flags: c_int,
) -> *mut File {
    match FILES.get() {
        Some(ops) => {
            // SAFETY: Caller guarantees `name` is a valid NUL-terminated string, `fops` is a valid
            // file-ops table, and `private_data` remains valid for the returned file lifetime.
            unsafe { (ops.anon_inode_getfile)(name, fops, private_data, flags) }
        }
        None => err_ptr(-ENODEV),
    }
}

/// Linux `get_unused_fd_flags(flags)`. Without a file table: `-ENODEV`.
///
/// # Safety
/// `flags` must contain only file-descriptor allocation flags supported by
/// the installed provider.
#[unsafe(export_name = "get_unused_fd_flags")]
pub unsafe extern "C" fn get_unused_fd_flags(flags: c_int) -> c_int {
    match FILES.get() {
        Some(ops) => {
            // SAFETY: Caller guarantees `flags` contains valid descriptor flags; `ops` is a valid static table.
            unsafe { (ops.get_unused_fd_flags)(flags) }
        }
        None => -ENODEV,
    }
}

/// Linux `put_unused_fd(fd)`.
///
/// # Safety
/// `fd` must be a live unused descriptor previously allocated from the file
/// table and not yet installed or released.
#[unsafe(export_name = "put_unused_fd")]
pub unsafe extern "C" fn put_unused_fd(fd: c_int) {
    // SAFETY: Caller guarantees `fd` is a live unused descriptor previously allocated and not yet released.
    // `ops` is a valid static file table.
    unsafe { (FILES.require("file table").put_unused_fd)(fd) };
}

/// Linux `fget(fd)`. Without a file table no descriptor resolves: null.
///
/// # Safety
/// A returned reference must be dropped with `shmem::fput_file`.
#[unsafe(export_name = "fget")]
pub unsafe extern "C" fn fget(fd: c_int) -> *mut File {
    match FILES.get() {
        Some(ops) => {
            // SAFETY: `ops` is an installed static file table whose `fget` function pointer is valid.
            unsafe { (ops.fget)(fd) }
        }
        None => ptr::null_mut(),
    }
}

/// Linux `fd_install(fd, file)`.
///
/// # Safety
/// `fd` must be an allocated unused descriptor and `file` a live referenced
/// file whose ownership is transferred into that descriptor.
#[unsafe(export_name = "fd_install")]
pub unsafe extern "C" fn fd_install(fd: c_int, file: *mut File) {
    // SAFETY: Caller guarantees `fd` is an allocated unused descriptor and `file` is a live referenced
    // file whose ownership is transferred. `ops` is a valid static table.
    unsafe { (FILES.require("file table").fd_install)(fd, file) };
}

// ---------------------------------------------------------------------------
// Eventfd contexts
// ---------------------------------------------------------------------------

/// `eventfd_ctx_fdget()`: returns `err_ptr(-ENODEV)` without an OS provider.
///
/// # Safety
/// Any installed eventfd provider must remain live and obey the `EventfdOps`
/// callback contract; release a successful context with `eventfd_ctx_put`.
#[unsafe(export_name = "drm_kapi_eventfd_ctx_fdget")]
pub unsafe extern "C" fn eventfd_ctx_fdget(fd: c_int) -> *mut c_void {
    match EVENTFD.get() {
        Some(ops) => {
            // SAFETY: `ops` is an installed static eventfd ops table whose `ctx_fdget` function pointer is valid.
            unsafe { (ops.ctx_fdget)(fd) }
        }
        None => err_ptr(-ENODEV),
    }
}

/// Signal an eventfd context. Returns `-ENODEV` without an OS provider.
///
/// # Safety
/// `ctx` must be a live reference returned by `eventfd_ctx_fdget` and must
/// remain referenced for the duration of the operation.
#[unsafe(export_name = "drm_kapi_eventfd_signal")]
pub unsafe extern "C" fn eventfd_signal(ctx: *mut c_void, count: u64) -> c_int {
    match EVENTFD.get() {
        Some(ops) => {
            // SAFETY: Caller guarantees `ctx` is a live reference returned by `eventfd_ctx_fdget`
            // and remains referenced for the call duration.
            unsafe { (ops.signal)(ctx, count) }
        }
        None => -ENODEV,
    }
}

/// Drop the context reference returned by `eventfd_ctx_fdget()`.
///
/// # Safety
/// `ctx` must be a live referenced context returned by `eventfd_ctx_fdget`
/// and must not be released more than once.
#[unsafe(export_name = "drm_kapi_eventfd_ctx_put")]
pub unsafe extern "C" fn eventfd_ctx_put(ctx: *mut c_void) {
    if let Some(ops) = EVENTFD.get() {
        // SAFETY: Caller guarantees `ctx` is a live referenced context returned by `eventfd_ctx_fdget`
        // and is not released more than once.
        unsafe { (ops.ctx_put)(ctx) };
    }
}

// ---------------------------------------------------------------------------
// Installation
// ---------------------------------------------------------------------------

pub fn install_task_ops(ops: &'static TaskOps) -> Result<(), &'static str> {
    TASK.install(ops)
}

pub fn install_file_ops(ops: &'static FileOps) -> Result<(), &'static str> {
    FILES.install(ops)
}

pub fn install_eventfd_ops(ops: &'static EventfdOps) -> Result<(), &'static str> {
    EVENTFD.install(ops)
}

#[cfg(test)]
mod tests {
    use super::ProviderSlot;

    #[test]
    fn provider_slot_install_is_idempotent_and_rejects_another_table() {
        static FIRST: u32 = 1;
        static SECOND: u32 = 2;

        let slot = ProviderSlot::new();
        assert!(slot.install(&FIRST).is_ok());
        assert!(slot.install(&FIRST).is_ok());
        assert!(slot.install(&SECOND).is_err());
        assert_eq!(slot.get(), Some(&FIRST));
    }
}
