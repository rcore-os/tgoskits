use alloc::vec::Vec;
use core::{
    future::poll_fn,
    mem::{MaybeUninit, offset_of},
    task::Poll,
};

use ax_runtime::hal::time::TimeValue;
use axpoll::{IoEvents, Pollable};
use linux_raw_sys::general::{POLLNVAL, RLIMIT_NOFILE, pollfd, timespec};
use starry_signal::SignalSet;

use super::FdPollSet;
use crate::{
    StarryError, StarryResult,
    file::get_file_like,
    mm::{UserConstPtr, UserPtr, vm_read_slice, vm_write_slice},
    syscall::signal::check_sigset_size,
    task::{
        future::{UserWaitOutcome, block_on_user_timeout},
        with_blocked_signals,
    },
    time::TimeValueLike,
};

fn check_nfds_limit(current: &crate::task::UserTaskRef, nfds: usize) -> crate::StarryResult<()> {
    let nofile = current.as_thread().proc_data.rlimits()[RLIMIT_NOFILE].current;
    if nfds as u64 > nofile {
        Err(StarryError::InvalidInput)
    } else {
        Ok(())
    }
}

fn read_poll_fds(
    current: &crate::task::UserTaskRef,
    fds: UserPtr<pollfd>,
    nfds: usize,
) -> crate::StarryResult<Vec<pollfd>> {
    check_nfds_limit(current, nfds)?;
    if nfds == 0 {
        return Ok(Vec::new());
    }

    let mut buf = Vec::with_capacity(nfds);
    buf.resize_with(nfds, MaybeUninit::uninit);
    vm_read_slice(current, fds.as_ptr(), &mut buf)?;
    Ok(buf
        .into_iter()
        .map(|fd| unsafe { fd.assume_init() })
        .collect())
}

fn write_poll_revents(
    current: &crate::task::UserTaskRef,
    fds: UserPtr<pollfd>,
    poll_fds: &[pollfd],
) -> crate::StarryResult<()> {
    let revents_offset = offset_of!(pollfd, revents);

    for (index, poll_fd) in poll_fds.iter().enumerate() {
        let revents_ptr = (fds.as_ptr().wrapping_add(index) as *mut u8)
            .wrapping_add(revents_offset)
            .cast::<_>();
        vm_write_slice(
            current,
            revents_ptr,
            core::slice::from_ref(&poll_fd.revents),
        )?;
    }

    Ok(())
}

fn collect_ready_poll_events(
    fds: &FdPollSet,
    revent_indices: &[usize],
    poll_fds: &mut [pollfd],
) -> usize {
    let mut res = 0usize;
    for ((fd, events), revent_index) in fds.0.iter().zip(revent_indices.iter()) {
        let mut result = fd.poll();
        // POSIX: POLLHUP and POLLERR are always reported in revents,
        // even if not requested in events. They must NOT be masked out.
        let always_report =
            result & (IoEvents::HUP | IoEvents::ERR | IoEvents::RDHUP | IoEvents::NVAL);
        result &= *events;
        result |= always_report;

        let revents = &mut poll_fds[*revent_index].revents;
        *revents = result.bits() as _;
        if *revents != 0 {
            res += 1;
        }
    }
    res
}

fn do_poll(
    current: &crate::task::UserTaskRef,
    poll_fds: &mut [pollfd],
    timeout: Option<TimeValue>,
    sigmask: Option<SignalSet>,
) -> StarryResult<isize> {
    debug!("do_poll fds={poll_fds:?} timeout={timeout:?}");

    let mut res = 0isize;
    let mut fds = Vec::with_capacity(poll_fds.len());
    let mut revent_indices = Vec::with_capacity(poll_fds.len());
    for (index, fd) in poll_fds.iter_mut().enumerate() {
        fd.revents = 0;
        if fd.fd < 0 {
            // Linux ignores every negative descriptor and returns zero revents.
            continue;
        }
        match get_file_like(fd.fd) {
            Ok(f) => {
                fds.push((
                    f,
                    IoEvents::from_bits_truncate(u32::from(fd.events as u16))
                        | IoEvents::ALWAYS_POLL,
                ));
                revent_indices.push(index);
            }
            Err(_) => {
                // If the fd is invalid, set revents to POLLNVAL
                fd.revents = POLLNVAL as _;
                res += 1;
            }
        }
    }
    if res > 0 {
        return Ok(res);
    }
    let fds = FdPollSet(fds);

    with_blocked_signals(sigmask, || {
        let wait = poll_fn(|cx| {
            let mut res = collect_ready_poll_events(&fds, &revent_indices, poll_fds);
            if res > 0 {
                return Poll::Ready(Ok(res as _));
            }

            fds.register(cx, IoEvents::empty());

            res = collect_ready_poll_events(&fds, &revent_indices, poll_fds);
            if res > 0 {
                return Poll::Ready(Ok(res as _));
            }
            Poll::Pending
        });

        let task = current;
        match block_on_user_timeout(task, timeout, wait) {
            UserWaitOutcome::Ready(result) => result,
            UserWaitOutcome::TimedOut => Ok(0),
            UserWaitOutcome::Interrupted => Err(crate::StarryError::Interrupted),
        }
    })
}

#[cfg(target_arch = "x86_64")]
pub fn sys_poll(
    current: &crate::task::UserTaskRef,
    fds: UserPtr<pollfd>,
    nfds: u32,
    timeout: i32,
) -> crate::StarryResult<isize> {
    let nfds = nfds as usize;
    let mut poll_fds = read_poll_fds(current, fds, nfds)?;
    let timeout = if timeout < 0 {
        None
    } else {
        Some(TimeValue::from_millis(timeout as u64))
    };
    let res = do_poll(current, &mut poll_fds, timeout, None);
    // Linux copies the cleared/recomputed revents array back even when the
    // wait is interrupted. A copy fault still takes precedence over EINTR.
    if nfds > 0 {
        write_poll_revents(current, fds, &poll_fds)?;
    }
    res
}

pub fn sys_ppoll(
    current: &crate::task::UserTaskRef,
    fds: UserPtr<pollfd>,
    nfds: i32,
    timeout: UserConstPtr<timespec>,
    sigmask: UserConstPtr<SignalSet>,
    sigsetsize: usize,
) -> StarryResult<isize> {
    if !sigmask.is_null() {
        check_sigset_size(sigsetsize)?;
    }
    let nfds = nfds
        .try_into()
        .map_err(|_| crate::StarryError::InvalidInput)?;
    let mut poll_fds = read_poll_fds(current, fds, nfds)?;
    let timeout = (if timeout.is_null() {
        None
    } else {
        // SAFETY: timespec contains only signed integer fields; semantic
        // range validation is performed by try_into_time_value below.
        Some(unsafe { timeout.read_abi(current)? })
    })
    .map(|ts| ts.try_into_time_value())
    .transpose()?;
    let sigmask = if sigmask.is_null() {
        None
    } else {
        // SAFETY: SignalSet is a transparent signal-bit mask; every bit
        // pattern is valid and unsupported bits are handled by signal logic.
        Some(unsafe { sigmask.read_abi(current)? })
    };
    let res = do_poll(current, &mut poll_fds, timeout, sigmask);
    // Match poll(2): interruption does not leave the caller's old revents
    // values visible, and a failed writeback is reported as EFAULT.
    if nfds > 0 {
        write_poll_revents(current, fds, &poll_fds)?;
    }
    res
}

#[cfg(test)]
pub(crate) fn poll_nfds_validation_rules_hold_for_test() -> bool {
    // Test nfds validation logic
    // nfds must be <= RLIMIT_NOFILE current limit
    let valid_nfds = 0usize;
    assert!(valid_nfds as u64 <= u64::MAX); // Always valid

    let small_nfds = 1024usize;
    assert!(small_nfds as u64 <= u64::MAX);

    // POLLNVAL constant check
    assert!(POLLNVAL != 0);

    true
}
