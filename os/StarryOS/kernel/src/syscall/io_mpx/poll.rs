use alloc::vec::Vec;
use core::mem::{MaybeUninit, offset_of};

use ax_errno::{AxError, AxResult};
use ax_hal::time::TimeValue;
use ax_task::{
    current,
    future::{self, block_on, poll_io},
};
use axpoll::IoEvents;
use linux_raw_sys::general::{POLLNVAL, RLIMIT_NOFILE, pollfd, timespec};
use starry_signal::SignalSet;
use starry_vm::{vm_read_slice, vm_write_slice};

use super::FdPollSet;
use crate::{
    file::get_file_like,
    mm::{UserConstPtr, UserPtr, nullable},
    syscall::signal::check_sigset_size,
    task::{AsThread, with_blocked_signals},
    time::TimeValueLike,
};

fn check_nfds_limit(nfds: usize) -> AxResult<()> {
    let nofile = current().as_thread().proc_data.rlim.read()[RLIMIT_NOFILE].current;
    if nfds as u64 > nofile {
        Err(AxError::InvalidInput)
    } else {
        Ok(())
    }
}

fn read_poll_fds(fds: UserPtr<pollfd>, nfds: usize) -> AxResult<Vec<pollfd>> {
    check_nfds_limit(nfds)?;
    if nfds == 0 {
        return Ok(Vec::new());
    }

    let mut buf = Vec::with_capacity(nfds);
    buf.resize_with(nfds, MaybeUninit::uninit);
    vm_read_slice(fds.as_ptr(), &mut buf)?;
    Ok(buf
        .into_iter()
        .map(|fd| unsafe { fd.assume_init() })
        .collect())
}

fn write_poll_revents(fds: UserPtr<pollfd>, poll_fds: &[pollfd]) -> AxResult<()> {
    let revents_offset = offset_of!(pollfd, revents);

    for (index, poll_fd) in poll_fds.iter().enumerate() {
        let revents_ptr = (fds.as_ptr().wrapping_add(index) as *mut u8)
            .wrapping_add(revents_offset)
            .cast::<_>();
        vm_write_slice(revents_ptr, core::slice::from_ref(&poll_fd.revents))?;
    }

    Ok(())
}

fn do_poll(
    poll_fds: &mut [pollfd],
    timeout: Option<TimeValue>,
    sigmask: Option<SignalSet>,
) -> AxResult<isize> {
    debug!("do_poll fds={poll_fds:?} timeout={timeout:?}");

    let mut res = 0isize;
    let mut fds = Vec::with_capacity(poll_fds.len());
    let mut revent_indices = Vec::with_capacity(poll_fds.len());
    for (index, fd) in poll_fds.iter_mut().enumerate() {
        fd.revents = 0;
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
        match block_on(future::timeout(
            timeout,
            poll_io(&fds, IoEvents::empty(), false, || {
                let mut res = 0usize;
                for ((fd, events), revent_index) in fds.0.iter().zip(revent_indices.iter()) {
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

                    let revents = &mut poll_fds[*revent_index].revents;
                    *revents = result.bits() as _;
                    if *revents != 0 {
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
    let nfds = nfds as usize;
    let mut poll_fds = read_poll_fds(fds, nfds)?;
    let timeout = if timeout < 0 {
        None
    } else {
        Some(TimeValue::from_millis(timeout as u64))
    };
    let res = do_poll(&mut poll_fds, timeout, None)?;
    if nfds > 0 {
        write_poll_revents(fds, &poll_fds)?;
    }
    Ok(res)
}

pub fn sys_ppoll(
    fds: UserPtr<pollfd>,
    nfds: i32,
    timeout: UserConstPtr<timespec>,
    sigmask: UserConstPtr<SignalSet>,
    sigsetsize: usize,
) -> AxResult<isize> {
    if !sigmask.is_null() {
        check_sigset_size(sigsetsize)?;
    }
    let nfds = nfds.try_into().map_err(|_| AxError::InvalidInput)?;
    let mut poll_fds = read_poll_fds(fds, nfds)?;
    let timeout = nullable!(timeout.get_as_ref())?
        .map(|ts| ts.try_into_time_value())
        .transpose()?;
    // TODO: handle signal
    let res = do_poll(
        &mut poll_fds,
        timeout,
        nullable!(sigmask.get_as_ref())?.copied(),
    )?;
    if nfds > 0 {
        write_poll_revents(fds, &poll_fds)?;
    }
    Ok(res)
}
