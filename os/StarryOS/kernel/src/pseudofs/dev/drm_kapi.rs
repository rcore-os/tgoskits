//! StarryOS providers for the shared LinuxKPI DRM core.
//!
//! LinuxKPI files use the Starry descriptor table as their only fd namespace.
//! The wrapper owns exactly the shmem::File reference transferred by fd_install;
//! fget returns a separate LinuxKPI reference.

#[cfg(all(test, axtest))]
mod tests;

use alloc::{
    borrow::Cow,
    boxed::Box,
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    cell::UnsafeCell,
    ffi::{c_char, c_int, c_long, c_void},
    mem, ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering},
};

use ax_runtime::hal::{paging::MappingFlags, trap::PageFaultFlags};
use axfs_ng_vfs::DeviceId;
use axpoll::{IoEvents, Pollable};
use axpoll_set::PollSet;
use drm_kapi::{
    config::{EINVAL, EIO, ENOMEM, err_ptr, is_err},
    kernel_services::{
        EventfdOps, FileOps, TaskOps, install_eventfd_ops, install_file_ops, install_task_ops,
    },
    mm_native::{UserAddressSpace, VmFlags, install_current_user_address_space},
    shmem::{self, File as LinuxKpiFile, FileOperations},
    types::{WaitQueueEntry, WaitQueueHead},
    wait::{PollTable, add_wait_queue, remove_wait_queue},
};
use linux_raw_sys::general::{CAP_PERFMON, CAP_SYS_ADMIN, O_CLOEXEC, O_NONBLOCK};

use crate::{
    StarryError, StarryResult,
    file::{
        FileDescriptor, FileDescriptorReservation, FileLike, IoDst, IoSrc, Kstat, current_fd_table,
        dma_buf_seek, get_file_like,
    },
    pseudofs::DeviceMmap,
    sync::Mutex,
    task::{UserTaskRef, try_current_user_task},
};

const EPOLLIN: u32 = 0x0001;
const EPOLLOUT: u32 = 0x0004;
const EPOLLERR: u32 = 0x0008;
const EPOLLHUP: u32 = 0x0010;
const EPOLLRDNORM: u32 = 0x0040;
const EPOLLWRNORM: u32 = 0x0100;

static TASK_OPS: TaskOps = TaskOps {
    copy_current_comm,
    current_pid_vnr,
    capable,
    perfmon_capable,
};

static FILE_OPS: FileOps = FileOps {
    anon_inode_getfile,
    get_unused_fd_flags,
    put_unused_fd,
    fd_install,
    fget,
};

static EVENTFD_OPS: EventfdOps = EventfdOps {
    ctx_fdget: eventfd_ctx_fdget,
    signal: eventfd_signal,
    ctx_put: eventfd_ctx_put,
};

struct PendingFd {
    reservation: FileDescriptorReservation,
    file: Arc<KapiFile>,
    cloexec: bool,
}

static FD_RESERVATIONS: Mutex<BTreeMap<(usize, c_int), PendingFd>> = Mutex::new(BTreeMap::new());
/// Resolve an installed LinuxKPI `struct file` back to its Starry owner while
/// `poll_table.qproc` runs. A weak entry avoids making the file table a second
/// owner and lets close remove the lookup before final fput.
static KAPI_FILES: Mutex<BTreeMap<usize, Weak<KapiFile>>> = Mutex::new(BTreeMap::new());

/// Install the common kernel services before any DRM node is published.
pub(crate) fn init() {
    install_task_ops(&TASK_OPS).expect("DRM task provider already belongs to another kernel");
    install_file_ops(&FILE_OPS).expect("DRM file provider already belongs to another kernel");
    install_eventfd_ops(&EVENTFD_OPS)
        .expect("DRM eventfd provider already belongs to another kernel");
    install_current_user_address_space(current_user_address_space)
        .expect("DRM current-mm provider already belongs to another kernel");
    drm_kapi::signal::install_signal_pending_state(starry_signal_pending_state)
        .expect("DRM signal provider already belongs to another kernel");
    super::card0::init();
}

fn active_table_key() -> usize {
    Arc::as_ptr(&current_fd_table()) as usize
}

/// # Safety
/// `buf`, if non-null, must point to at least 16 writable bytes.
unsafe extern "C" fn copy_current_comm(buf: *mut c_char) {
    const TASK_COMM_LEN: usize = 16;

    if buf.is_null() {
        return;
    }
    // SAFETY: the LinuxKPI callback contract gives this provider a writable
    // TASK_COMM_LEN-byte destination owned by the caller.
    unsafe { ptr::write_bytes(buf, 0, TASK_COMM_LEN) };
    let Ok(Some(task)) = try_current_user_task() else {
        return;
    };
    // `name()` returns an Arc-backed immutable snapshot, so a concurrent
    // PR_SET_NAME cannot invalidate the bytes while this synchronous copy is
    // in progress.
    let name = task.name();
    let destination = buf.cast::<u8>();
    for (index, byte) in name.as_bytes().iter().take(TASK_COMM_LEN - 1).enumerate() {
        if *byte == 0 {
            break;
        }
        // SAFETY: `destination` is valid for TASK_COMM_LEN bytes and `index`
        // is bounded to the first TASK_COMM_LEN - 1 entries.
        unsafe { ptr::write(destination.add(index), *byte) };
    }
}

/// # Safety
/// Must be called in a context where task querying is valid.
unsafe extern "C" fn current_pid_vnr() -> c_int {
    // DRM_GETCLIENT is a userspace ioctl. Kernel workers have no Starry user
    // PID context, so return 0 rather than substituting their scheduler ID.
    let Ok(Some(task)) = try_current_user_task() else {
        return 0;
    };
    c_int::try_from(task.as_thread().user_tid().get()).unwrap_or_default()
}

/// LinuxKPI passes an opaque scheduler identity token, not an axhal task
/// pointer. Only the currently executing Starry user thread can be resolved
/// through Starry's task API; kernel workers and mismatched tokens fail closed.
///
/// # Safety
/// `task` is an opaque scheduler token passed by LinuxKPI task runtime.
unsafe extern "C" fn starry_signal_pending_state(state: c_int, task: *mut c_void) -> bool {
    const TASK_INTERRUPTIBLE: c_int = drm_kapi::wait::TASK_INTERRUPTIBLE as c_int;

    if state & TASK_INTERRUPTIBLE == 0 || task.is_null() {
        return false;
    }
    let Some(current_id) = drm_kapi::task_runtime::current_task_id() else {
        return false;
    };
    if current_id != task as usize as u64 {
        return false;
    }
    let Ok(Some(current)) = try_current_user_task() else {
        return false;
    };

    let signals = current.as_thread().signal();
    let mut unblocked = !signals.blocked();
    // Linux never allows SIGKILL or SIGSTOP to be blocked.
    unblocked.add(starry_signal::Signo::SIGKILL);
    unblocked.add(starry_signal::Signo::SIGSTOP);
    let pending = signals.pending() & unblocked;
    if pending.is_empty() {
        return false;
    }

    // A signal whose disposition is ignored (including default-ignore and
    // default-continue) does not interrupt Linux's interruptible waits.
    let actions = signals.process().actions();
    let actions = actions.lock_irqsave();
    (1..=64).any(|number| {
        starry_signal::Signo::from_repr(number)
            .is_some_and(|signo| pending.has(signo) && !actions[signo].is_ignore(signo))
    })
}

/// # Safety
/// `capability` is a Linux capability value.
unsafe extern "C" fn capable(capability: c_int) -> bool {
    capability >= 0
        && try_current_user_task()
            .ok()
            .flatten()
            .is_some_and(|task| task.as_thread().cred().has_cap(capability as u32))
}

/// # Safety
/// Must be called in a valid task execution context.
unsafe extern "C" fn perfmon_capable() -> bool {
    try_current_user_task().ok().flatten().is_some_and(|task| {
        let cred = task.as_thread().cred();
        cred.has_cap(CAP_PERFMON) || cred.has_cap(CAP_SYS_ADMIN)
    })
}

/// # Safety
/// `flags` must contain valid open/file-descriptor flags.
unsafe extern "C" fn get_unused_fd_flags(flags: c_int) -> c_int {
    let reservation = match FileDescriptorReservation::new() {
        Ok(reservation) => reservation,
        Err(error) => return -error.linux_errno().into_raw(),
    };
    let fd = reservation.fd();
    let key = (active_table_key(), fd);
    let file = match Arc::try_new(KapiFile::pending(flags as u32)) {
        Ok(file) => file,
        Err(_) => return -ENOMEM,
    };
    let pending = PendingFd {
        reservation,
        file,
        cloexec: flags as u32 & O_CLOEXEC != 0,
    };
    if FD_RESERVATIONS.lock().insert(key, pending).is_some() {
        panic!("DRM fd number was reserved twice in one Starry file table");
    }
    fd
}

/// # Safety
/// `fd` must be an allocated unused file descriptor not yet installed or released.
unsafe extern "C" fn put_unused_fd(fd: c_int) {
    let pending = FD_RESERVATIONS.lock().remove(&(active_table_key(), fd));
    // Drop outside the map lock to release the private Starry fd reservation.
    drop(pending);
}

/// # Safety
/// `name` must be a valid NUL-terminated C string. `fops` must point to a live `FileOperations`
/// table. `private_data` must remain valid for the lifetime of the returned file.
unsafe extern "C" fn anon_inode_getfile(
    name: *const c_char,
    fops: *const c_void,
    private_data: *mut c_void,
    flags: c_int,
) -> *mut LinuxKpiFile {
    if name.is_null() || fops.is_null() {
        return err_ptr(-EINVAL);
    }
    // SAFETY: `name` is verified non-null and valid per caller contract. Allocates a shmem anon file.
    let raw = unsafe { shmem::shmem_file_setup(name, 0, 0) };
    if raw.is_null() || is_err(raw) {
        return raw;
    }
    // SAFETY: `raw` is non-null and verified not an error pointer; initializes its file operations,
    // private data, and flags before publishing to the caller.
    unsafe {
        (*raw).f_op = fops.cast::<FileOperations>();
        (*raw)._private_data = private_data;
        (*raw).f_flags = flags as u32;
    }
    raw
}

/// # Safety
/// `fd` must be a valid reserved descriptor from `get_unused_fd_flags`.
/// `raw` must be a live, referenced `LinuxKpiFile`.
unsafe extern "C" fn fd_install(fd: c_int, raw: *mut LinuxKpiFile) {
    if raw.is_null() || is_err(raw) {
        panic!("DRM fd_install received a non-file pointer");
    }
    let pending = FD_RESERVATIONS
        .lock()
        .remove(&(active_table_key(), fd))
        .unwrap_or_else(|| panic!("DRM fd_install without a Starry fd reservation"));
    pending.file.install(raw);
    KAPI_FILES
        .lock()
        .insert(raw as usize, Arc::downgrade(&pending.file));
    let inner: Arc<dyn FileLike> = pending.file;
    pending.reservation.install(FileDescriptor {
        inner,
        cloexec: pending.cloexec,
    });
}

/// # Safety
/// `fd` is a file descriptor index to resolve in the current task's descriptor table.
unsafe extern "C" fn fget(fd: c_int) -> *mut LinuxKpiFile {
    let Ok(file) = get_file_like(fd) else {
        return ptr::null_mut();
    };
    if let Some(kapi) = file.downcast_ref::<KapiFile>() {
        let raw = kapi.raw();
        if raw.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: `raw` is verified non-null and points to a live `LinuxKpiFile` owned by `kapi`.
        return unsafe { shmem::get_file_file(raw) };
    }
    if let Some(dma_buf) = file.downcast_ref::<crate::file::dmabuf::DmaBufFile>() {
        return dma_buf.get_drm_file();
    }
    ptr::null_mut()
}

/// # Safety
/// `fd` must be a valid descriptor in the current task.
unsafe extern "C" fn eventfd_ctx_fdget(fd: c_int) -> *mut c_void {
    let Ok(file) = get_file_like(fd) else {
        return err_ptr(-crate::Errno::EBADF.into_raw());
    };
    let Ok(eventfd) = file.downcast_arc::<crate::file::event::EventFd>() else {
        return err_ptr(-crate::Errno::EINVAL.into_raw());
    };
    Arc::into_raw(eventfd).cast_mut().cast()
}

/// # Safety
/// `ctx`, if non-null, must be an `Arc<EventFd>` raw pointer returned by `eventfd_ctx_fdget`.
unsafe extern "C" fn eventfd_signal(ctx: *mut c_void, count: u64) -> c_int {
    if ctx.is_null() {
        return -EINVAL;
    }
    // SAFETY: `ctx` was checked non-null and was obtained from `Arc::into_raw` in `eventfd_ctx_fdget`.
    let eventfd = unsafe { &*ctx.cast::<crate::file::event::EventFd>() };
    eventfd.signal_kernel_saturating(count);
    0
}

/// # Safety
/// `ctx`, if non-null, must be an `Arc<EventFd>` raw pointer returned by `eventfd_ctx_fdget`
/// and must not be released more than once.
unsafe extern "C" fn eventfd_ctx_put(ctx: *mut c_void) {
    if !ctx.is_null() {
        // SAFETY: `ctx` was non-null and created via `Arc::into_raw` in `eventfd_ctx_fdget`.
        // Dropping reconstructs the Arc to decrement its reference count exactly once.
        unsafe { drop(Arc::from_raw(ctx.cast::<crate::file::event::EventFd>())) };
    }
}

struct KapiFile {
    raw: AtomicPtr<LinuxKpiFile>,
    flags: AtomicU32,
    nonblocking: AtomicBool,
    poll: PollSet,
    wait_entries: Mutex<Vec<WaitEntryOwner>>,
    flags_lock: Mutex<()>,
}

struct WaitEntryOwner {
    head: usize,
    entry: Box<UnsafeCell<WaitQueueEntry>>,
}

// SAFETY: Linux file fields are internally synchronized by LinuxKPI, while
// Starry's f_flags mirror and wait-entry collection use explicit locks/atomics.
unsafe impl Send for KapiFile {}
// SAFETY: FileLike operations may run concurrently on one open file description;
// the raw LinuxKPI file contract supports that, and wrapper state is synchronized.
unsafe impl Sync for KapiFile {}

impl KapiFile {
    fn pending(flags: u32) -> Self {
        Self {
            raw: AtomicPtr::new(ptr::null_mut()),
            flags: AtomicU32::new(flags),
            nonblocking: AtomicBool::new(flags & O_NONBLOCK != 0),
            poll: PollSet::new(),
            wait_entries: Mutex::new(Vec::new()),
            flags_lock: Mutex::new(()),
        }
    }

    fn install(&self, raw: *mut LinuxKpiFile) {
        assert!(
            self.raw
                .compare_exchange(ptr::null_mut(), raw, Ordering::Release, Ordering::Relaxed)
                .is_ok(),
            "LinuxKPI file installed into Starry wrapper twice"
        );
        // SAFETY: `raw` is non-null and points to a valid live `LinuxKpiFile`.
        let flags = unsafe { (*raw).f_flags };
        self.flags.store(flags, Ordering::Release);
        self.nonblocking
            .store(flags & O_NONBLOCK != 0, Ordering::Release);
    }

    fn raw(&self) -> *mut LinuxKpiFile {
        self.raw.load(Ordering::Acquire)
    }
}

impl Drop for KapiFile {
    fn drop(&mut self) {
        let raw = self.raw.swap(ptr::null_mut(), Ordering::AcqRel);
        if raw.is_null() {
            return;
        }
        KAPI_FILES.lock().remove(&(raw as usize));
        let entries = mem::take(&mut *self.wait_entries.lock());
        for entry in entries {
            // Detach before fput can run the owning file_operations release.
            // SAFETY: `entry.head` is a valid pointer to WaitQueueHead registered previously.
            unsafe { remove_wait_queue(entry.head_as_ptr(), entry.entry.get()) };
        }
        // SAFETY: Ownership of `raw` is transferred to `fput_file` to release the LinuxKPI file reference.
        unsafe { shmem::fput_file(raw) };
    }
}

impl WaitEntryOwner {
    fn head_as_ptr(&self) -> *mut WaitQueueHead {
        self.head as *mut WaitQueueHead
    }
}

impl FileLike for KapiFile {
    fn seek(&self, pos: ax_io::SeekFrom) -> StarryResult<u64> {
        let raw = self.raw();
        if raw.is_null() {
            return Err(StarryError::BadFileDescriptor);
        }
        // Only DMA-BUF anon files support Linux's size-probe seeks; other
        // LinuxKPI anon inodes such as sync_file remain non-seekable.
        // SAFETY: `raw` is checked non-null and points to a valid live `LinuxKpiFile`.
        let Some(dmabuf) = (unsafe { kdrm_core::drm_dma_buf_upstream::drm_dma_buf_from_file(raw) })
        else {
            return Err(StarryError::from(crate::Errno::ESPIPE));
        };
        // SAFETY: `dmabuf` is verified non-null and live from `drm_dma_buf_from_file`.
        dma_buf_seek(unsafe { (*dmabuf).size as u64 }, pos)
    }

    fn validate_write_access(&self) -> StarryResult {
        Ok(())
    }

    fn read(&self, _dst: &mut IoDst) -> StarryResult<usize> {
        Err(StarryError::InvalidInput)
    }

    fn write(&self, _src: &mut IoSrc) -> StarryResult<usize> {
        Err(StarryError::InvalidInput)
    }

    fn stat(&self) -> StarryResult<Kstat> {
        // Linux anon_inode/dma-buf stats expose permissions without an inode
        // type; dma_buf_export additionally sets i_size to the exported size.
        let mut stat = Kstat {
            mode: 0o600,
            rdev: DeviceId::default(),
            ..Kstat::default()
        };
        let raw = self.raw();
        if !raw.is_null()
            // SAFETY: `raw` is non-null and points to a valid live `LinuxKpiFile`.
            && let Some(dmabuf) =
                (unsafe { kdrm_core::drm_dma_buf_upstream::drm_dma_buf_from_file(raw) })
        {
            // SAFETY: `dmabuf` is non-null and valid for inspection.
            stat.size = unsafe { (*dmabuf).size as u64 };
        }
        Ok(stat)
    }

    fn path(&self) -> Cow<'_, str> {
        "anon_inode:drm-kapi".into()
    }

    fn ioctl(&self, current: &UserTaskRef, cmd: u32, arg: usize) -> StarryResult<usize> {
        let raw = self.raw();
        if raw.is_null() {
            return Err(StarryError::BadFileDescriptor);
        }
        // SAFETY: `raw` is checked non-null and points to a valid `LinuxKpiFile`.
        let fops = unsafe { (*raw).f_op };
        if fops.is_null() {
            return Err(StarryError::NotATty);
        }
        // SAFETY: `fops` is non-null and points to valid `FileOperations`.
        let Some(ioctl) = (unsafe { (*fops).unlocked_ioctl }) else {
            return Err(StarryError::NotATty);
        };
        // SAFETY: Calls the driver's ioctl function with valid parameters.
        let result: c_long = unsafe { ioctl(raw, cmd, arg) };
        if result < 0 {
            let errno = c_int::try_from(result.saturating_neg()).unwrap_or(EIO);
            if errno == drm_kapi::config::ERESTARTSYS {
                // Preserve the internal restart class until Starry's signal
                // return path applies EINTR/SA_RESTART policy.
                Err(StarryError::Interrupted)
            } else {
                Err(StarryError::from(crate::Errno::new(errno)))
            }
        } else {
            let _ = current;
            Ok(result as usize)
        }
    }

    fn device_mmap(&self, offset: u64, length: u64) -> StarryResult<DeviceMmap> {
        let raw = self.raw();
        if raw.is_null() {
            return Err(StarryError::BadFileDescriptor);
        }
        // The DRM core owns the exact DMA_BUF_FOPS discriminator. Do not
        // inspect `_private_data` until it confirms this is a DMA-BUF file;
        // other anon-inode fds (notably sync_file) store different objects.
        // SAFETY: `raw` is checked non-null and points to a valid `LinuxKpiFile`.
        let Some(dmabuf) = (unsafe { kdrm_core::drm_dma_buf_upstream::drm_dma_buf_from_file(raw) })
        else {
            return Ok(DeviceMmap::None);
        };
        let Some(device) = ax_drm::device() else {
            return Ok(DeviceMmap::None);
        };
        // SAFETY: `device.raw()` and `dmabuf` are non-null and valid for query.
        if !unsafe {
            kdrm_core::drm_prime_upstream::drm_gem_is_prime_exported_dma_buf(device.raw(), dmabuf)
        } {
            return Ok(DeviceMmap::None);
        }
        // The DMA-BUF file reference pins `dmabuf` and its GEM object for the
        // synchronous lease creation; the lease takes its own GEM reference.
        // SAFETY: `dmabuf` is verified to be a PRIME DMA-BUF whose `r#priv` holds the `DrmGemObject`.
        let obj = unsafe {
            (*dmabuf)
                .r#priv
                .cast::<kdrm_core::drm_gem_types::DrmGemObject>()
        };
        // SAFETY: the verified DRM PRIME DMA-BUF file pins this exported GEM
        // object for the synchronous lease creation; mmap_gem_object takes a
        // separate GEM reference for the returned lease.
        let lease = unsafe { ax_drm::mmap_gem_object(obj, offset, length) }.map_err(|error| {
            let errno = error.saturating_neg();
            StarryError::from(crate::Errno::new(errno))
        })?;
        let range = lease.physical();
        let cached = lease.cached();
        let retainer = Some(Arc::new(lease) as Arc<dyn core::any::Any + Send + Sync>);
        Ok(if cached {
            DeviceMmap::PhysicalCachedResolved(range, retainer)
        } else {
            DeviceMmap::PhysicalResolved(range, retainer)
        })
    }

    fn open_flags(&self) -> u32 {
        self.flags.load(Ordering::Acquire)
    }

    fn nonblocking(&self) -> bool {
        self.nonblocking.load(Ordering::Acquire)
    }

    fn set_nonblocking(&self, nonblocking: bool) -> StarryResult {
        let _guard = self.flags_lock.lock();
        let flags = if nonblocking {
            self.flags.load(Ordering::Acquire) | O_NONBLOCK
        } else {
            self.flags.load(Ordering::Acquire) & !O_NONBLOCK
        };
        self.flags.store(flags, Ordering::Release);
        self.nonblocking.store(nonblocking, Ordering::Release);
        Ok(())
    }
}

impl Pollable for KapiFile {
    fn poll(&self) -> IoEvents {
        let raw = self.raw();
        if raw.is_null() {
            return IoEvents::ERR | IoEvents::HUP;
        }
        // SAFETY: `raw` is checked non-null and points to a valid `LinuxKpiFile`.
        let fops = unsafe { (*raw).f_op };
        if fops.is_null() {
            return IoEvents::IN | IoEvents::OUT;
        }
        // SAFETY: `fops` is non-null and points to valid `FileOperations`.
        let Some(poll) = (unsafe { (*fops).poll }) else {
            return IoEvents::IN | IoEvents::OUT;
        };
        let mut table = PollTable {
            qproc: Some(register_poll_wait),
            key: EPOLLIN | EPOLLOUT | EPOLLRDNORM | EPOLLWRNORM,
        };
        // SAFETY: Calls driver poll callback with live `raw` and valid stack-allocated `PollTable`.
        linux_poll_events(unsafe { poll(raw, &mut table) })
    }

    /// # Safety
    /// Must be called in task or deferred context. The caller must guarantee `&self`
    /// remains valid for the duration of the registration on `sink`.
    unsafe fn register_shared(
        &self,
        sink: &mut dyn axpoll::SharedRegistrationSink,
        events: IoEvents,
    ) {
        if events.intersects(
            IoEvents::IN
                | IoEvents::RDNORM
                | IoEvents::OUT
                | IoEvents::WRNORM
                | IoEvents::ERR
                | IoEvents::HUP,
        ) {
            // SAFETY: `&self.poll` is pinned by `self` and implements `PollSource`. Registration
            // is performed in task/deferred context as required by caller contract.
            unsafe { sink.register_shared(&self.poll, events) };
        }
    }

    /// # Safety
    /// Must be called in task or deferred context. The caller must guarantee `&self`
    /// remains valid for the duration of the registration on `sink`.
    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn axpoll::ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        if events.intersects(
            IoEvents::IN
                | IoEvents::RDNORM
                | IoEvents::OUT
                | IoEvents::WRNORM
                | IoEvents::ERR
                | IoEvents::HUP,
        ) {
            // SAFETY: `&self.poll` is pinned by `self` and implements `PollSource`. Registration
            // is performed in task/deferred context as required by caller contract.
            unsafe { sink.register_exclusive(&self.poll, events) };
        }
    }
}

/// # Safety
/// `filp` is a live file identity token used only as a lookup key, never
/// dereferenced; `head` must point to an initialized `WaitQueueHead`.
unsafe extern "C" fn register_poll_wait(
    filp: *mut drm_kapi::types::File,
    head: *mut WaitQueueHead,
    _table: *mut PollTable,
) {
    if filp.is_null() || head.is_null() {
        return;
    }
    let target = filp as usize;
    let Some(wrapper) = KAPI_FILES.lock().get(&target).and_then(Weak::upgrade) else {
        return;
    };
    // The upgraded Arc pins the wrapper through registration, even if the
    // original descriptor is concurrently closed.
    let mut entries = wrapper.wait_entries.lock();
    if entries.iter().any(|entry| entry.head == head as usize) {
        return;
    }
    // SAFETY: Zeroed memory creates an initial unlinked WaitQueueEntry.
    let Ok(entry) = Box::try_new(UnsafeCell::new(unsafe { mem::zeroed::<WaitQueueEntry>() })) else {
        return;
    };
    // SAFETY: The Box gives stable storage, not yet published. UnsafeCell
    // keeps later queue-link mutations from aliasing an exclusive Box borrow.
    unsafe {
        (*entry.get()).private = (&wrapper.poll as *const PollSet).cast_mut().cast();
        (*entry.get()).func = Some(wake_poll_waiters);
        drm_kapi::list::init_list_head(ptr::addr_of_mut!((*entry.get()).entry));
    }
    // SAFETY: `head` is non-null and valid per caller contract; `entry` is pinned by Box.
    unsafe { add_wait_queue(head, entry.get()) };
    if entries.try_reserve(1).is_err() {
        // SAFETY: Removes `entry` from `head` if allocating vector space fails.
        unsafe { remove_wait_queue(head, entry.get()) };
        return;
    }
    entries.push(WaitEntryOwner {
        head: head as usize,
        entry,
    });
}

/// # Safety
/// `entry`, if non-null, must point to a live `WaitQueueEntry` registered via `register_poll_wait`.
unsafe extern "C" fn wake_poll_waiters(
    entry: *mut WaitQueueEntry,
    _mode: u32,
    _sync: c_int,
    key: *mut c_void,
) -> c_int {
    if entry.is_null() {
        return 0;
    }
    // SAFETY: `entry` is non-null and points to a registered `WaitQueueEntry` whose `private`
    // was assigned a valid pointer to `PollSet`.
    let poll = unsafe { (*entry).private.cast::<PollSet>() };
    if !poll.is_null() {
        // ax-drm sends RDIF completion and synthetic-vblank notifications
        // from task/deferred context. A future hardware IRQ path must defer
        // before entering the common DRM event-send path.
        // Preserve keyed device-terminal notifications as well as read events.
        // SAFETY: The embedded PollSet stays pinned until this wait entry is
        // detached, and ax-drm sends completion/vblank events from
        // task/deferred context.
        let events = if key.is_null() { IoEvents::IN | IoEvents::RDNORM } else { super::card0::linux_poll_events(key as usize as u32) };
        unsafe { (&*poll).wake_all(events) };
    }
    1
}

fn linux_poll_events(mask: u32) -> IoEvents {
    let mut events = IoEvents::empty();
    events.set(IoEvents::IN, mask & EPOLLIN != 0);
    events.set(IoEvents::OUT, mask & EPOLLOUT != 0);
    events.set(IoEvents::RDNORM, mask & EPOLLRDNORM != 0);
    events.set(IoEvents::WRNORM, mask & EPOLLWRNORM != 0);
    events.set(IoEvents::ERR, mask & EPOLLERR != 0);
    events.set(IoEvents::HUP, mask & EPOLLHUP != 0);
    events
}

fn current_user_address_space() -> Option<Arc<dyn UserAddressSpace>> {
    let task = try_current_user_task().ok().flatten()?;
    let mm = task.as_thread().proc_data.pin_aspace().ok()?;
    Some(Arc::new(StarryAddressSpace { mm }))
}

struct StarryAddressSpace {
    mm: crate::mm::MmPin,
}

impl UserAddressSpace for StarryAddressSpace {
    fn id(&self) -> u64 {
        self.mm.id().get()
    }

    fn alive(&self) -> bool {
        self.mm.state() == crate::mm::MmState::Live
    }

    fn bounds(&self) -> (usize, usize) {
        let aspace = self.mm.lock();
        (aspace.base().as_usize(), aspace.end().as_usize())
    }

    fn contains_range(&self, addr: usize, size: usize) -> bool {
        self.alive()
            && self
                .mm
                .lock()
                .contains_range(ax_memory_addr::VirtAddr::from(addr), size)
    }

    fn find_free_area(&self, hint: usize, size: usize) -> Option<usize> {
        if !self.alive() || size == 0 {
            return None;
        }
        let aspace = self.mm.lock();
        let range_size = aspace
            .end()
            .as_usize()
            .saturating_sub(aspace.base().as_usize());
        aspace
            .find_free_area(
                ax_memory_addr::VirtAddr::from(hint),
                size,
                ax_memory_addr::VirtAddrRange::from_start_size(aspace.base(), range_size),
                4096,
            )
            .map(|address| address.as_usize())
    }

    fn query(&self, addr: usize) -> Option<(usize, VmFlags)> {
        let (physical, flags) = self
            .mm
            .lock()
            .query_mapping(ax_memory_addr::VirtAddr::from(addr))?;
        Some((physical, vm_flags(flags)))
    }

    fn read(&self, addr: usize, dst: &mut [u8]) -> bool {
        if dst.is_empty() {
            return true;
        }
        if !self.access_range(addr, dst.len(), false) {
            return false;
        }
        let mm = self.mm.lock();
        let address = ax_memory_addr::VirtAddr::from(addr);
        // Faulting may have dropped and reacquired the address-space lock.
        // Recheck current policy and copy under one guard so concurrent
        // mprotect cannot revoke access between validation and usercopy.
        mm.can_access_range(address, dst.len(), MappingFlags::READ) && mm.read(address, dst).is_ok()
    }

    fn write(&self, addr: usize, src: &[u8]) -> bool {
        if src.is_empty() {
            return true;
        }
        if !self.access_range(addr, src.len(), true) {
            return false;
        }
        let mut mm = self.mm.lock();
        let address = ax_memory_addr::VirtAddr::from(addr);
        // Keep the WRITE check, any COW performed by `write`, and the actual
        // copy under the same address-space guard.
        mm.can_access_range(address, src.len(), MappingFlags::WRITE)
            && mm.write(address, src).is_ok()
    }

    fn map_linear(&self, addr: usize, paddr: usize, size: usize, flags: VmFlags) -> bool {
        let native_flags = native_mapping_flags(flags);
        self.alive()
            && self
                .mm
                .lock()
                .map_linear(
                    ax_memory_addr::VirtAddr::from(addr),
                    ax_memory_addr::PhysAddr::from(paddr),
                    size,
                    native_flags | MappingFlags::USER,
                )
                .is_ok()
    }

    fn unmap(&self, addr: usize, size: usize) -> bool {
        self.alive()
            && self
                .mm
                .lock()
                .unmap(ax_memory_addr::VirtAddr::from(addr), size)
                .is_ok()
    }

    fn pin(
        &self,
        _addr: usize,
        _size: usize,
        _write: bool,
    ) -> Result<drm_kapi::mm_native::PinnedFrames, c_int> {
        // Starry has no long-term user-page pin contract for this DRM path.
        Err(-drm_kapi::config::EOPNOTSUPP)
    }
}

impl StarryAddressSpace {
    fn access_range(&self, addr: usize, size: usize, write: bool) -> bool {
        if size == 0 {
            return true;
        }
        if !self.contains_range(addr, size) {
            return false;
        }
        let access = if write {
            MappingFlags::WRITE
        } else {
            MappingFlags::READ
        };
        if !self
            .mm
            .lock()
            .can_access_range(ax_memory_addr::VirtAddr::from(addr), size, access)
        {
            return false;
        }
        let Some(end) = addr.checked_add(size) else {
            return false;
        };
        let first = addr & !4095;
        let last = (end - 1) & !4095;
        let fault_flags = PageFaultFlags::USER
            | if write {
                PageFaultFlags::WRITE
            } else {
                PageFaultFlags::READ
            };
        let mut page = first;
        loop {
            if !self
                .mm
                .handle_page_fault(ax_memory_addr::VirtAddr::from(page), fault_flags)
            {
                return false;
            }
            if page == last {
                return true;
            }
            let Some(next) = page.checked_add(4096) else {
                return false;
            };
            page = next;
        }
    }
}

fn vm_flags(flags: MappingFlags) -> VmFlags {
    let mut result = VmFlags::default();
    if flags.contains(MappingFlags::USER) {
        result |= VmFlags::USER;
    }
    if flags.contains(MappingFlags::READ) {
        result |= VmFlags::READ;
    }
    if flags.contains(MappingFlags::WRITE) {
        result |= VmFlags::WRITE;
    }
    if flags.contains(MappingFlags::EXECUTE) {
        result |= VmFlags::EXECUTE;
    }
    if flags.contains(MappingFlags::UNCACHED) {
        result |= VmFlags::UNCACHED;
    }
    result
}

fn native_mapping_flags(flags: VmFlags) -> MappingFlags {
    let mut result = MappingFlags::empty();
    if flags.contains(VmFlags::READ) {
        result |= MappingFlags::READ;
    }
    if flags.contains(VmFlags::WRITE) {
        result |= MappingFlags::WRITE;
    }
    if flags.contains(VmFlags::EXECUTE) {
        result |= MappingFlags::EXECUTE;
    }
    if flags.contains(VmFlags::UNCACHED) {
        result |= MappingFlags::UNCACHED;
    }
    result
}
