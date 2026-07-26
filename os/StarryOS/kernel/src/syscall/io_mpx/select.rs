use alloc::vec::Vec;
use core::{fmt, time::Duration};

use ax_errno::{AxError, AxResult};
use ax_task::future::{self, block_on, poll_io};
use axpoll::IoEvents;
use bitmaps::Bitmap;
use linux_raw_sys::{
    general::*,
    select_macros::{FD_ISSET, FD_SET, FD_ZERO},
};
use starry_signal::SignalSet;

use super::FdPollSet;
use crate::{
    mm::{UserConstPtr, UserPtr, nullable},
    syscall::signal::check_sigset_size,
    task::with_blocked_signals,
    time::TimeValueLike,
};

struct FdSet(Bitmap<{ __FD_SETSIZE as usize }>);

impl FdSet {
    fn new(nfds: usize, fds: Option<&__kernel_fd_set>) -> Self {
        let mut bitmap = Bitmap::new();
        if let Some(fds) = fds {
            for i in 0..nfds {
                if unsafe { FD_ISSET(i as _, fds) } {
                    bitmap.set(i, true);
                }
            }
        }
        Self(bitmap)
    }
}

fn write_fd_set(user: Option<&mut __kernel_fd_set>, selected: &FdSet, nfds: usize) {
    if let Some(user) = user {
        unsafe { FD_ZERO(user) };
        for index in selected.0.into_iter().take(nfds) {
            unsafe { FD_SET(index as _, user) };
        }
    }
}

impl fmt::Debug for FdSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(&self.0).finish()
    }
}

fn do_select(
    nfds: u32,
    readfds: UserPtr<__kernel_fd_set>,
    writefds: UserPtr<__kernel_fd_set>,
    exceptfds: UserPtr<__kernel_fd_set>,
    timeout: Option<Duration>,
    sigmask: UserConstPtr<SignalSetWithSize>,
) -> AxResult<isize> {
    if nfds > __FD_SETSIZE {
        return Err(AxError::InvalidInput);
    }
    let sigmask = if let Some(sigmask) = nullable!(sigmask.get_as_ref())? {
        check_sigset_size(sigmask.sigsetsize)?;
        let set = sigmask.set;
        nullable!(set.get_as_ref())?
    } else {
        None
    };

    let mut readfds = nullable!(readfds.get_as_mut())?;
    let mut writefds = nullable!(writefds.get_as_mut())?;
    let mut exceptfds = nullable!(exceptfds.get_as_mut())?;

    let read_set = FdSet::new(nfds as _, readfds.as_deref());
    let write_set = FdSet::new(nfds as _, writefds.as_deref());
    let except_set = FdSet::new(nfds as _, exceptfds.as_deref());

    debug!(
        "sys_select <= nfds: {nfds} sets: [read: {read_set:?}, write: {write_set:?}, except: \
         {except_set:?}] timeout: {timeout:?}"
    );

    let current_fd_table = crate::file::current_fd_table();
    let fd_table = current_fd_table.read();
    let fd_bitmap = read_set.0 | write_set.0 | except_set.0;
    let fd_count = fd_bitmap.len();
    let mut fds = Vec::with_capacity(fd_count);
    let mut fd_indices = Vec::with_capacity(fd_count);
    for fd in fd_bitmap.into_iter() {
        let f = fd_table
            .get(fd)
            .ok_or(AxError::BadFileDescriptor)?
            .inner
            .clone();
        let mut events = IoEvents::empty();
        events.set(IoEvents::IN, read_set.0.get(fd));
        events.set(IoEvents::OUT, write_set.0.get(fd));
        events.set(IoEvents::ERR, except_set.0.get(fd));
        if !events.is_empty() {
            fds.push((f, events));
            fd_indices.push(fd);
        }
    }

    drop(fd_table);
    let fds = FdPollSet(fds);

    with_blocked_signals(sigmask.copied(), || {
        let result = block_on(future::timeout(
            timeout,
            poll_io(&fds, IoEvents::empty(), false, || {
                let mut res = 0usize;
                let mut selected_readfds = FdSet(Bitmap::new());
                let mut selected_writefds = FdSet(Bitmap::new());
                let mut selected_exceptfds = FdSet(Bitmap::new());
                for ((fd, interested), index) in fds.0.iter().zip(fd_indices.iter().copied()) {
                    let events = fd.poll();
                    let always_report = events & IoEvents::ALWAYS_POLL;
                    let selected = events & *interested;
                    let selected_read = selected.contains(IoEvents::IN)
                        || (read_set.0.get(index) && !always_report.is_empty());
                    let selected_write = selected.contains(IoEvents::OUT)
                        || (write_set.0.get(index) && !always_report.is_empty());
                    let selected_except =
                        selected.contains(IoEvents::ERR) && except_set.0.get(index);

                    if selected_read {
                        res += 1;
                        selected_readfds.0.set(index, true);
                    }
                    if selected_write {
                        res += 1;
                        selected_writefds.0.set(index, true);
                    }
                    if selected_except {
                        res += 1;
                        selected_exceptfds.0.set(index, true);
                    }
                }
                if res > 0 {
                    write_fd_set(readfds.as_deref_mut(), &selected_readfds, nfds as _);
                    write_fd_set(writefds.as_deref_mut(), &selected_writefds, nfds as _);
                    write_fd_set(exceptfds.as_deref_mut(), &selected_exceptfds, nfds as _);
                    return Ok(res as _);
                }

                Err(AxError::WouldBlock)
            }),
        ));
        match result {
            Ok(r) => r,
            Err(_) => {
                let empty = FdSet(Bitmap::new());
                write_fd_set(readfds, &empty, nfds as _);
                write_fd_set(writefds, &empty, nfds as _);
                write_fd_set(exceptfds, &empty, nfds as _);
                Ok(0)
            }
        }
    })
}

#[cfg(target_arch = "x86_64")]
pub fn sys_select(
    nfds: u32,
    readfds: UserPtr<__kernel_fd_set>,
    writefds: UserPtr<__kernel_fd_set>,
    exceptfds: UserPtr<__kernel_fd_set>,
    timeout: UserConstPtr<timeval>,
) -> AxResult<isize> {
    do_select(
        nfds,
        readfds,
        writefds,
        exceptfds,
        nullable!(timeout.get_as_ref())?
            .map(|it| it.try_into_time_value())
            .transpose()?,
        0.into(),
    )
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SignalSetWithSize {
    set: UserConstPtr<SignalSet>,
    sigsetsize: usize,
}

pub fn sys_pselect6(
    nfds: u32,
    readfds: UserPtr<__kernel_fd_set>,
    writefds: UserPtr<__kernel_fd_set>,
    exceptfds: UserPtr<__kernel_fd_set>,
    timeout: UserConstPtr<timespec>,
    sigmask: UserConstPtr<SignalSetWithSize>,
) -> AxResult<isize> {
    do_select(
        nfds,
        readfds,
        writefds,
        exceptfds,
        nullable!(timeout.get_as_ref())?
            .map(|ts| ts.try_into_time_value())
            .transpose()?,
        sigmask,
    )
}
