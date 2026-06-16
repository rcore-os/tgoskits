use alloc::{
    collections::{BTreeMap, VecDeque},
    string::String,
    sync::Arc,
    vec,
    vec::Vec,
};
use core::{
    ffi::c_int,
    future::poll_fn,
    mem::{MaybeUninit, offset_of, size_of},
    slice,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_fs::FileFlags;
use ax_memory_addr::{MemoryAddr, VirtAddr, VirtAddrRange, align_up_4k};
use ax_runtime::hal::{
    paging::{MappingFlags, PageSize},
    time::wall_time,
};
use ax_sync::Mutex;
use ax_task::{
    WaitQueue,
    future::{block_on, interruptible, timeout_at_wall},
};
use axpoll::{IoEvents, PollSet};
use linux_raw_sys::general::timespec;
use spin::RwLock;
use starry_process::Pid;
use starry_signal::SignalSet;
use starry_vm::{VmMutPtr, VmPtr};

use crate::{
    file::{Directory, File, FileLike, event::EventFd, get_file_like, memfd::Memfd},
    mm::{AddrSpace, Backend, IoVec},
    syscall::signal::check_sigset_size,
    task::{AsThread, with_blocked_signals},
    time::TimeValueLike,
};

type AioContextId = usize;
type AioRequestId = u64;

// Linux AIO ring structure. Userspace libaio/MySQL reads this mapping directly.
#[repr(C)]
#[derive(Clone, Copy)]
struct AioRing {
    id: u32,
    nr: u32,
    head: u32,
    tail: u32,
    magic: u32,
    compat_features: u32,
    incompat_features: u32,
    header_length: u32,
}

const AIO_RING_MAGIC: u32 = 0xa10a10a1;
const AIO_RING_COMPAT_FEATURES: u32 = 1;
const AIO_RING_INCOMPAT_FEATURES: u32 = 0;

const IOCB_CMD_PREAD: u16 = 0;
const IOCB_CMD_PWRITE: u16 = 1;
const IOCB_CMD_FSYNC: u16 = 2;
const IOCB_CMD_FDSYNC: u16 = 3;
const IOCB_CMD_POLL: u16 = 5;
const IOCB_CMD_NOOP: u16 = 6;
const IOCB_CMD_PREADV: u16 = 7;
const IOCB_CMD_PWRITEV: u16 = 8;

const IOCB_FLAG_RESFD: u32 = 1;
const IOCB_FLAG_IOPRIO: u32 = 1 << 1;
const AIO_MAX_WORKERS: usize = 4;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IoEvent {
    data: u64,
    obj: u64,
    res: i64,
    res2: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Iocb {
    data: u64,
    #[cfg(target_endian = "little")]
    key: u32,
    rw_flags: u32,
    #[cfg(target_endian = "big")]
    key: u32,
    lio_opcode: u16,
    reqprio: i16,
    fildes: u32,
    buf: u64,
    nbytes: u64,
    offset: i64,
    reserved2: u64,
    flags: u32,
    resfd: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AioSigSet {
    sigmask: *const SignalSet,
    sigsetsize: usize,
}

#[derive(Clone)]
struct UserSegment {
    start: VirtAddr,
    len: usize,
}

struct UserBuffer {
    segments: Vec<UserSegment>,
    len: usize,
}

enum AioWriteTarget {
    File(Arc<File>),
    Memfd(Arc<Memfd>),
}

enum AioSyncTarget {
    File(Arc<File>),
    Directory(Arc<Directory>),
    Memfd(Arc<Memfd>),
}

enum AioOperation {
    Read {
        file: Arc<File>,
        offset: u64,
        dst: UserBuffer,
    },
    Write {
        target: AioWriteTarget,
        offset: u64,
        data: Vec<u8>,
    },
    Fsync {
        target: AioSyncTarget,
        data_only: bool,
    },
    Poll {
        file: Arc<dyn FileLike>,
        events: IoEvents,
    },
    Noop,
}

struct AioRequest {
    id: AioRequestId,
    cb_ptr: usize,
    data: u64,
    op: AioOperation,
    resfd: Option<Arc<EventFd>>,
}

struct PendingRequest {
    cb_ptr: usize,
    data: u64,
    running: bool,
}

struct AioContextInner {
    inflight: usize,
    queue: VecDeque<Arc<AioRequest>>,
    pending: BTreeMap<AioRequestId, PendingRequest>,
    worker_count: usize,
}

struct AioContext {
    id: AioContextId,
    owner: Pid,
    aspace: Arc<Mutex<AddrSpace>>,
    ring_vaddr: VirtAddr,
    ring_size: usize,
    ring_events: u32,
    ring_tail: AtomicUsize,
    ring_lock: Mutex<()>,
    ready_count: AtomicUsize,
    queued_count: AtomicUsize,
    destroying: AtomicBool,
    work_wq: WaitQueue,
    inflight_wq: WaitQueue,
    completion_wakers: PollSet,
    inner: Mutex<AioContextInner>,
}

impl AioContext {
    // Build a process-owned AIO context around a mapped user ring.
    fn new(
        id: AioContextId,
        owner: Pid,
        aspace: Arc<Mutex<AddrSpace>>,
        ring_vaddr: VirtAddr,
        ring_size: usize,
        ring_events: u32,
    ) -> Self {
        Self {
            id,
            owner,
            aspace,
            ring_vaddr,
            ring_size,
            ring_events,
            ring_tail: AtomicUsize::new(0),
            ring_lock: Mutex::new(()),
            ready_count: AtomicUsize::new(0),
            queued_count: AtomicUsize::new(0),
            destroying: AtomicBool::new(false),
            work_wq: WaitQueue::new(),
            inflight_wq: WaitQueue::new(),
            completion_wakers: PollSet::new(),
            inner: Mutex::new(AioContextInner {
                inflight: 0,
                queue: VecDeque::new(),
                pending: BTreeMap::new(),
                worker_count: 0,
            }),
        }
    }

    // Return usable completion slots, leaving one slot empty to distinguish full.
    fn capacity(&self) -> usize {
        self.ring_events.saturating_sub(1) as usize
    }
}

static NEXT_AIO_CONTEXT_ID: AtomicUsize = AtomicUsize::new(1);
static NEXT_AIO_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
static AIO_CONTEXTS: RwLock<BTreeMap<AioContextId, Arc<AioContext>>> = RwLock::new(BTreeMap::new());

// Return the process id that owns newly created or looked-up contexts.
fn current_pid() -> Pid {
    ax_task::current().as_thread().proc_data.proc.pid()
}

// Use Linux EINVAL for all invalid AIO context handles.
fn invalid_context() -> AxError {
    AxError::from(LinuxError::EINVAL)
}

// Return the byte size of the userspace AIO ring header.
fn aio_ring_header_size() -> usize {
    size_of::<AioRing>()
}

// Return the byte size of one userspace completion event.
fn aio_event_size() -> usize {
    size_of::<IoEvent>()
}

// Compute a page-aligned ring layout for the requested event count.
fn aio_ring_layout(nr_events: u32) -> AxResult<(usize, u32)> {
    let requested = usize::try_from(nr_events).map_err(|_| AxError::InvalidInput)?;
    let wanted_events = requested
        .checked_mul(2)
        .and_then(|events| events.checked_add(2))
        .ok_or(AxError::InvalidInput)?;
    let min_size = aio_ring_header_size()
        .checked_add(
            wanted_events
                .checked_mul(aio_event_size())
                .ok_or(AxError::InvalidInput)?,
        )
        .ok_or(AxError::InvalidInput)?;
    let ring_size = align_up_4k(min_size);
    let ring_events = (ring_size - aio_ring_header_size()) / aio_event_size();
    let ring_events = u32::try_from(ring_events).map_err(|_| AxError::InvalidInput)?;
    Ok((ring_size, ring_events))
}

// Reserve and map the userspace ring buffer in the process address space.
fn allocate_aio_ring(aspace: &mut AddrSpace, ring_size: usize) -> AxResult<VirtAddr> {
    let ring_vaddr = aspace
        .find_free_area(
            aspace.base(),
            ring_size,
            VirtAddrRange::new(aspace.base(), aspace.end()),
            PageSize::Size4K as usize,
        )
        .ok_or(AxError::NoMemory)?;

    let backend = Backend::new_alloc(ring_vaddr, PageSize::Size4K, "aio_ring");
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    aspace.map(ring_vaddr, ring_size, flags, true, backend)?;
    Ok(ring_vaddr)
}

// Create the initial Linux-compatible ring header.
fn initial_ring(ctx_id: AioContextId, ring_events: u32) -> AxResult<AioRing> {
    Ok(AioRing {
        id: u32::try_from(ctx_id).map_err(|_| AxError::NoMemory)?,
        nr: ring_events,
        head: 0,
        tail: 0,
        magic: AIO_RING_MAGIC,
        compat_features: AIO_RING_COMPAT_FEATURES,
        incompat_features: AIO_RING_INCOMPAT_FEATURES,
        header_length: u32::try_from(aio_ring_header_size()).map_err(|_| AxError::NoMemory)?,
    })
}

// Interpret the public context id as the userspace ring address.
fn ring_ptr(ctx: AioContextId) -> *mut AioRing {
    ctx as *mut AioRing
}

// Compute the address of a completion event inside the mapped ring.
fn ring_event_addr(context: &AioContext, index: u32) -> VirtAddr {
    context.ring_vaddr + aio_ring_header_size() + index as usize * aio_event_size()
}

// View a plain value as bytes for address-space writes.
fn typed_as_bytes<T>(value: &T) -> &[u8] {
    unsafe { slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) }
}

// View uninitialized storage as bytes for address-space reads.
fn typed_as_bytes_mut<T>(value: &mut MaybeUninit<T>) -> &mut [u8] {
    unsafe { slice::from_raw_parts_mut(value.as_mut_ptr().cast::<u8>(), size_of::<T>()) }
}

// Read the ring header through the caller's user pointer.
fn read_ring_user(ctx: AioContextId) -> AxResult<AioRing> {
    let ring = ring_ptr(ctx)
        .cast_const()
        .vm_read_uninit()
        .map_err(|_| invalid_context())?;
    Ok(unsafe { ring.assume_init() })
}

// Read the ring header from its owning address space.
fn read_ring_context(context: &AioContext) -> AxResult<AioRing> {
    let mut ring = MaybeUninit::<AioRing>::uninit();
    context
        .aspace
        .lock()
        .read(context.ring_vaddr, typed_as_bytes_mut(&mut ring))?;
    Ok(unsafe { ring.assume_init() })
}

// Store a new ring head after userspace events are drained.
fn write_ring_head_context(context: &AioContext, head: u32) -> AxResult<()> {
    context.aspace.lock().write(
        context.ring_vaddr + offset_of!(AioRing, head),
        typed_as_bytes(&head),
    )
}

// Store a new ring tail after the kernel enqueues a completion.
fn write_ring_tail_context(context: &AioContext, tail: u32) -> AxResult<()> {
    context.aspace.lock().write(
        context.ring_vaddr + offset_of!(AioRing, tail),
        typed_as_bytes(&tail),
    )
}

// Read one completion event from the ring.
fn read_event_context(context: &AioContext, index: u32) -> AxResult<IoEvent> {
    let mut event = MaybeUninit::<IoEvent>::uninit();
    context.aspace.lock().read(
        ring_event_addr(context, index),
        typed_as_bytes_mut(&mut event),
    )?;
    Ok(unsafe { event.assume_init() })
}

// Write one completion event into the ring.
fn write_event_context(context: &AioContext, index: u32, event: &IoEvent) -> AxResult<()> {
    context
        .aspace
        .lock()
        .write(ring_event_addr(context, index), typed_as_bytes(event))
}

// Validate a userspace context handle and return its kernel object.
fn lookup_context(ctx: AioContextId) -> AxResult<Arc<AioContext>> {
    let owner = current_pid();
    let ring = read_ring_user(ctx)?;
    let contexts = AIO_CONTEXTS.read();
    let ctx_id = ring.id as usize;
    match contexts.get(&ctx_id) {
        Some(context)
            if context.owner == owner
                && context.ring_vaddr.as_usize() == ctx
                && ring.magic == AIO_RING_MAGIC =>
        {
            Ok(context.clone())
        }
        _ => Err(invalid_context()),
    }
}

// Convert a syscall-style result into an io_event result field.
fn result_to_event_res(result: AxResult<isize>) -> i64 {
    match result {
        Ok(n) => n as i64,
        Err(err) => -(LinuxError::from(err).code() as i64),
    }
}

// Convert a user u64 length to this kernel's pointer-sized length.
fn u64_to_usize(value: u64) -> AxResult<usize> {
    usize::try_from(value).map_err(|_| AxError::InvalidInput)
}

// Convert an iocb offset into a non-negative file offset.
fn u64_to_offset(value: i64) -> AxResult<u64> {
    if value < 0 {
        Err(AxError::InvalidInput)
    } else {
        Ok(value as u64)
    }
}

// Fault in and validate a user memory range before worker access.
fn prepare_user_region(
    aspace: &Arc<Mutex<AddrSpace>>,
    start: VirtAddr,
    len: usize,
    flags: MappingFlags,
) -> AxResult<()> {
    if len == 0 {
        return Ok(());
    }
    let end = start
        .as_usize()
        .checked_add(len)
        .ok_or(AxError::BadAddress)?;
    let page_start = start.align_down_4k();
    let page_end = VirtAddr::from(end).align_up_4k();
    let mut guard = aspace.lock();
    if !guard.can_access_range(start, len, flags) {
        return Err(AxError::BadAddress);
    }
    guard.populate_area(page_start, page_end - page_start, flags)
}

// Copy a linear user buffer into owned kernel memory.
fn read_user_region(
    aspace: &Arc<Mutex<AddrSpace>>,
    start: VirtAddr,
    len: usize,
) -> AxResult<Vec<u8>> {
    prepare_user_region(aspace, start, len, MappingFlags::READ)?;
    let mut data = vec![0; len];
    if len != 0 {
        let guard = aspace.lock();
        if !guard.can_access_range(start, len, MappingFlags::READ) {
            return Err(AxError::BadAddress);
        }
        guard.read(start, &mut data)?;
    }
    Ok(data)
}

// Build a one-segment user buffer descriptor.
fn user_buffer_from_linear(
    aspace: &Arc<Mutex<AddrSpace>>,
    ptr: u64,
    len: usize,
    flags: MappingFlags,
) -> AxResult<UserBuffer> {
    let start = VirtAddr::from(usize::try_from(ptr).map_err(|_| AxError::BadAddress)?);
    prepare_user_region(aspace, start, len, flags)?;
    Ok(UserBuffer {
        segments: if len == 0 {
            Vec::new()
        } else {
            vec![UserSegment { start, len }]
        },
        len,
    })
}

// Read an iovec array and normalize zero-length entries away.
fn read_iov(iov: *const IoVec, iovcnt: usize) -> AxResult<Vec<UserSegment>> {
    if iovcnt > 1024 {
        return Err(AxError::InvalidInput);
    }
    let mut segments = Vec::with_capacity(iovcnt);
    for i in 0..iovcnt {
        let iov = iov.wrapping_add(i).vm_read()?;
        if iov.iov_len < 0 {
            return Err(AxError::InvalidInput);
        }
        let len = iov.iov_len as usize;
        if len != 0 {
            segments.push(UserSegment {
                start: VirtAddr::from(iov.iov_base as usize),
                len,
            });
        }
    }
    Ok(segments)
}

// Build a multi-segment user buffer from an iovec array.
fn user_buffer_from_iov(
    aspace: &Arc<Mutex<AddrSpace>>,
    iov: *const IoVec,
    iovcnt: usize,
    flags: MappingFlags,
) -> AxResult<UserBuffer> {
    let segments = read_iov(iov, iovcnt)?;
    let mut total = 0usize;
    for segment in &segments {
        prepare_user_region(aspace, segment.start, segment.len, flags)?;
        total = total
            .checked_add(segment.len)
            .filter(|len| *len <= isize::MAX as usize)
            .ok_or(AxError::InvalidInput)?;
    }
    Ok(UserBuffer {
        segments,
        len: total,
    })
}

// Copy all user segments into a contiguous kernel buffer.
fn read_user_segments(aspace: &Arc<Mutex<AddrSpace>>, buf: &UserBuffer) -> AxResult<Vec<u8>> {
    let mut data = vec![0; buf.len];
    let mut offset = 0usize;
    let guard = aspace.lock();
    for segment in &buf.segments {
        if !guard.can_access_range(segment.start, segment.len, MappingFlags::READ) {
            return Err(AxError::BadAddress);
        }
        guard.read(segment.start, &mut data[offset..offset + segment.len])?;
        offset += segment.len;
    }
    Ok(data)
}

// Copy a kernel buffer back into user segments.
fn write_user_segments(
    aspace: &Arc<Mutex<AddrSpace>>,
    buf: &UserBuffer,
    data: &[u8],
) -> AxResult<()> {
    let mut offset = 0usize;
    let guard = aspace.lock();
    for segment in &buf.segments {
        if offset >= data.len() {
            break;
        }
        let len = segment.len.min(data.len() - offset);
        if !guard.can_access_range(segment.start, len, MappingFlags::WRITE) {
            return Err(AxError::BadAddress);
        }
        guard.write(segment.start, &data[offset..offset + len])?;
        offset += len;
    }
    Ok(())
}

// Resolve an fd that can be used by asynchronous writes.
fn write_target_from_fd(fd: c_int) -> AxResult<AioWriteTarget> {
    if let Ok(memfd) = Memfd::from_fd(fd) {
        Ok(AioWriteTarget::Memfd(memfd))
    } else {
        let file = File::from_fd(fd).map_err(|e| {
            if e == AxError::IsADirectory {
                AxError::BadFileDescriptor
            } else if e == AxError::BadFileDescriptor {
                e
            } else {
                AxError::from(LinuxError::ESPIPE)
            }
        })?;
        let _ = file.inner().access(FileFlags::WRITE)?;
        Ok(AioWriteTarget::File(file))
    }
}

// Resolve an fd that can be used by asynchronous reads.
fn read_file_from_fd(fd: c_int) -> AxResult<Arc<File>> {
    File::from_fd(fd).map_err(|e| {
        if e == AxError::BadFileDescriptor || e == AxError::IsADirectory {
            e
        } else {
            AxError::from(LinuxError::ESPIPE)
        }
    })
}

// Resolve an fd that can handle fsync or fdatasync.
fn sync_target_from_fd(fd: c_int) -> AxResult<AioSyncTarget> {
    let file = get_file_like(fd)?;
    if let Ok(memfd) = file.clone().downcast_arc::<Memfd>() {
        Ok(AioSyncTarget::Memfd(memfd))
    } else if let Ok(file) = file.clone().downcast_arc::<File>() {
        Ok(AioSyncTarget::File(file))
    } else if let Ok(dir) = file.downcast_arc::<Directory>() {
        Ok(AioSyncTarget::Directory(dir))
    } else {
        Err(AxError::from(LinuxError::EINVAL))
    }
}

// Resolve the optional eventfd notification target from an iocb.
fn resolve_resfd(cb: &Iocb) -> AxResult<Option<Arc<EventFd>>> {
    if (cb.flags & IOCB_FLAG_RESFD) == 0 {
        Ok(None)
    } else {
        let file = get_file_like(cb.resfd as c_int)?;
        file.downcast_arc::<EventFd>()
            .map(Some)
            .map_err(|_| AxError::InvalidInput)
    }
}

// Validate iocb fields shared by all supported operations.
fn validate_iocb_common(cb: &Iocb) -> AxResult<()> {
    if cb.reserved2 != 0 {
        return Err(AxError::InvalidInput);
    }
    if (cb.flags & !(IOCB_FLAG_RESFD | IOCB_FLAG_IOPRIO)) != 0 {
        return Err(AxError::InvalidInput);
    }
    Ok(())
}

// Translate a userspace iocb into an owned request for worker execution.
fn prepare_request(
    context: &Arc<AioContext>,
    cb: &Iocb,
    cb_ptr: *const Iocb,
) -> AxResult<Arc<AioRequest>> {
    validate_iocb_common(cb)?;
    let resfd = resolve_resfd(cb)?;
    let fd = cb.fildes as c_int;
    // Snapshot or pin all user data before handing the request to a worker.
    let op = match cb.lio_opcode {
        IOCB_CMD_PREAD => {
            if cb.rw_flags != 0 {
                return Err(AxError::OperationNotSupported);
            }
            AioOperation::Read {
                file: read_file_from_fd(fd)?,
                offset: u64_to_offset(cb.offset)?,
                dst: user_buffer_from_linear(
                    &context.aspace,
                    cb.buf,
                    u64_to_usize(cb.nbytes)?,
                    MappingFlags::WRITE,
                )?,
            }
        }
        IOCB_CMD_PWRITE => {
            if cb.rw_flags != 0 {
                return Err(AxError::OperationNotSupported);
            }
            let start = VirtAddr::from(usize::try_from(cb.buf).map_err(|_| AxError::BadAddress)?);
            let len = u64_to_usize(cb.nbytes)?;
            let data = read_user_region(&context.aspace, start, len)?;
            AioOperation::Write {
                target: write_target_from_fd(fd)?,
                offset: u64_to_offset(cb.offset)?,
                data,
            }
        }
        IOCB_CMD_FSYNC => AioOperation::Fsync {
            target: sync_target_from_fd(fd)?,
            data_only: false,
        },
        IOCB_CMD_FDSYNC => AioOperation::Fsync {
            target: sync_target_from_fd(fd)?,
            data_only: true,
        },
        IOCB_CMD_POLL => {
            if cb.rw_flags != 0 {
                return Err(AxError::OperationNotSupported);
            }
            let events = IoEvents::from_bits(cb.buf as u32).ok_or(AxError::InvalidInput)?;
            AioOperation::Poll {
                file: get_file_like(fd)?,
                events: events | IoEvents::ALWAYS_POLL,
            }
        }
        IOCB_CMD_NOOP => AioOperation::Noop,
        IOCB_CMD_PREADV => {
            if cb.rw_flags != 0 {
                return Err(AxError::OperationNotSupported);
            }
            AioOperation::Read {
                file: read_file_from_fd(fd)?,
                offset: u64_to_offset(cb.offset)?,
                dst: user_buffer_from_iov(
                    &context.aspace,
                    cb.buf as *const IoVec,
                    u64_to_usize(cb.nbytes)?,
                    MappingFlags::WRITE,
                )?,
            }
        }
        IOCB_CMD_PWRITEV => {
            if cb.rw_flags != 0 {
                return Err(AxError::OperationNotSupported);
            }
            let src = user_buffer_from_iov(
                &context.aspace,
                cb.buf as *const IoVec,
                u64_to_usize(cb.nbytes)?,
                MappingFlags::READ,
            )?;
            let data = read_user_segments(&context.aspace, &src)?;
            AioOperation::Write {
                target: write_target_from_fd(fd)?,
                offset: u64_to_offset(cb.offset)?,
                data,
            }
        }
        _ => return Err(AxError::InvalidInput),
    };

    let id = NEXT_AIO_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    if id == 0 {
        return Err(AxError::NoMemory);
    }
    Ok(Arc::new(AioRequest {
        id,
        cb_ptr: cb_ptr as usize,
        data: cb.data,
        op,
        resfd,
    }))
}

// Signal an eventfd completion counter when IOCB_FLAG_RESFD is set.
fn notify_resfd(resfd: &EventFd) -> AxResult<()> {
    let data = 1u64.to_ne_bytes();
    resfd.write(&mut data.as_slice())?;
    Ok(())
}

// Execute a positioned read and copy the bytes into the original user buffer.
fn execute_read(
    context: &AioContext,
    file: &Arc<File>,
    offset: u64,
    dst: &UserBuffer,
) -> AxResult<isize> {
    let mut data = vec![0; dst.len];
    let read = file.inner().read_at(&mut data[..], offset)?;
    write_user_segments(&context.aspace, dst, &data[..read])?;
    Ok(read as isize)
}

// Execute a positioned write to a regular file or memfd.
fn execute_write(target: &AioWriteTarget, offset: u64, data: &[u8]) -> AxResult<isize> {
    match target {
        AioWriteTarget::File(file) => {
            let file = file.inner().access(FileFlags::WRITE)?;
            file.write_at(data, offset).map(|n| n as isize)
        }
        AioWriteTarget::Memfd(memfd) => memfd.write_at(data, offset).map(|n| n as isize),
    }
}

// Execute fsync or fdatasync against a supported target.
fn execute_fsync(target: &AioSyncTarget, data_only: bool) -> AxResult<isize> {
    match target {
        AioSyncTarget::File(file) => file.inner().sync(data_only)?,
        AioSyncTarget::Directory(dir) => dir.inner().sync(data_only)?,
        AioSyncTarget::Memfd(memfd) => memfd.inner().inner().sync(data_only)?,
    }
    Ok(0)
}

// Return ready poll events, including Linux always-reported error bits.
fn ready_poll_events(file: &Arc<dyn FileLike>, interested: IoEvents) -> Option<isize> {
    let mut ready = file.poll();
    if ready.contains(IoEvents::IN) {
        ready |= IoEvents::RDNORM;
    }
    if ready.contains(IoEvents::OUT) {
        ready |= IoEvents::WRNORM;
    }
    let always = ready & (IoEvents::ERR | IoEvents::HUP | IoEvents::RDHUP | IoEvents::NVAL);
    ready &= interested;
    ready |= always;
    (!ready.is_empty()).then_some(ready.bits() as isize)
}

// Wait until a poll request becomes ready or the context is destroyed.
fn poll_result(
    context: &AioContext,
    file: &Arc<dyn FileLike>,
    interested: IoEvents,
) -> AxResult<isize> {
    block_on(interruptible(poll_fn(|cx| {
        // Check before registration so already-ready fds complete immediately.
        if context.destroying.load(Ordering::Acquire) {
            return core::task::Poll::Ready(Err(AxError::Interrupted));
        }
        if let Some(ready) = ready_poll_events(file, interested) {
            return core::task::Poll::Ready(Ok(ready));
        }
        file.register(cx, interested);
        context.completion_wakers.register(cx.waker());
        // Re-check after registration to avoid losing a destroy or readiness wake.
        if context.destroying.load(Ordering::Acquire) {
            return core::task::Poll::Ready(Err(AxError::Interrupted));
        }
        if let Some(ready) = ready_poll_events(file, interested) {
            return core::task::Poll::Ready(Ok(ready));
        }
        core::task::Poll::Pending
    })))
    .map_err(AxError::from)?
}

// Dispatch one prepared request to the matching operation implementation.
fn execute_request(context: &AioContext, request: &AioRequest) -> AxResult<isize> {
    debug!(
        "execute_request: request_id={}, cb_ptr={:#x}",
        request.id, request.cb_ptr
    );
    let result = match &request.op {
        AioOperation::Read { file, offset, dst } => {
            debug!("execute_request: READ offset={}, len={}", offset, dst.len);
            execute_read(context, file, *offset, dst)
        }
        AioOperation::Write {
            target,
            offset,
            data,
        } => {
            debug!(
                "execute_request: WRITE offset={}, len={}",
                offset,
                data.len()
            );
            execute_write(target, *offset, data)
        }
        AioOperation::Fsync { target, data_only } => {
            debug!("execute_request: FSYNC data_only={}", data_only);
            execute_fsync(target, *data_only)
        }
        AioOperation::Poll { file, events } => {
            debug!("execute_request: POLL events={:?}", events);
            poll_result(context, file, *events)
        }
        AioOperation::Noop => {
            debug!("execute_request: NOOP");
            Ok(0)
        }
    };
    debug!(
        "execute_request: request_id={} completed, result={:?}",
        request.id, result
    );
    result
}

// Build the userspace completion event for a finished request.
fn completion_event(request: &AioRequest, result: AxResult<isize>) -> IoEvent {
    if let Some(resfd) = &request.resfd {
        let _ = notify_resfd(resfd);
    }
    IoEvent {
        data: request.data,
        obj: request.cb_ptr as u64,
        res: result_to_event_res(result),
        res2: 0,
    }
}

// Count events currently visible in the circular ring.
fn ring_ready_count(ring_events: u32, head: u32, tail: u32) -> usize {
    if ring_events == 0 {
        return 0;
    }
    if tail >= head {
        (tail - head) as usize
    } else {
        (ring_events - head + tail) as usize
    }
}

// Read and validate the user-visible ring head.
fn checked_ring_head(context: &AioContext) -> AxResult<u32> {
    let ring = read_ring_context(context)?;
    if ring.magic != AIO_RING_MAGIC || ring.nr != context.ring_events || ring.nr < 2 {
        return Err(invalid_context());
    }
    Ok(ring.head % context.ring_events)
}

// Recompute the cached ready count from ring head and tail.
fn refresh_ready_count(context: &AioContext) -> AxResult<usize> {
    let _ring = context.ring_lock.lock();
    let head = checked_ring_head(context)?;
    let tail = context.ring_tail.load(Ordering::Acquire) as u32 % context.ring_events;
    let ready = ring_ready_count(context.ring_events, head, tail);
    context.ready_count.store(ready, Ordering::Release);
    Ok(ready)
}

// Append one completion into the ring and wake waiters.
fn enqueue_completion(context: &AioContext, event: IoEvent) -> AxResult<()> {
    let _ring = context.ring_lock.lock();
    let head = checked_ring_head(context)?;
    let tail = context.ring_tail.load(Ordering::Acquire) as u32 % context.ring_events;
    let next_tail = if tail + 1 >= context.ring_events {
        0
    } else {
        tail + 1
    };
    if next_tail == head {
        return Err(AxError::WouldBlock);
    }

    // Event data must be visible before publishing the new tail.
    write_event_context(context, tail, &event)?;
    write_ring_tail_context(context, next_tail)?;
    context
        .ring_tail
        .store(next_tail as usize, Ordering::Release);
    context.ready_count.store(
        ring_ready_count(context.ring_events, head, next_tail),
        Ordering::Release,
    );
    context.completion_wakers.wake();
    Ok(())
}

// Remove a finished request from accounting and notify blocked paths.
fn finish_request(context: &AioContext, request: &AioRequest, event: IoEvent) {
    debug!(
        "finish_request: request_id={}, res={}",
        request.id, event.res
    );
    match enqueue_completion(context, event) {
        Ok(()) => {
            debug!("finish_request: enqueued completion successfully");
        }
        Err(e) => {
            warn!(
                "finish_request: enqueue_completion failed: {:?}, ring may be full",
                e
            );
        }
    }
    {
        let mut inner = context.inner.lock();
        inner.pending.remove(&request.id);
        inner.inflight = inner.inflight.saturating_sub(1);
        debug!(
            "finish_request: inflight={}, pending={}",
            inner.inflight,
            inner.pending.len()
        );
    }
    context.inflight_wq.notify_all(true);
    context.completion_wakers.wake();
}

// Pop the next queued request and mark it as running.
fn next_work(context: &Arc<AioContext>) -> Option<Arc<AioRequest>> {
    let mut inner = context.inner.lock();
    let request = inner.queue.pop_front()?;
    context.queued_count.fetch_sub(1, Ordering::AcqRel);
    if let Some(pending) = inner.pending.get_mut(&request.id) {
        pending.running = true;
        Some(request)
    } else {
        None
    }
}

// Worker loop that executes queued requests for one AIO context.
fn aio_worker(context: Arc<AioContext>) {
    debug!("aio_worker: started");
    loop {
        let request = loop {
            // Prefer existing queued work; otherwise block until work or destroy.
            if let Some(request) = next_work(&context) {
                debug!("aio_worker: got work, request_id={}", request.id);
                break request;
            }
            if context.destroying.load(Ordering::Acquire) {
                debug!("aio_worker: destroying flag set, exiting");
                return;
            }
            debug!("aio_worker: waiting for work");
            context.work_wq.wait_until(|| {
                context.queued_count.load(Ordering::Acquire) != 0
                    || context.destroying.load(Ordering::Acquire)
            });
        };

        // All completions pass through finish_request for accounting and wakeups.
        debug!("aio_worker: executing request_id={}", request.id);
        let result = execute_request(&context, &request);
        let event = completion_event(&request, result);
        finish_request(&context, &request, event);
        debug!("aio_worker: finished request_id={}", request.id);
    }
}

// Bound worker fan-out by both ring capacity and a fixed kernel limit.
fn max_worker_count(context: &AioContext) -> usize {
    context.capacity().clamp(1, AIO_MAX_WORKERS)
}

// Queue a request and start a worker if this context can use another one.
fn enqueue_request(context: &Arc<AioContext>, request: Arc<AioRequest>) -> AxResult<()> {
    refresh_ready_count(context)?;
    let spawn_worker = {
        let mut inner = context.inner.lock();
        if context.destroying.load(Ordering::Acquire) {
            return Err(invalid_context());
        }
        // Ring capacity covers both ready completions and in-flight work.
        let used = inner
            .inflight
            .checked_add(context.ready_count.load(Ordering::Acquire))
            .ok_or(AxError::InvalidInput)?;
        if used >= context.capacity() {
            return Err(AxError::WouldBlock);
        }
        inner.inflight += 1;
        inner.pending.insert(
            request.id,
            PendingRequest {
                cb_ptr: request.cb_ptr,
                data: request.data,
                running: false,
            },
        );
        inner.queue.push_back(request);
        context.queued_count.fetch_add(1, Ordering::AcqRel);
        if inner.worker_count < max_worker_count(context) {
            inner.worker_count += 1;
            true
        } else {
            false
        }
    };

    if spawn_worker {
        let worker_context = context.clone();
        ax_task::spawn_with_name(
            move || aio_worker(worker_context),
            String::from("aio-worker"),
        );
    }
    context.work_wq.notify_one(true);
    Ok(())
}

// Wait for at least one completion or for the optional deadline to expire.
fn wait_for_completion(
    context: &AioContext,
    deadline: Option<core::time::Duration>,
) -> AxResult<bool> {
    let wait = poll_fn(|cx| {
        // Register then re-check to avoid a completion wake racing this waiter.
        if context.ready_count.load(Ordering::Acquire) != 0
            || context.destroying.load(Ordering::Acquire)
        {
            core::task::Poll::Ready(())
        } else {
            context.completion_wakers.register(cx.waker());
            if context.ready_count.load(Ordering::Acquire) != 0
                || context.destroying.load(Ordering::Acquire)
            {
                core::task::Poll::Ready(())
            } else {
                core::task::Poll::Pending
            }
        }
    });

    match block_on(interruptible(timeout_at_wall(deadline, wait))) {
        Ok(Ok(())) => Ok(true),
        Ok(Err(_)) => Ok(false),
        Err(_) => Err(AxError::Interrupted),
    }
}

// Wait until io_destroy sees every running request finish.
fn wait_for_inflight_drain(context: &AioContext) {
    block_on(poll_fn(|cx| {
        let inflight = context.inner.lock().inflight;
        if inflight == 0 {
            return core::task::Poll::Ready(());
        }

        debug!("sys_io_destroy: still waiting, inflight={}", inflight);
        context.completion_wakers.register(cx.waker());
        // Re-check after registration so the final completion cannot be missed.
        if context.inner.lock().inflight == 0 {
            core::task::Poll::Ready(())
        } else {
            core::task::Poll::Pending
        }
    }))
}

// Read an optional relative timeout from userspace.
fn read_timeout(timeout: *const timespec) -> AxResult<Option<core::time::Duration>> {
    if timeout.is_null() {
        return Ok(None);
    }
    let timeout = unsafe { timeout.vm_read_uninit()?.assume_init() }.try_into_time_value()?;
    Ok(Some(timeout))
}

// Drain completion events from the ring into the userspace output array.
fn copy_completed_events(
    context: &AioContext,
    max: usize,
    events: *mut IoEvent,
    completed_offset: usize,
) -> AxResult<usize> {
    let mut copied = 0usize;
    let _ring = context.ring_lock.lock();
    let ring_events = context.ring_events;
    let mut head = checked_ring_head(context)?;
    let tail = context.ring_tail.load(Ordering::Acquire) as u32 % ring_events;

    while copied < max && head != tail {
        let event = read_event_context(context, head)?;
        // If a later copy fails, keep the events already delivered visible.
        if let Err(err) = events
            .wrapping_add(completed_offset + copied)
            .vm_write(event)
        {
            if copied > 0 {
                write_ring_head_context(context, head)?;
                context
                    .ready_count
                    .store(ring_ready_count(ring_events, head, tail), Ordering::Release);
                return Ok(copied);
            }
            context
                .ready_count
                .store(ring_ready_count(ring_events, head, tail), Ordering::Release);
            return Err(err.into());
        }
        copied += 1;
        head = if head + 1 >= ring_events { 0 } else { head + 1 };
    }

    // Publish the new head only after successful user copies.
    if copied > 0 {
        write_ring_head_context(context, head)?;
    }
    context
        .ready_count
        .store(ring_ready_count(ring_events, head, tail), Ordering::Release);
    Ok(copied)
}

// Shared implementation for io_getevents and io_pgetevents.
fn do_io_getevents(
    context: Arc<AioContext>,
    min_nr: isize,
    nr: isize,
    events: *mut IoEvent,
    timeout: *const timespec,
) -> AxResult<isize> {
    if min_nr < 0 || nr < 0 || min_nr > nr {
        return Err(AxError::InvalidInput);
    }
    if nr == 0 {
        return Ok(0);
    }

    let min_nr = min_nr as usize;
    let nr = nr as usize;
    let deadline = read_timeout(timeout)?.and_then(|duration| wall_time().checked_add(duration));
    let mut completed = 0usize;

    loop {
        // First drain everything already ready before sleeping.
        let copied = copy_completed_events(&context, nr - completed, events, completed)?;
        completed += copied;
        if completed >= min_nr || completed == nr || min_nr == 0 {
            return Ok(completed as isize);
        }
        if context.destroying.load(Ordering::Acquire) {
            return Ok(completed as isize);
        }

        // Sleep only when min_nr still requires more events.
        match wait_for_completion(&context, deadline) {
            Ok(true) => {}
            Ok(false) => return Ok(completed as isize),
            Err(_) if completed > 0 => return Ok(completed as isize),
            Err(err) => return Err(err),
        }
    }
}

// Create an AIO context and expose its ring address to userspace.
pub fn sys_io_setup(nr_events: u32, ctxp: *mut AioContextId) -> AxResult<isize> {
    debug!(
        "sys_io_setup called: nr_events={}, ctxp={:p}",
        nr_events, ctxp
    );
    if nr_events == 0 {
        return Err(AxError::InvalidInput);
    }
    if ctxp.cast_const().vm_read()? != 0 {
        return Err(AxError::InvalidInput);
    }

    let ctx_id = NEXT_AIO_CONTEXT_ID.fetch_add(1, Ordering::Relaxed);
    if ctx_id == 0 || u32::try_from(ctx_id).is_err() {
        return Err(AxError::NoMemory);
    }
    // Allocate the user ring before publishing the context globally.
    let (ring_size, ring_events) = aio_ring_layout(nr_events)?;
    let curr = ax_task::current();
    let aspace = curr.as_thread().proc_data.aspace();
    let ring_vaddr = {
        let mut guard = aspace.lock();
        allocate_aio_ring(&mut guard, ring_size)?
    };
    let ring = initial_ring(ctx_id, ring_events)?;
    aspace.lock().write(ring_vaddr, typed_as_bytes(&ring))?;

    let context = Arc::new(AioContext::new(
        ctx_id,
        current_pid(),
        aspace.clone(),
        ring_vaddr,
        ring_size,
        ring_events,
    ));
    AIO_CONTEXTS.write().insert(ctx_id, context);

    // If writing ctxp fails, roll back both the global entry and mapping.
    let ctx_value = ring_vaddr.as_usize();
    if let Err(err) = ctxp.vm_write(ctx_value) {
        AIO_CONTEXTS.write().remove(&ctx_id);
        let _ = aspace.lock().unmap(ring_vaddr, ring_size);
        return Err(err.into());
    }
    debug!(
        "sys_io_setup: success, ctx_id={:#x}, ring_vaddr={:#x}",
        ctx_id, ctx_value
    );
    Ok(0)
}

// Destroy an AIO context after cancelling queued work and draining workers.
fn destroy_context(context: Arc<AioContext>) {
    context.destroying.store(true, Ordering::Release);

    {
        // Drop queued requests; running requests are allowed to complete.
        let mut inner = context.inner.lock();
        let queued = inner.queue.len();
        inner.queue.clear();
        inner.pending.retain(|_, pending| pending.running);
        inner.inflight = inner.inflight.saturating_sub(queued);
        context.queued_count.store(0, Ordering::Release);
        warn!(
            "sys_io_destroy: cleared queue, inflight={}, pending={}",
            inner.inflight,
            inner.pending.len()
        );
    }
    context.work_wq.notify_all(true);
    context.completion_wakers.wake();

    // Wait outside the inner lock so workers can finish_request.
    warn!("sys_io_destroy: waiting for inflight to drain");
    wait_for_inflight_drain(&context);
    warn!("sys_io_destroy: all inflight drained");

    let mut aspace = context.aspace.lock();
    if let Err(err) = aspace.unmap(context.ring_vaddr, context.ring_size) {
        warn!("sys_io_destroy: failed to unmap ring buffer: {:?}", err);
    }
    debug!("sys_io_destroy: success");
}

// Destroy all AIO contexts owned by a process during last-thread exit.
pub fn cleanup_aio_contexts_for_pid(pid: Pid) {
    let contexts = {
        let mut table = AIO_CONTEXTS.write();
        let ids: Vec<_> = table
            .iter()
            .filter_map(|(&id, context)| (context.owner == pid).then_some(id))
            .collect();
        ids.into_iter()
            .filter_map(|id| table.remove(&id))
            .collect::<Vec<_>>()
    };

    for context in contexts {
        destroy_context(context);
    }
}

// Destroy an AIO context after cancelling queued work and draining workers.
pub fn sys_io_destroy(ctx: AioContextId) -> AxResult<isize> {
    debug!("sys_io_destroy called: ctx={:#x}", ctx);
    let context = lookup_context(ctx)?;
    let context = AIO_CONTEXTS
        .write()
        .remove(&context.id)
        .ok_or_else(invalid_context)?;
    destroy_context(context);
    Ok(0)
}

// Submit a batch of iocbs to the target AIO context.
pub fn sys_io_submit(ctx: AioContextId, nr: isize, iocbpp: *const *const Iocb) -> AxResult<isize> {
    debug!("sys_io_submit <= ctx: {ctx:#x}, nr: {nr}, iocbpp: {iocbpp:p}");
    if nr < 0 {
        return Err(AxError::InvalidInput);
    }
    if nr == 0 {
        lookup_context(ctx)?;
        return Ok(0);
    }
    let context = lookup_context(ctx)?;
    if context.destroying.load(Ordering::Acquire) {
        return Err(invalid_context());
    }

    let mut submitted = 0isize;
    for i in 0..nr as usize {
        // Linux returns a partial count once at least one request was queued.
        let cb_ptr = match iocbpp.wrapping_add(i).vm_read() {
            Ok(ptr) => ptr,
            Err(_) if submitted > 0 => return Ok(submitted),
            Err(err) => return Err(err.into()),
        };
        let cb = match cb_ptr.vm_read_uninit() {
            Ok(cb) => unsafe { cb.assume_init() },
            Err(_) if submitted > 0 => return Ok(submitted),
            Err(err) => return Err(err.into()),
        };
        debug!(
            "sys_io_submit: opcode={}, fd={}, offset={}, nbytes={}",
            cb.lio_opcode, cb.fildes, cb.offset, cb.nbytes
        );
        let request = match prepare_request(&context, &cb, cb_ptr) {
            Ok(request) => request,
            Err(_) if submitted > 0 => return Ok(submitted),
            Err(err) => return Err(err),
        };
        match enqueue_request(&context, request) {
            Ok(()) => {
                submitted += 1;
                debug!("sys_io_submit: enqueued, submitted={}", submitted);
            }
            Err(_) if submitted > 0 => return Ok(submitted),
            Err(err) => return Err(err),
        }
    }
    debug!("sys_io_submit => submitted={}", submitted);
    Ok(submitted)
}

// Retrieve completed events from an AIO context.
pub fn sys_io_getevents(
    ctx: AioContextId,
    min_nr: isize,
    nr: isize,
    events: *mut IoEvent,
    timeout: *const timespec,
) -> AxResult<isize> {
    debug!("sys_io_getevents <= ctx: {ctx:#x}, min_nr: {min_nr}, nr: {nr}, events: {events:p}");
    let context = lookup_context(ctx)?;
    let result = do_io_getevents(context, min_nr, nr, events, timeout)?;
    debug!("sys_io_getevents => result={}", result);
    Ok(result)
}

// Retrieve events while temporarily applying a signal mask.
pub fn sys_io_pgetevents(
    ctx: AioContextId,
    min_nr: isize,
    nr: isize,
    events: *mut IoEvent,
    timeout: *const timespec,
    sigmask: usize,
) -> AxResult<isize> {
    let context = lookup_context(ctx)?;
    if sigmask == 0 {
        return do_io_getevents(context, min_nr, nr, events, timeout);
    }

    let sigset = unsafe {
        (sigmask as *const AioSigSet)
            .vm_read_uninit()?
            .assume_init()
    };
    check_sigset_size(sigset.sigsetsize)?;
    // A null sigmask means pgetevents behaves like getevents.
    let blocked = if sigset.sigmask.is_null() {
        None
    } else {
        Some(unsafe { sigset.sigmask.vm_read_uninit()?.assume_init() })
    };
    with_blocked_signals(blocked, || {
        do_io_getevents(context, min_nr, nr, events, timeout)
    })
}

// Cancel a queued request that has not started running.
pub fn sys_io_cancel(
    ctx: AioContextId,
    iocb: *const Iocb,
    result: *mut IoEvent,
) -> AxResult<isize> {
    debug!("sys_io_cancel <= ctx: {ctx:#x}, iocb: {iocb:p}, result: {result:p}");
    let context = lookup_context(ctx)?;
    let cb_ptr = iocb as usize;

    let event = {
        let mut inner = context.inner.lock();
        // Linux AIO can only cancel requests still waiting in the queue here.
        let Some((&request_id, pending)) = inner
            .pending
            .iter()
            .find(|(_, pending)| pending.cb_ptr == cb_ptr)
        else {
            return Err(AxError::InvalidInput);
        };
        if pending.running {
            return Err(AxError::InvalidInput);
        }
        let pending = inner
            .pending
            .remove(&request_id)
            .ok_or(AxError::InvalidInput)?;
        let before = inner.queue.len();
        inner.queue.retain(|request| request.id != request_id);
        if inner.queue.len() != before {
            context.queued_count.fetch_sub(1, Ordering::AcqRel);
        }
        inner.inflight = inner.inflight.saturating_sub(1);
        IoEvent {
            data: pending.data,
            obj: cb_ptr as u64,
            res: -(LinuxError::ECANCELED.code() as i64),
            res2: 0,
        }
    };

    result.vm_write(event)?;
    context.inflight_wq.notify_all(true);
    context.work_wq.notify_one(true);
    context.completion_wakers.wake();
    Ok(0)
}
