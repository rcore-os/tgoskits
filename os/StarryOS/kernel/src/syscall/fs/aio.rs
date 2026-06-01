use alloc::collections::{BTreeMap, VecDeque};
use core::{
    ffi::c_int,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_errno::{AxError, AxResult, LinuxError};
use linux_raw_sys::general::{__kernel_off_t, timespec};
use spin::RwLock;
use starry_process::Pid;
use starry_vm::{VmMutPtr, VmPtr};

use super::io::{sys_fdatasync, sys_fsync, sys_pread64, sys_preadv2, sys_pwrite64, sys_pwritev2};
use crate::{file::get_file_like, mm::IoVec, task::AsThread};

type AioContextId = usize;

const IOCB_CMD_PREAD: u16 = 0;
const IOCB_CMD_PWRITE: u16 = 1;
const IOCB_CMD_FSYNC: u16 = 2;
const IOCB_CMD_FDSYNC: u16 = 3;
const IOCB_CMD_POLL: u16 = 5;
const IOCB_CMD_NOOP: u16 = 6;
const IOCB_CMD_PREADV: u16 = 7;
const IOCB_CMD_PWRITEV: u16 = 8;

const IOCB_FLAG_RESFD: u32 = 1;
const MAX_PREALLOC_EVENTS: usize = 1024;

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

struct AioContext {
    owner: Pid,
    events: VecDeque<IoEvent>,
}

impl AioContext {
    // Create an AIO context owned by one process.
    fn new(owner: Pid, nr_events: u32) -> Self {
        Self {
            owner,
            events: VecDeque::with_capacity((nr_events as usize).min(MAX_PREALLOC_EVENTS)),
        }
    }
}

static NEXT_AIO_CONTEXT_ID: AtomicUsize = AtomicUsize::new(1);
static AIO_CONTEXTS: RwLock<BTreeMap<AioContextId, AioContext>> = RwLock::new(BTreeMap::new());

// Return the current process id used to scope AIO contexts.
fn current_pid() -> Pid {
    ax_task::current().as_thread().proc_data.proc.pid()
}

// Build the Linux-compatible error for an invalid AIO context.
fn invalid_context() -> AxError {
    AxError::from(LinuxError::EINVAL)
}

// Check whether a context belongs to the expected process.
fn context_belongs_to(context: &AioContext, owner: Pid) -> bool {
    context.owner == owner
}

// Validate that the AIO context exists for the current process.
fn check_context(ctx_id: AioContextId) -> AxResult<()> {
    let owner = current_pid();
    let contexts = AIO_CONTEXTS.read();
    match contexts.get(&ctx_id) {
        Some(context) if context_belongs_to(context, owner) => Ok(()),
        _ => Err(invalid_context()),
    }
}

// Queue one completed event into an AIO context.
fn enqueue_event(ctx_id: AioContextId, event: IoEvent) -> AxResult<()> {
    let owner = current_pid();
    let mut contexts = AIO_CONTEXTS.write();
    let context = contexts
        .get_mut(&ctx_id)
        .filter(|context| context_belongs_to(context, owner))
        .ok_or_else(invalid_context)?;
    context.events.push_back(event);
    Ok(())
}

// Convert an operation result into the signed Linux AIO event result.
fn result_to_event_res(result: AxResult<isize>) -> i64 {
    match result {
        Ok(n) => n as i64,
        Err(err) => -(LinuxError::from(err).code() as i64),
    }
}

// Convert a userspace length field into a native usize.
fn u64_to_usize(value: u64) -> AxResult<usize> {
    usize::try_from(value).map_err(|_| AxError::InvalidInput)
}

// Extract the high offset word for 32-bit preadv2/pwritev2 ABI helpers.
fn offset_hi(offset: i64) -> usize {
    #[cfg(target_pointer_width = "32")]
    {
        ((offset as u64) >> 32) as usize
    }

    #[cfg(target_pointer_width = "64")]
    {
        let _ = offset;
        0
    }
}

// Notify an eventfd completion target when IOCB_FLAG_RESFD is set.
fn notify_resfd(resfd: u32) -> AxResult<()> {
    let file = get_file_like(resfd as c_int)?;
    let data = 1u64.to_ne_bytes();
    file.write(&mut data.as_slice())?;
    Ok(())
}

// Execute one iocb synchronously through existing file syscalls.
fn execute_iocb(cb: &Iocb) -> AxResult<isize> {
    if (cb.flags & !IOCB_FLAG_RESFD) != 0 {
        return Err(AxError::InvalidInput);
    }

    // Dispatch only the common file operations needed by the compatibility layer.
    match cb.lio_opcode {
        IOCB_CMD_PREAD => {
            if cb.rw_flags != 0 {
                return Err(AxError::OperationNotSupported);
            }
            sys_pread64(
                cb.fildes as c_int,
                cb.buf as *mut u8,
                u64_to_usize(cb.nbytes)?,
                cb.offset as __kernel_off_t,
            )
        }
        IOCB_CMD_PWRITE => {
            if cb.rw_flags != 0 {
                return Err(AxError::OperationNotSupported);
            }
            sys_pwrite64(
                cb.fildes as c_int,
                cb.buf as *const u8,
                u64_to_usize(cb.nbytes)?,
                cb.offset as __kernel_off_t,
            )
        }
        IOCB_CMD_FSYNC => sys_fsync(cb.fildes as c_int),
        IOCB_CMD_FDSYNC => sys_fdatasync(cb.fildes as c_int),
        IOCB_CMD_NOOP => Ok(0),
        IOCB_CMD_PREADV => sys_preadv2(
            cb.fildes as c_int,
            cb.buf as *const IoVec,
            u64_to_usize(cb.nbytes)?,
            cb.offset as __kernel_off_t,
            offset_hi(cb.offset),
            cb.rw_flags,
        ),
        IOCB_CMD_PWRITEV => sys_pwritev2(
            cb.fildes as c_int,
            cb.buf as *const IoVec,
            u64_to_usize(cb.nbytes)?,
            cb.offset as __kernel_off_t,
            offset_hi(cb.offset),
            cb.rw_flags,
        ),
        IOCB_CMD_POLL => Err(AxError::OperationNotSupported),
        _ => Err(AxError::InvalidInput),
    }
}

// Build the completion event for one submitted iocb.
fn complete_iocb(cb: &Iocb, cb_ptr: *const Iocb) -> IoEvent {
    let result = execute_iocb(cb);
    if result.is_ok() && (cb.flags & IOCB_FLAG_RESFD) != 0 {
        // Best-effort eventfd notification; the AIO result stays in the queue.
        let _ = notify_resfd(cb.resfd);
    }

    IoEvent {
        data: cb.data,
        obj: cb_ptr as u64,
        res: result_to_event_res(result),
        res2: 0,
    }
}

// Create a process-local AIO context for compatibility.
pub fn sys_io_setup(nr_events: u32, ctxp: *mut AioContextId) -> AxResult<isize> {
    debug!("sys_io_setup <= nr_events: {nr_events}, ctxp: {ctxp:p}");
    // Linux expects a non-zero ring size and an empty user context slot.
    if nr_events == 0 {
        return Err(AxError::InvalidInput);
    }
    if ctxp.cast_const().vm_read()? != 0 {
        return Err(AxError::InvalidInput);
    }

    let ctx_id = NEXT_AIO_CONTEXT_ID.fetch_add(1, Ordering::Relaxed);
    if ctx_id == 0 {
        return Err(AxError::NoMemory);
    }

    // Keep contexts process-local so stale ids from other processes are rejected.
    AIO_CONTEXTS
        .write()
        .insert(ctx_id, AioContext::new(current_pid(), nr_events));
    ctxp.vm_write(ctx_id)?;
    Ok(0)
}

// Destroy a process-local AIO context and drop pending events.
pub fn sys_io_destroy(ctx_id: AioContextId) -> AxResult<isize> {
    debug!("sys_io_destroy <= ctx_id: {ctx_id:#x}");
    let owner = current_pid();
    let mut contexts = AIO_CONTEXTS.write();
    match contexts.get(&ctx_id) {
        Some(context) if context_belongs_to(context, owner) => {
            contexts.remove(&ctx_id);
            Ok(0)
        }
        _ => Err(invalid_context()),
    }
}

// Submit iocbs and complete supported operations synchronously.
pub fn sys_io_submit(
    ctx_id: AioContextId,
    nr: isize,
    iocbpp: *const *const Iocb,
) -> AxResult<isize> {
    debug!("sys_io_submit <= ctx_id: {ctx_id:#x}, nr: {nr}, iocbpp: {iocbpp:p}");
    // Validate the request before touching the userspace iocb array.
    if nr < 0 {
        return Err(AxError::InvalidInput);
    }
    if nr == 0 {
        return Ok(0);
    }
    check_context(ctx_id)?;

    let mut submitted = 0isize;
    for i in 0..nr as usize {
        // Read each userspace iocb pointer, then copy the iocb itself.
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

        // Operations complete synchronously; only their completion event is queued.
        let event = complete_iocb(&cb, cb_ptr);
        match enqueue_event(ctx_id, event) {
            Ok(()) => submitted += 1,
            Err(_) if submitted > 0 => return Ok(submitted),
            Err(err) => return Err(err),
        }
    }

    Ok(submitted)
}

// Copy queued AIO completions back to userspace.
pub fn sys_io_getevents(
    ctx_id: AioContextId,
    min_nr: isize,
    nr: isize,
    events: *mut IoEvent,
    _timeout: *const timespec,
) -> AxResult<isize> {
    debug!(
        "sys_io_getevents <= ctx_id: {ctx_id:#x}, min_nr: {min_nr}, nr: {nr}, events: {events:p}"
    );
    // min_nr cannot exceed the maximum number of events requested.
    if min_nr < 0 || nr < 0 || min_nr > nr {
        return Err(AxError::InvalidInput);
    }
    if nr == 0 {
        check_context(ctx_id)?;
        return Ok(0);
    }

    let owner = current_pid();
    let mut completed = 0usize;
    let mut contexts = AIO_CONTEXTS.write();
    let context = contexts
        .get_mut(&ctx_id)
        .filter(|context| context_belongs_to(context, owner))
        .ok_or_else(invalid_context)?;

    while completed < nr as usize {
        let Some(event) = context.events.front().copied() else {
            break;
        };
        // Write first, then pop, so EFAULT does not lose a completion.
        if let Err(err) = events.wrapping_add(completed).vm_write(event) {
            if completed > 0 {
                return Ok(completed as isize);
            }
            return Err(err.into());
        }
        context.events.pop_front();
        completed += 1;
    }

    Ok(completed as isize)
}

// Handle io_pgetevents with the same non-blocking completion queue.
pub fn sys_io_pgetevents(
    ctx_id: AioContextId,
    min_nr: isize,
    nr: isize,
    events: *mut IoEvent,
    timeout: *const timespec,
    _sigmask: usize,
) -> AxResult<isize> {
    sys_io_getevents(ctx_id, min_nr, nr, events, timeout)
}

// Report cancellation as unavailable because operations complete immediately.
pub fn sys_io_cancel(
    ctx_id: AioContextId,
    _iocb: *const Iocb,
    _result: *mut IoEvent,
) -> AxResult<isize> {
    debug!("sys_io_cancel <= ctx_id: {ctx_id:#x}");
    check_context(ctx_id)?;
    Err(AxError::InvalidInput)
}
