use core::{ffi::c_int, mem::size_of};

use ax_errno::{AxError, AxResult, LinuxError};
use linux_raw_sys::io_uring::{
    IORING_ENTER_GETEVENTS, IORING_SETUP_CLAMP, IORING_SETUP_CQSIZE, io_uring_params,
};
use starry_vm::{VmMutPtr, VmPtr};

use super::io::{
    sys_fdatasync, sys_fsync, sys_pread64, sys_preadv2, sys_pwrite64, sys_pwritev2, sys_read,
    sys_write,
};
use crate::{
    file::{FileLike, IoUring, io_uring::IoUringSqe},
    mm::IoVec,
};

const MAX_SQ_ENTRIES: u32 = 4096;
const MAX_CQ_ENTRIES: u32 = 8192;

const IORING_OP_NOP: u8 = 0;
const IORING_OP_READV: u8 = 1;
const IORING_OP_WRITEV: u8 = 2;
const IORING_OP_FSYNC: u8 = 3;
const IORING_OP_TIMEOUT: u8 = 11;
const IORING_OP_READ: u8 = 22;
const IORING_OP_WRITE: u8 = 23;

const IORING_FSYNC_DATASYNC: u32 = 1;
const IORING_REGISTER_EVENTFD: u32 = 4;
const IORING_UNREGISTER_EVENTFD: u32 = 5;
const IORING_REGISTER_EVENTFD_ASYNC: u32 = 7;
const IORING_REGISTER_PROBE: u32 = 8;

const IO_URING_OP_SUPPORTED: u16 = 1 << 0;

const SUPPORTED_SETUP_FLAGS: u32 = IORING_SETUP_CQSIZE | IORING_SETUP_CLAMP;
const SUPPORTED_ENTER_FLAGS: u32 = IORING_ENTER_GETEVENTS;
const SUPPORTED_OPS: [u8; 7] = [
    IORING_OP_NOP,
    IORING_OP_READV,
    IORING_OP_WRITEV,
    IORING_OP_FSYNC,
    IORING_OP_TIMEOUT,
    IORING_OP_READ,
    IORING_OP_WRITE,
];

#[repr(C)]
#[derive(Clone, Copy)]
struct IoUringProbeHeader {
    last_op: u8,
    ops_len: u8,
    resv: u16,
    resv2: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct IoUringProbeOp {
    op: u8,
    resv: u8,
    flags: u16,
    resv2: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KernelTimespec {
    tv_sec: i64,
    tv_nsec: i64,
}

const _: () = assert!(size_of::<IoUringProbeHeader>() == 16);
const _: () = assert!(size_of::<IoUringProbeOp>() == 8);

fn round_ring_entries(requested: u32, max: u32, clamp: bool) -> AxResult<u32> {
    if requested == 0 {
        return Err(AxError::InvalidInput);
    }
    let rounded = requested
        .checked_next_power_of_two()
        .ok_or(AxError::InvalidInput)?;
    if rounded > max {
        if clamp {
            Ok(max)
        } else {
            Err(AxError::InvalidInput)
        }
    } else {
        Ok(rounded)
    }
}

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

fn result_to_cqe_res(result: AxResult<isize>) -> i32 {
    match result {
        Ok(value) => value.try_into().unwrap_or(i32::MAX),
        Err(err) => -LinuxError::from(err).code(),
    }
}

fn execute_timeout(sqe: &IoUringSqe) -> AxResult<isize> {
    let ts = unsafe {
        (sqe.addr as *const KernelTimespec)
            .vm_read_uninit()?
            .assume_init()
    };
    if ts.tv_sec < 0 || !(0..1_000_000_000).contains(&ts.tv_nsec) {
        return Err(AxError::InvalidInput);
    }
    Ok(0)
}

fn execute_sqe(sqe: &IoUringSqe) -> i32 {
    if sqe.flags != 0 {
        return result_to_cqe_res(Err(AxError::OperationNotSupported));
    }

    let offset = sqe.off as i64;
    let result = match sqe.opcode {
        IORING_OP_NOP => Ok(0),
        IORING_OP_READV => sys_preadv2(
            sqe.fd,
            sqe.addr as *const IoVec,
            sqe.len as usize,
            offset,
            offset_hi(offset),
            sqe.rw_flags,
        ),
        IORING_OP_WRITEV => sys_pwritev2(
            sqe.fd,
            sqe.addr as *const IoVec,
            sqe.len as usize,
            offset,
            offset_hi(offset),
            sqe.rw_flags,
        ),
        IORING_OP_FSYNC => match sqe.rw_flags {
            0 => sys_fsync(sqe.fd),
            IORING_FSYNC_DATASYNC => sys_fdatasync(sqe.fd),
            _ => Err(AxError::InvalidInput),
        },
        IORING_OP_TIMEOUT => execute_timeout(sqe),
        IORING_OP_READ => {
            if sqe.rw_flags != 0 {
                Err(AxError::OperationNotSupported)
            } else if offset == -1 {
                sys_read(sqe.fd, sqe.addr as *mut u8, sqe.len as usize)
            } else {
                sys_pread64(sqe.fd, sqe.addr as *mut u8, sqe.len as usize, offset)
            }
        }
        IORING_OP_WRITE => {
            if sqe.rw_flags != 0 {
                Err(AxError::OperationNotSupported)
            } else if offset == -1 {
                sys_write(sqe.fd, sqe.addr as *mut u8, sqe.len as usize)
            } else {
                sys_pwrite64(sqe.fd, sqe.addr as *const u8, sqe.len as usize, offset)
            }
        }
        _ => Err(AxError::OperationNotSupported),
    };

    result_to_cqe_res(result)
}

fn setup_entries(entries: u32, params: &io_uring_params) -> AxResult<(u32, u32)> {
    if params.flags & !SUPPORTED_SETUP_FLAGS != 0 {
        return Err(AxError::InvalidInput);
    }
    let clamp = params.flags & IORING_SETUP_CLAMP != 0;
    let sq_entries = round_ring_entries(entries, MAX_SQ_ENTRIES, clamp)?;
    let cq_entries = if params.flags & IORING_SETUP_CQSIZE != 0 {
        if params.cq_entries < entries {
            return Err(AxError::InvalidInput);
        }
        round_ring_entries(params.cq_entries, MAX_CQ_ENTRIES, clamp)?
    } else {
        sq_entries
            .checked_mul(2)
            .filter(|entries| *entries <= MAX_CQ_ENTRIES)
            .unwrap_or(MAX_CQ_ENTRIES)
    };
    Ok((sq_entries, cq_entries))
}

pub fn sys_io_uring_setup(entries: u32, params: *mut io_uring_params) -> AxResult<isize> {
    debug!("sys_io_uring_setup <= entries: {entries}, params: {params:p}");
    let mut params_value = unsafe { params.vm_read_uninit()?.assume_init() };
    let (sq_entries, cq_entries) = setup_entries(entries, &params_value)?;

    let ring = IoUring::new(sq_entries, cq_entries)?;
    ring.fill_params(&mut params_value);
    params.vm_write(params_value)?;
    ring.add_to_fd_table(false).map(|fd| fd as isize)
}

pub fn sys_io_uring_enter(
    fd: c_int,
    to_submit: usize,
    _min_complete: usize,
    flags: u32,
    _sig: usize,
    _sigsz: usize,
) -> AxResult<isize> {
    debug!("sys_io_uring_enter <= fd: {fd}, to_submit: {to_submit}, flags: {flags:#x}");
    if flags & !SUPPORTED_ENTER_FLAGS != 0 {
        return Err(AxError::InvalidInput);
    }
    let to_submit = u32::try_from(to_submit).map_err(|_| AxError::InvalidInput)?;
    let ring = IoUring::from_fd(fd)?;
    ring.submit(to_submit, execute_sqe)
        .map(|submitted| submitted as isize)
}

fn write_probe(arg: *mut u8, nr_args: usize) -> AxResult<isize> {
    let ops_len = SUPPORTED_OPS.len().min(nr_args).min(u8::MAX as usize);
    let header = IoUringProbeHeader {
        last_op: IORING_OP_WRITE,
        ops_len: ops_len as u8,
        resv: 0,
        resv2: [0; 3],
    };
    (arg as *mut IoUringProbeHeader).vm_write(header)?;

    for (idx, op) in SUPPORTED_OPS.iter().copied().take(ops_len).enumerate() {
        let probe_op = IoUringProbeOp {
            op,
            resv: 0,
            flags: IO_URING_OP_SUPPORTED,
            resv2: 0,
        };
        (arg.wrapping_add(size_of::<IoUringProbeHeader>())
            .wrapping_add(idx * size_of::<IoUringProbeOp>()) as *mut IoUringProbeOp)
            .vm_write(probe_op)?;
    }

    Ok(0)
}

pub fn sys_io_uring_register(
    fd: c_int,
    opcode: u32,
    arg: usize,
    nr_args: usize,
) -> AxResult<isize> {
    debug!("sys_io_uring_register <= fd: {fd}, opcode: {opcode}, nr_args: {nr_args}");
    let _ring = IoUring::from_fd(fd)?;
    match opcode {
        IORING_REGISTER_PROBE => write_probe(arg as *mut u8, nr_args),
        IORING_REGISTER_EVENTFD | IORING_UNREGISTER_EVENTFD | IORING_REGISTER_EVENTFD_ASYNC => {
            Err(AxError::OperationNotSupported)
        }
        _ => Err(AxError::OperationNotSupported),
    }
}
