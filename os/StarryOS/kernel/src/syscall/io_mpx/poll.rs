use alloc::vec::Vec;
use core::mem::MaybeUninit;

use ax_errno::{AxError, AxResult};
use ax_hal::time::TimeValue;
use ax_task::future::{self, block_on, poll_io};
use axpoll::IoEvents;
use linux_raw_sys::general::{POLLNVAL, pollfd, timespec};
use starry_signal::SignalSet;
use starry_vm::{vm_read_slice, vm_write_slice};

use super::FdPollSet;
use crate::{
    file::get_file_like,
    mm::{UserConstPtr, UserPtr, nullable},
    syscall::signal::check_sigset_size,
    task::with_blocked_signals,
    time::TimeValueLike,
};

/// Copy a user-space pollfd array into a kernel-owned Vec.
///
/// This is the Linux-style approach: copy the struct array to kernel memory
/// first, operate on the copy, then write results back via vm_write_slice.
/// Direct references to user memory are avoided to prevent unhandled COW
/// write-faults when the page is re-protected by a concurrent fork.
fn read_poll_fds_from_user(ptr: UserPtr<pollfd>, nfds: usize) -> AxResult<Vec<pollfd>> {
    if nfds == 0 {
        return Ok(Vec::new());
    }
    let mut buf = alloc::vec![MaybeUninit::<pollfd>::uninit(); nfds];
    vm_read_slice(ptr.address().as_usize() as *const pollfd, &mut buf)
        .map_err(|_| AxError::BadAddress)?;
    // SAFETY: pollfd consists entirely of integer fields (i32, i16, i16);
    // all bit patterns are valid, so assume_init is sound.
    let mut md = core::mem::ManuallyDrop::new(buf);
    Ok(unsafe { Vec::from_raw_parts(md.as_mut_ptr().cast::<pollfd>(), md.len(), md.capacity()) })
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
    debug!("sys_poll <= nfds: {nfds}");
    let nfds = nfds as usize;
    // Copy user pollfd array into kernel buffer (Linux-compatible approach).
    let mut kfds = read_poll_fds_from_user(fds, nfds)?;
    let timeout = if timeout < 0 {
        None
    } else {
        Some(TimeValue::from_millis(timeout as u64))
    };
    let result = do_poll(&mut kfds, timeout, None)?;
    // Write the updated pollfd array (including revents) back to user space.
    if nfds > 0 {
        vm_write_slice(fds.address().as_usize() as *mut pollfd, &kfds)
            .map_err(|_| AxError::BadAddress)?;
    }
    Ok(result)
}

pub fn sys_ppoll(
    fds: UserPtr<pollfd>,
    nfds: i32,
    timeout: UserConstPtr<timespec>,
    sigmask: UserConstPtr<SignalSet>,
    sigsetsize: usize,
) -> AxResult<isize> {
    check_sigset_size(sigsetsize)?;
    let nfds = nfds.try_into().map_err(|_| AxError::InvalidInput)?;
    let mut kfds = read_poll_fds_from_user(fds, nfds)?;
    let timeout = nullable!(timeout.get_as_ref())?
        .map(|ts| ts.try_into_time_value())
        .transpose()?;
    let result = do_poll(
        &mut kfds,
        timeout,
        nullable!(sigmask.get_as_ref())?.copied(),
    )?;
    if nfds > 0 {
        vm_write_slice(fds.address().as_usize() as *mut pollfd, &kfds)
            .map_err(|_| AxError::BadAddress)?;
    }
    Ok(result)
}
