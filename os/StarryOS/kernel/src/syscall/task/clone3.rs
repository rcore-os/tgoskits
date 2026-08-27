use core::mem::{self, MaybeUninit};

use ax_memory_addr::PAGE_SIZE_4K;
use ax_runtime::hal::cpu::uspace::UserContext;
use bytemuck::AnyBitPattern;
use starry_vm::{vm_load, vm_read_slice};

use super::clone::{CloneArgs, CloneFlags};
use crate::{
    StarryError, StarryResult,
    file::{ResolveAtResult, resolve_at},
};

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

fn clone3_check_extra_bytes(args: *const u8, size: usize) -> StarryResult<()> {
    let base_size = mem::size_of::<Clone3Args>();
    if size <= base_size {
        return Ok(());
    }

    let extra = vm_load(args.wrapping_add(base_size), size - base_size)?;
    if extra.iter().any(|byte| *byte != 0) {
        return Err(StarryError::ArgumentListTooLong);
    }
    Ok(())
}

impl TryFrom<Clone3Args> for CloneArgs {
    type Error = crate::StarryError;

    fn try_from(args: Clone3Args) -> StarryResult<Self> {
        if args.set_tid != 0 || args.set_tid_size != 0 {
            warn!("sys_clone3: set_tid/set_tid_size not supported, ignoring");
        }
        let flags = CloneFlags::from_bits_truncate(args.flags);

        if args.exit_signal > 0 && flags.intersects(CloneFlags::THREAD | CloneFlags::PARENT) {
            return Err(StarryError::InvalidInput);
        }
        if flags.contains(CloneFlags::DETACHED) {
            return Err(StarryError::InvalidInput);
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

pub fn sys_clone3(uctx: &UserContext, args: *const u8, size: usize) -> StarryResult<isize> {
    debug!("sys_clone3 <= args: {args:p}, size: {size}");

    if size > PAGE_SIZE_4K {
        return Err(StarryError::ArgumentListTooLong);
    }
    if size < MIN_CLONE_ARGS_SIZE {
        warn!("sys_clone3: size {size} too small, minimum is {MIN_CLONE_ARGS_SIZE}");
        return Err(StarryError::InvalidInput);
    }

    let mut buffer = [0u8; core::mem::size_of::<Clone3Args>()];
    let read_len = size.min(buffer.len());
    // SAFETY: MaybeUninit<T> is compatible with T, and we're filling in the
    // buffer with bytes read from the user
    vm_read_slice(args, unsafe {
        mem::transmute::<&mut [u8], &mut [MaybeUninit<u8>]>(&mut buffer[..read_len])
    })?;
    let clone3_args: Clone3Args =
        bytemuck::try_pod_read_unaligned(&buffer).map_err(|_| StarryError::InvalidInput)?;
    clone3_check_extra_bytes(args, size)?;

    let clone_args = CloneArgs::try_from(clone3_args)?;
    let requested_cgroup = if clone_args.flags.contains(CloneFlags::INTO_CGROUP) {
        let cgroup_fd = i32::try_from(clone3_args.cgroup).map_err(|_| StarryError::InvalidInput)?;
        let location = match resolve_at(cgroup_fd, None, linux_raw_sys::general::AT_EMPTY_PATH)? {
            ResolveAtResult::File(location) => location,
            ResolveAtResult::Other(_) => return Err(StarryError::InvalidInput),
        };
        Some(
            crate::pseudofs::cgroup::node_from_location(&location)
                .ok_or(StarryError::InvalidInput)?,
        )
    } else {
        if clone3_args.cgroup != 0 {
            return Err(StarryError::InvalidInput);
        }
        None
    };
    clone_args.do_clone_in_cgroup(uctx, requested_cgroup)
}

#[cfg(all(test, not(axtest)))]
fn clone3_validation_rules_hold_for_test() -> bool {
    use linux_raw_sys::general::{
        CLONE_DETACHED, CLONE_NEWPID, CLONE_PARENT, CLONE_THREAD, SIGCHLD,
    };

    let parent_signal_rejected = CloneArgs::try_from(Clone3Args {
        flags: CLONE_PARENT as u64,
        exit_signal: SIGCHLD as u64,
        ..Default::default()
    })
    .is_err();
    let thread_signal_rejected = CloneArgs::try_from(Clone3Args {
        flags: CLONE_THREAD as u64,
        exit_signal: SIGCHLD as u64,
        ..Default::default()
    })
    .is_err();
    let detached_rejected = CloneArgs::try_from(Clone3Args {
        flags: CLONE_DETACHED as u64,
        ..Default::default()
    })
    .is_err();
    let stack_top_is_derived_from_base_and_size = CloneArgs::try_from(Clone3Args {
        stack: 0x4000,
        stack_size: 0x2000,
        ..Default::default()
    })
    .is_ok_and(|args| args.stack == 0x6000);

    // Cover the remaining CloneArgs::try_from branches:
    //   - exit_signal == 0 + THREAD flag is accepted
    //   - exit_signal == 0 + PARENT flag is accepted
    //   - stack == 0 yields stack == 0 regardless of stack_size
    //   - stack > 0 with stack_size == 0 yields stack == args.stack directly
    let thread_zero_signal_accepted = CloneArgs::try_from(Clone3Args {
        flags: CLONE_THREAD as u64,
        exit_signal: 0,
        ..Default::default()
    })
    .is_ok();
    let parent_zero_signal_accepted = CloneArgs::try_from(Clone3Args {
        flags: CLONE_PARENT as u64,
        exit_signal: 0,
        ..Default::default()
    })
    .is_ok();
    let parent_newpid_zero_signal_accepted = CloneArgs::try_from(Clone3Args {
        flags: (CLONE_PARENT | CLONE_NEWPID) as u64,
        exit_signal: 0,
        ..Default::default()
    })
    .is_ok_and(|args| args.flags.contains(CloneFlags::PARENT | CloneFlags::NEWPID));
    let zero_stack_ignored_size = CloneArgs::try_from(Clone3Args {
        stack: 0,
        stack_size: 0x2000,
        ..Default::default()
    })
    .is_ok_and(|args| args.stack == 0);
    let stack_only_no_size = CloneArgs::try_from(Clone3Args {
        stack: 0x4000,
        stack_size: 0,
        ..Default::default()
    })
    .is_ok_and(|args| args.stack == 0x4000);
    // Plain clone (no flags, exit_signal = 0) is accepted.
    let plain_clone_accepted = CloneArgs::try_from(Clone3Args::default()).is_ok();
    // Plain clone with exit_signal SIGCHLD is accepted (matches sys_clone3 PID
    // inheritance semantics).
    let plain_clone_sigchld_accepted = CloneArgs::try_from(Clone3Args {
        exit_signal: SIGCHLD as u64,
        ..Default::default()
    })
    .is_ok();
    // Auxiliary fields propagate to CloneArgs unchanged.
    let auxiliary_fields_propagate = CloneArgs::try_from(Clone3Args {
        tls: 0xdead_beef,
        parent_tid: 0x1000,
        child_tid: 0x2000,
        pidfd: 0x3000,
        ..Default::default()
    })
    .is_ok_and(|args| {
        args.tls == 0xdead_beef
            && args.parent_tid == 0x1000
            && args.child_tid == 0x2000
            && args.pidfd == 0x3000
    });

    parent_signal_rejected
        && thread_signal_rejected
        && detached_rejected
        && stack_top_is_derived_from_base_and_size
        && thread_zero_signal_accepted
        && parent_zero_signal_accepted
        && parent_newpid_zero_signal_accepted
        && zero_stack_ignored_size
        && stack_only_no_size
        && plain_clone_accepted
        && plain_clone_sigchld_accepted
        && auxiliary_fields_propagate
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use linux_raw_sys::general::{CLONE_PARENT, CLONE_THREAD, SIGCHLD};

    use super::{Clone3Args, CloneArgs, clone3_validation_rules_hold_for_test};

    #[test]
    fn clone3_parent_rejects_nonzero_exit_signal() {
        let args = Clone3Args {
            flags: CLONE_PARENT as u64,
            exit_signal: SIGCHLD as u64,
            ..Default::default()
        };

        assert!(CloneArgs::try_from(args).is_err());
    }

    #[test]
    fn clone3_thread_rejects_nonzero_exit_signal() {
        let args = Clone3Args {
            flags: CLONE_THREAD as u64,
            exit_signal: SIGCHLD as u64,
            ..Default::default()
        };

        assert!(CloneArgs::try_from(args).is_err());
    }

    #[test]
    fn clone3_validation_rules_hold() {
        assert!(clone3_validation_rules_hold_for_test());
    }
}
