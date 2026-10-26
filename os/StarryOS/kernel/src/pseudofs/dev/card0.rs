//! StarryOS VFS adapter for the shared LinuxKPI DRM core.
//!
//! The DRM minor, per-open `drm_file`, ioctl dispatch, event queue and driver
//! callbacks are owned by `kdrm-core`/`ax-drm`. This module only binds that
//! lifecycle to Starry's device nodes, file descriptions, waiters and VMAs.

use alloc::{borrow::Cow, sync::Arc};
use core::{
    any::Any,
    cell::UnsafeCell,
    ffi::{c_int, c_ulong, c_void},
    mem, ptr,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

use ax_fs_ng::vfs::{OpenOptions, OpenResult};
use axfs_ng_vfs::{DeviceId, NodeFlags, VfsError, VfsResult};
use axpoll::{IoEvents, Pollable};
use axpoll_set::PollSet;
use kdrm_core::{
    drm_file_types::DrmFile,
    drm_file_upstream::{drm_open_minor_index, drm_poll, drm_read_kernel_to, drm_release},
    drm_ioctl_upstream::drm_ioctl,
    drm_lease_upstream::{LeaseFileOps, LesseeFileSetup, install_lease_file_ops},
};
use drm_kapi::{
    config::EFAULT,
    list::init_list_head,
    types::{File as LinuxFile, WaitQueueEntry},
    wait::{add_wait_queue, remove_wait_queue},
};
use linux_raw_sys::general::{O_ACCMODE, O_CLOEXEC, O_NONBLOCK, O_RDONLY, O_WRONLY};

use crate::{
    StarryError, StarryResult,
    file::{
        File as KernelFile, FileDescriptor, FileDescriptorReservation, FileLike, IoDst, IoSrc,
        Kstat,
    },
    pseudofs::{DeviceMmap, DeviceOps},
    sync::Mutex,
    task::{
        UserTaskRef, current_user_task,
        future::{block_on_user, poll_io},
    },
};

const DRM_MAJOR: u32 = 226;
const DRM_EPOLLIN: u32 = 0x0001;
const DRM_EPOLLOUT: u32 = 0x0004;
const DRM_EPOLLERR: u32 = 0x0008;
const DRM_EPOLLHUP: u32 = 0x0010;
const DRM_EPOLLRDNORM: u32 = 0x0040;
const DRM_EPOLLWRNORM: u32 = 0x0100;

/// The devfs node marker shared by the primary and render-node VFS entries.
pub struct Card0 {
    device: Arc<ax_drm::DeviceOwner>,
}

impl Card0 {
    pub(crate) fn new(device: Arc<ax_drm::DeviceOwner>) -> Self {
        Self { device }
    }
}

/// True if `inner` is the node marker for the registered shared DRM device.
pub(crate) fn is_card0_device(inner: &dyn Any) -> bool {
    inner.is::<Card0>()
}

/// Create one Linux DRM open-file description for a primary or render node.
pub(crate) fn open_card0_file(
    inner: &dyn Any,
    file: ax_fs_ng::File,
    open_flags: u32,
) -> StarryResult<Arc<dyn FileLike>> {
    let card = inner
        .downcast_ref::<Card0>()
        .ok_or(StarryError::NoSuchDevice)?;
    let rdev = file.location().metadata()?.rdev;
    let primary = card.device.primary_minor_index();
    let render = card.device.render_minor_index();
    let minor_index = match (primary, render) {
        (Some(index), _) if rdev == DeviceId::new(DRM_MAJOR, index) => index,
        (_, Some(index)) if rdev == DeviceId::new(DRM_MAJOR, index) => index,
        _ => return Err(StarryError::NoSuchDevice),
    };

    // Allocate the adapter first: kdrm-core stores `filp` inside `drm_file`,
    // so the embedded Linux file must not move after `drm_open_minor_index`.
    let mut opened = Arc::new(Card0File::new(
        KernelFile::new(file, open_flags),
        card.device.clone(),
        open_flags,
    ));
    let opened_mut = Arc::get_mut(&mut opened).ok_or(StarryError::NoSuchDevice)?;
    // SAFETY: 'opened_mut.raw()' points to the newly allocated LinuxFile pinned inside Arc<Card0File>.
    // It is zero-initialized and valid for drm_open_minor_index to populate private_data.
    let ret = unsafe { drm_open_minor_index(minor_index, opened_mut.raw()) };
    if ret != 0 {
        return Err(errno_from_linux(ret));
    }
    opened_mut.register_event_waiter()?;
    let opened: Arc<dyn FileLike> = opened;
    Ok(opened)
}

static LEASE_FILE_OPS: LeaseFileOps = LeaseFileOps { open_lessee_fd };

/// Let `DRM_IOCTL_MODE_CREATE_LEASE` hand lessees their own DRM descriptors.
pub(crate) fn init() {
    install_lease_file_ops(&LEASE_FILE_OPS)
        .expect("DRM lease provider already belongs to another kernel");
}

/// # Safety
/// `lessor_filp` must be the Linux file embedded in a live [`Card0File`]
/// whose ioctl is running; `setup` and `ctx` follow the [`LeaseFileOps`]
/// contract.
unsafe extern "C" fn open_lessee_fd(
    lessor_filp: *mut LinuxFile,
    flags: c_int,
    setup: LesseeFileSetup,
    ctx: *mut c_void,
) -> c_int {
    // SAFETY: Starry only passes DRM files embedded in `Card0File::raw` to
    // kdrm-core, and the lessor's ioctl borrows that `Card0File` for this call.
    let lessor = unsafe { &*Card0File::from_raw(lessor_filp) };
    // SAFETY: forwarded caller contract for `setup` and `ctx`.
    match unsafe { lessor.open_lessee(flags as u32, setup, ctx) } {
        Ok(fd) => fd,
        Err(error) => -error.linux_errno().into_raw(),
    }
}

impl DeviceOps for Card0 {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::BadFileDescriptor)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::BadFileDescriptor)
    }

    fn ioctl(&self, _current: &UserTaskRef, _cmd: u32, _arg: usize) -> VfsResult<usize> {
        // DRM operations require per-open `drm_file` state and are dispatched
        // by `Card0File`, never through the shared inode-level device object.
        Err(VfsError::NotATty)
    }

    fn mmap(&self, _offset: u64, _length: u64) -> DeviceMmap {
        DeviceMmap::None
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

struct Card0File {
    base: KernelFile,
    device: Arc<ax_drm::DeviceOwner>,
    raw: UnsafeCell<LinuxFile>,
    wait_entry: UnsafeCell<WaitQueueEntry>,
    poll: PollSet,
    open_flags: AtomicU32,
    nonblocking: AtomicBool,
    flags_lock: Mutex<()>,
}

// SAFETY: The raw Linux file is initialized before publication and remains
// pinned in this `Arc`. DRM serializes mutable private state internally; the
// VFS-owned wait entry is detached before the DRM core releases that state.
unsafe impl Send for Card0File {}
// SAFETY: `open_flags`/`nonblocking` are atomic, flag mutation is serialized,
// and every access to the shared DRM file follows its LinuxKPI synchronization
// contract. The intrusive wait entry is only changed by open/drop.
unsafe impl Sync for Card0File {}

impl Card0File {
    fn new(base: KernelFile, device: Arc<ax_drm::DeviceOwner>, flags: u32) -> Self {
        // SAFETY: The Linux file prefix is zero-initialized before DRM open,
        // matching the kernel's initial `struct file` state used by this
        // adapter. The raw LinuxKPI side is kept nonblocking permanently;
        // Starry's poll_io layer owns blocking/nonblocking semantics and
        // updates only the synchronized adapter state.
        let mut raw: LinuxFile = unsafe { mem::zeroed() };
        raw.f_flags = flags | O_NONBLOCK;
        Self {
            base,
            device,
            raw: UnsafeCell::new(raw),
            // SAFETY: A zeroed wait-queue entry is initialized with an
            // intrusive list head before it is linked during open.
            wait_entry: UnsafeCell::new(unsafe { mem::zeroed() }),
            poll: PollSet::new(),
            open_flags: AtomicU32::new(flags),
            nonblocking: AtomicBool::new(flags & O_NONBLOCK != 0),
            flags_lock: Mutex::new(()),
        }
    }

    fn raw(&self) -> *mut LinuxFile {
        self.raw.get()
    }

    /// Recover the adapter that embeds `raw`.
    fn from_raw(raw: *mut LinuxFile) -> *const Self {
        // `UnsafeCell` is `repr(transparent)`, so the Linux file sits at the
        // offset of the `raw` field.
        raw.cast::<u8>()
            .wrapping_sub(mem::offset_of!(Self, raw))
            .cast::<Self>()
    }

    /// Linux `file_clone_open()` for a lessee: a new open file description of
    /// the same DRM minor that inherits this file's access and status flags.
    ///
    /// # Safety
    /// `setup` and `ctx` must satisfy the [`LeaseFileOps`] contract.
    unsafe fn open_lessee(
        &self,
        flags: u32,
        setup: LesseeFileSetup,
        ctx: *mut c_void,
    ) -> StarryResult<c_int> {
        let reservation = FileDescriptorReservation::new()?;
        // SAFETY: this file's `drm_file` and its minor stay live while its
        // ioctl, which called us, is running.
        let minor_index = unsafe {
            let private = (*self.raw()).private_data.cast::<DrmFile>();
            (*(*private).minor).index
        };
        let open_flags = self.open_flags() & !O_CLOEXEC;
        let access = open_flags & O_ACCMODE;
        let mut options = OpenOptions::new();
        options.read(access != O_WRONLY).write(access != O_RDONLY);
        let OpenResult::File(file) = options.open_loc(self.base.inner().location().clone())?
        else {
            return Err(StarryError::NoSuchDevice);
        };

        let mut opened = Arc::new(Card0File::new(
            KernelFile::new(file, open_flags),
            self.device.clone(),
            open_flags,
        ));
        let opened_mut = Arc::get_mut(&mut opened).ok_or(StarryError::NoSuchDevice)?;
        // SAFETY: the new LinuxFile is zero-initialized and pinned inside the
        // Arc, as in `open_card0_file`.
        let ret = unsafe { drm_open_minor_index(minor_index as u32, opened_mut.raw()) };
        if ret != 0 {
            return Err(errno_from_linux(ret));
        }
        opened_mut.register_event_waiter()?;
        if open_flags & O_NONBLOCK != 0 {
            opened.set_nonblocking(true)?;
        }

        // Every fallible step is done: hand the unpublished drm_file to the
        // DRM core, then publish the descriptor.
        // SAFETY: the open above initialized `private_data`; no descriptor
        // refers to `opened` yet, and the caller vouches for `setup`/`ctx`.
        unsafe { setup((*opened.raw()).private_data.cast::<DrmFile>(), ctx) };
        let fd = reservation.fd();
        reservation.install(FileDescriptor {
            inner: opened,
            cloexec: flags & O_CLOEXEC != 0,
        });
        Ok(fd)
    }

    fn register_event_waiter(&mut self) -> StarryResult {
        // SAFETY: `drm_open_minor_index()` initialized `private_data` and its
        // event queue. The file is pinned inside the returned Arc before any
        // operation can access the embedded wait entry.
        unsafe {
            let private = (*self.raw()).private_data.cast::<DrmFile>();
            if private.is_null() {
                return Err(StarryError::NoSuchDevice);
            }
            let entry = self.wait_entry.get();
            (*entry).private = (&self.poll as *const PollSet).cast_mut().cast();
            (*entry).func = Some(wake_poll_waiters);
            init_list_head(ptr::addr_of_mut!((*entry).entry));
            add_wait_queue(ptr::addr_of_mut!((*private).event_wait), ptr::addr_of_mut!(*entry));
        }
        Ok(())
    }

    fn read_events(&self, dst: &mut IoDst) -> StarryResult<usize> {
        let remaining = dst.remaining_mut();
        let mut sink = IoDstSink { dst, remaining };
        // SAFETY: `raw` is a live DRM file. `sink` remains valid for this
        // synchronous call, and its callback only consumes initialized event
        // bytes supplied by kdrm-core.
        let result = unsafe {
            drm_read_kernel_to(
                self.raw(),
                sink.remaining,
                (&mut sink as *mut IoDstSink<'_>).cast(),
                copy_event_to_io_dst,
            )
        };
        if result < 0 {
            Err(errno_from_linux(result as i32))
        } else {
            Ok(result as usize)
        }
    }

    fn poll_events(&self) -> IoEvents {
        // SAFETY: `raw` is a live open DRM file. The null table asks the DRM
        // core only for its current readiness mask; Starry's persistent entry
        // was installed on event_wait during open.
        linux_poll_events(unsafe { drm_poll(self.raw(), ptr::null_mut()) })
    }
}

struct IoDstSink<'a> {
    dst: &'a mut IoDst<'a>,
    remaining: usize,
}

/// # Safety
/// If non-null, `context` must point to a live `IoDstSink` valid for mutable reference creation.
/// If non-null, `bytes` must point to an initialized byte buffer of at least `len` bytes.
unsafe extern "C" fn copy_event_to_io_dst(
    context: *mut c_void,
    bytes: *const c_void,
    len: usize,
) -> isize {
    if context.is_null() || bytes.is_null() {
        return -(EFAULT as isize);
    }
    // SAFETY: `context` points to the caller's stack `IoDstSink`, which is
    // live until the synchronous `drm_read_kernel_to` callback returns.
    let sink = unsafe { &mut *context.cast::<IoDstSink<'_>>() };
    if len > sink.remaining {
        return -(EFAULT as isize);
    }
    // SAFETY: kdrm-core supplies a contiguous, initialized DRM event of length
    // `len` for the duration of this callback.
    let source = unsafe { core::slice::from_raw_parts(bytes.cast::<u8>(), len) };
    match sink.dst.write_all(source) {
        Ok(()) => {
            sink.remaining -= len;
            len as isize
        }
        Err(_) => -(EFAULT as isize),
    }
}

/// # Safety
/// If non-null, `entry` must point to a valid, live `WaitQueueEntry` whose `private` field points to a live `PollSet`.
pub(super) unsafe extern "C" fn wake_poll_waiters(
    entry: *mut WaitQueueEntry,
    _mode: u32,
    _sync: c_int,
    key: *mut c_void,
) -> c_int {
    if entry.is_null() {
        return 0;
    }
    // SAFETY: Each registered entry stores a live `PollSet` pointer in
    // `private`; the entry is removed from DRM's queue before the PollSet drops.
    let poll = unsafe { (*entry).private.cast::<PollSet>() };
    if !poll.is_null() {
        // ax-drm sends RDIF completion and synthetic-vblank notifications
        // from task/deferred context. A future hardware IRQ path must defer
        // before entering the common DRM event-send path.
        // Unkeyed queue notifications are read readiness; keyed wakes also
        // carry terminal ERR/HUP when the shared device is unplugged.
        // SAFETY: The embedded PollSet is pinned until this wait entry is
        // removed, and ax-drm invokes DRM event wakeups from task/deferred
        // context.
        let events = if key.is_null() { IoEvents::IN | IoEvents::RDNORM } else { linux_poll_events(key as usize as u32) };
        unsafe { (&*poll).wake_all(events) };
    }
    1
}

pub(super) fn linux_poll_events(mask: u32) -> IoEvents {
    let mut events = IoEvents::empty();
    events.set(IoEvents::IN, mask & DRM_EPOLLIN != 0);
    events.set(IoEvents::OUT, mask & DRM_EPOLLOUT != 0);
    events.set(IoEvents::RDNORM, mask & DRM_EPOLLRDNORM != 0);
    events.set(IoEvents::WRNORM, mask & DRM_EPOLLWRNORM != 0);
    events.set(IoEvents::ERR, mask & DRM_EPOLLERR != 0);
    events.set(IoEvents::HUP, mask & DRM_EPOLLHUP != 0);
    events
}

fn errno_from_linux(result: i32) -> StarryError {
    let errno = result.saturating_neg();
    if errno == drm_kapi::config::ERESTARTSYS {
        // ERESTARTSYS is an internal Linux syscall-exit code. Let Starry's
        // signal-return path turn it into EINTR or restart the ioctl according
        // to the installed handler's SA_RESTART flag.
        StarryError::Interrupted
    } else {
        StarryError::from(crate::Errno::new(errno))
    }
}

impl Pollable for Card0File {
    fn poll(&self) -> IoEvents {
        self.poll_events()
    }

    /// # Safety
    /// The caller must ensure that `sink` remains valid for registration and that this registration is properly unregistered before destruction.
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
            // SAFETY: The caller guarantees that 'sink' is valid for registration; '&self.poll' lives as long as 'self'.
            unsafe { sink.register_shared(&self.poll, events) };
        }
    }

    /// # Safety
    /// The caller must ensure that `sink` remains valid for registration and that this registration is properly unregistered before destruction.
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
            // SAFETY: The caller guarantees that 'sink' is valid for registration; '&self.poll' lives as long as 'self'.
            unsafe { sink.register_exclusive(&self.poll, events) };
        }
    }
}

impl FileLike for Card0File {
    fn validate_write_access(&self) -> StarryResult {
        self.base.validate_write_access()
    }

    fn read(&self, dst: &mut IoDst) -> StarryResult<usize> {
        if dst.remaining_mut() == 0 {
            return Ok(0);
        }
        let task = current_user_task();
        block_on_user(
            &task,
            poll_io(self, IoEvents::IN, self.nonblocking(), || {
                self.read_events(dst)
            }),
        )
        .into_result()?
    }

    fn write(&self, _src: &mut IoSrc) -> StarryResult<usize> {
        Err(StarryError::BadFileDescriptor)
    }

    fn stat(&self) -> StarryResult<Kstat> {
        self.base.stat()
    }

    fn path(&self) -> Cow<'_, str> {
        self.base.path()
    }

    fn ioctl(&self, _current: &UserTaskRef, cmd: u32, arg: usize) -> StarryResult<usize> {
        // SAFETY: The VFS ioctl syscall validates the calling task context;
        // kdrm-core performs UAPI copy/permission checks and dispatches only
        // the ioctl registered by the common DRM driver.
        let result = unsafe { drm_ioctl(self.raw(), cmd, arg as c_ulong) };
        if result < 0 {
            Err(errno_from_linux(result))
        } else {
            Ok(result as usize)
        }
    }

    fn device_mmap(&self, offset: u64, length: u64) -> StarryResult<DeviceMmap> {
        // SAFETY: `private_data` is the live per-open drm_file installed by
        // the successful open path and is released only in this object's Drop.
        let private = unsafe { (*self.raw()).private_data.cast::<DrmFile>() };
        if private.is_null() {
            return Err(StarryError::NoSuchDevice);
        }
        // SAFETY: `private` is the live per-open drm_file for this borrowed
        // Card0File. The adapter retains its raw Linux file until Drop, and
        // this method keeps the FileLike borrow alive for the whole call.
        let lease = unsafe { ax_drm::mmap(private, offset, length) }.map_err(errno_from_linux)?;
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
        self.open_flags.load(Ordering::Acquire)
    }

    fn nonblocking(&self) -> bool {
        self.nonblocking.load(Ordering::Acquire)
    }

    fn set_nonblocking(&self, nonblocking: bool) -> StarryResult {
        let _guard = self.flags_lock.lock();
        self.base.set_nonblocking(nonblocking)?;
        let flags = if nonblocking {
            self.open_flags() | O_NONBLOCK
        } else {
            self.open_flags() & !O_NONBLOCK
        };
        self.open_flags.store(flags, Ordering::Release);
        self.nonblocking.store(nonblocking, Ordering::Release);
        Ok(())
    }
}

impl Drop for Card0File {
    fn drop(&mut self) {
        // SAFETY: this file owns its drm_file until `drm_release`; detach the
        // embedded waiter first so wakeups cannot touch freed PollSet storage.
        unsafe {
            let private = (*self.raw()).private_data.cast::<DrmFile>();
            if !private.is_null() {
                let entry = self.wait_entry.get();
                remove_wait_queue(ptr::addr_of_mut!((*private).event_wait), ptr::addr_of_mut!(*entry));
                drm_release(ptr::null_mut(), self.raw());
            }
        }
        let _ = &self.device;
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    #[test]
    fn restart_sys_is_deferred_to_signal_return_policy() {
        let error = errno_from_linux(-drm_kapi::config::ERESTARTSYS);
        assert!(matches!(error, StarryError::Interrupted));
        assert_eq!(error.linux_errno(), crate::Errno::EINTR);
    }
}
