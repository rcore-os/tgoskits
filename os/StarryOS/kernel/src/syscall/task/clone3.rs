use core::mem::{self, MaybeUninit};

use ax_errno::{AxError, AxResult};
use ax_hal::uspace::UserContext;
use bytemuck::AnyBitPattern;
use starry_vm::vm_read_slice;

use super::clone::{CloneArgs, CloneFlags};

/// Structure passed to clone3() system call.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, AnyBitPattern)]
pub struct Clone3Args {
    pub flags: u64,
    pub pidfd: u64,
    pub child_tid: u64,
    pub parent_tid: u64,
    pub exit_signal: u64,
    pub stack: u64,
    pub stack_size: u64,
    pub tls: u64,
    pub set_tid: u64,
    pub set_tid_size: u64,
    pub cgroup: u64,
}

const MIN_CLONE_ARGS_SIZE: usize = core::mem::size_of::<u64>() * 8;

impl TryFrom<Clone3Args> for CloneArgs {
    type Error = ax_errno::AxError;

    fn try_from(args: Clone3Args) -> AxResult<Self> {
        if args.set_tid != 0 || args.set_tid_size != 0 {
            warn!("sys_clone3: set_tid/set_tid_size not supported, ignoring");
        }
        if args.cgroup != 0 {
            warn!("sys_clone3: cgroup parameter not supported, ignoring");
        }

        let flags = CloneFlags::from_bits_truncate(args.flags);

        if args.exit_signal > 0 && flags.intersects(CloneFlags::THREAD | CloneFlags::PARENT) {
            return Err(AxError::InvalidInput);
        }
        if flags.contains(CloneFlags::DETACHED) {
            return Err(AxError::InvalidInput);
        }

        let stack = if args.stack > 0 {
            if args.stack_size > 0 {
                (args.stack + args.stack_size) as usize
            } else {
                args.stack as usize
            }
        } else {
            0
        };

        Ok(CloneArgs {
            flags,
            exit_signal: args.exit_signal,
            stack,
            tls: args.tls as usize,
            parent_tid: args.parent_tid as usize,
            child_tid: args.child_tid as usize,
            pidfd: args.pidfd as usize,
        })
    }
}

pub fn sys_clone3(uctx: &UserContext, args: *const u8, size: usize) -> AxResult<isize> {
    debug!("sys_clone3 <= args: {args:p}, size: {size}");

    if size < MIN_CLONE_ARGS_SIZE {
        warn!("sys_clone3: size {size} too small, minimum is {MIN_CLONE_ARGS_SIZE}");
        return Err(AxError::InvalidInput);
    }

    if size > core::mem::size_of::<Clone3Args>() {
        debug!("sys_clone3: size {size} larger than expected, using known fields only");
    }

    let mut buffer = [0u8; core::mem::size_of::<Clone3Args>()];
    let read_len = size.min(buffer.len());
    // SAFETY: MaybeUninit<T> is compatible with T, and we're filling in the
    // buffer with bytes read from the user
    vm_read_slice(args, unsafe {
        mem::transmute::<&mut [u8], &mut [MaybeUninit<u8>]>(&mut buffer[..read_len])
    })?;
    let clone3_args: Clone3Args =
        bytemuck::try_pod_read_unaligned(&buffer).map_err(|_| AxError::InvalidInput)?;

    let clone_args = CloneArgs::try_from(clone3_args)?;
    clone_args.do_clone(uctx)
}
