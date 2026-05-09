use alloc::vec::Vec;
use core::{future::poll_fn, task::Poll};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_hal::{time::TimeValue, uspace::UserContext};
use ax_task::future::{self, block_on, poll_io};
use axpoll::IoEvents;
use linux_raw_sys::general::{POLLNVAL, pollfd, timespec};
use starry_signal::SignalSet;

use super::FdPollSet;
use crate::{
    file::get_file_like,
    mm::{UserConstPtr, UserPtr, nullable},
    syscall::signal::check_sigset_size,
    task::{AsThread, check_signals, with_blocked_signals},
    time::TimeValueLike,
};

fn wait_for_signal(uctx: &mut UserContext) -> AxResult<isize> {
    let curr = ax_task::current();
    let thr = curr.as_thread();

    // Some libc pause(2) implementations use ppoll(NULL, 0, NULL, NULL).
    // It must return EINTR after a caught signal, even with SA_RESTART.
    uctx.set_retval(-LinuxError::EINTR.code() as usize);
    block_on(poll_fn(|cx| {
        if check_signals(thr, uctx, None, None) {
            return Poll::Ready(());
        }
        let _ = curr.poll_interrupt(cx);
        Poll::Pending
    }));

    Err(AxError::Interrupted)
}

fn do_poll(
    poll_fds: &mut [pollfd],
    timeout: Option<TimeValue>,
    sigmask: Option<SignalSet>,
) -> AxResult<isize> {
    debug!("do_poll fds={poll_fds:?} timeout={timeout:?}");

    let mut res = 0isize;
    let mut fds = Vec::with_capacity(poll_fds.len());
    let mut revents = Vec::with_capacity(poll_fds.len());
    for fd in poll_fds.iter_mut() {
        if fd.fd == -1 {
            // Skip -1
            continue;
        }
        match get_file_like(fd.fd) {
            Ok(f) => {
                fds.push((
                    f,
                    IoEvents::from_bits(fd.events as _).ok_or(AxError::InvalidInput)?
                        | IoEvents::ALWAYS_POLL,
                ));
                revents.push(&mut fd.revents);
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
        match block_on(future::timeout(
            timeout,
            poll_io(&fds, IoEvents::empty(), false, || {
                let mut res = 0usize;
                for ((fd, events), revents) in fds.0.iter().zip(revents.iter_mut()) {
                    let mut result = fd.poll();
                    if result.contains(IoEvents::IN) {
                        result |= IoEvents::RDNORM;
                    }
                    if result.contains(IoEvents::OUT) {
                        result |= IoEvents::WRNORM;
                    }
                    // POSIX: POLLHUP and POLLERR are always reported in revents,
                    // even if not requested in events. They must NOT be masked out.
                    let always_report =
                        result & (IoEvents::HUP | IoEvents::ERR | IoEvents::RDHUP | IoEvents::NVAL);
                    result &= *events;
                    result |= always_report;

                    **revents = result.bits() as _;
                    if **revents != 0 {
                        res += 1;
                    }
                }
                if res > 0 {
                    Ok(res as _)
                } else {
                    Err(AxError::WouldBlock)
                }
            }),
        )) {
            Ok(r) => r,
            Err(_) => Ok(0),
        }
    })
}

#[cfg(target_arch = "x86_64")]
pub fn sys_poll(fds: UserPtr<pollfd>, nfds: u32, timeout: i32) -> AxResult<isize> {
    let fds = fds.get_as_mut_slice(nfds as usize)?;
    let timeout = if timeout < 0 {
        None
    } else {
        Some(TimeValue::from_millis(timeout as u64))
    };
    do_poll(fds, timeout, None)
}

pub fn sys_ppoll(
    uctx: &mut UserContext,
    fds: UserPtr<pollfd>,
    nfds: i32,
    timeout: UserConstPtr<timespec>,
    sigmask: UserConstPtr<SignalSet>,
    sigsetsize: usize,
) -> AxResult<isize> {
    check_sigset_size(sigsetsize)?;
    let fds = fds.get_as_mut_slice(nfds.try_into().map_err(|_| AxError::InvalidInput)?)?;
    let timeout = nullable!(timeout.get_as_ref())?
        .map(|ts| ts.try_into_time_value())
        .transpose()?;
    let sigmask = nullable!(sigmask.get_as_ref())?.copied();
    if fds.is_empty() && timeout.is_none() && sigmask.is_none() {
        return wait_for_signal(uctx);
    }
    // TODO: handle signal for general poll sets.
    do_poll(fds, timeout, sigmask)
}
