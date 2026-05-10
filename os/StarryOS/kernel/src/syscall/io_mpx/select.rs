use alloc::vec::Vec;
use core::{fmt, mem::MaybeUninit, time::Duration};

use ax_errno::{AxError, AxResult};
use ax_task::future::{self, block_on, poll_io};
use axpoll::IoEvents;
use bitmaps::Bitmap;
use linux_raw_sys::{
    general::*,
    select_macros::{FD_ISSET, FD_SET},
};
use starry_signal::SignalSet;
use starry_vm::{vm_read_slice, vm_write_slice};

use super::FdPollSet;
use crate::{
    file::FD_TABLE,
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

impl fmt::Debug for FdSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(&self.0).finish()
    }
}

/// Copy a single `__kernel_fd_set` from user space into a kernel-local value.
///
/// Returns `None` if the pointer is null (fd_set argument was omitted).
fn read_fd_set_from_user(ptr: UserPtr<__kernel_fd_set>) -> AxResult<Option<__kernel_fd_set>> {
    if ptr.is_null() {
        return Ok(None);
    }
    let mut buf = [MaybeUninit::<__kernel_fd_set>::uninit()];
    vm_read_slice(ptr.address().as_usize() as *const __kernel_fd_set, &mut buf)
        .map_err(|_| AxError::BadAddress)?;
    // SAFETY: __kernel_fd_set is [c_ulong; 16]; all bit patterns are valid.
    Ok(Some(unsafe { buf[0].assume_init() }))
}

/// Write a kernel-local `__kernel_fd_set` back to user space.
fn write_fd_set_to_user(ptr: UserPtr<__kernel_fd_set>, set: &__kernel_fd_set) -> AxResult<()> {
    if ptr.is_null() {
        return Ok(());
    }
    vm_write_slice(
        ptr.address().as_usize() as *mut __kernel_fd_set,
        core::slice::from_ref(set),
    )
    .map_err(|_| AxError::BadAddress)
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

    // Read the input fd_sets from user space into kernel-local copies.
    // Reads are safe even on COW pages (read faults are never triggered by
    // COW; only writes are).
    let r_readfds = read_fd_set_from_user(readfds)?;
    let r_writefds = read_fd_set_from_user(writefds)?;
    let r_exceptfds = read_fd_set_from_user(exceptfds)?;

    let read_set = FdSet::new(nfds as _, r_readfds.as_ref());
    let write_set = FdSet::new(nfds as _, r_writefds.as_ref());
    let except_set = FdSet::new(nfds as _, r_exceptfds.as_ref());

    debug!(
        "sys_select <= nfds: {nfds} sets: [read: {read_set:?}, write: {write_set:?}, except: \
         {except_set:?}] timeout: {timeout:?}"
    );

    let fd_table = FD_TABLE.read();
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

    // Kernel-local output fd_sets, zero-initialised (equivalent to FD_ZERO).
    // We build results here and write them back to user space after polling.
    let mut k_readfds: Option<__kernel_fd_set> =
        r_readfds.map(|_| unsafe { core::mem::zeroed::<__kernel_fd_set>() });
    let mut k_writefds: Option<__kernel_fd_set> =
        r_writefds.map(|_| unsafe { core::mem::zeroed::<__kernel_fd_set>() });
    let mut k_exceptfds: Option<__kernel_fd_set> =
        r_exceptfds.map(|_| unsafe { core::mem::zeroed::<__kernel_fd_set>() });

    let result = with_blocked_signals(sigmask.copied(), || {
        match block_on(future::timeout(
            timeout,
            poll_io(&fds, IoEvents::empty(), false, || {
                let mut res = 0usize;
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

                    if selected_read && let Some(ref mut set) = k_readfds {
                        res += 1;
                        unsafe { FD_SET(index as _, set) };
                    }
                    if selected_write && let Some(ref mut set) = k_writefds {
                        res += 1;
                        unsafe { FD_SET(index as _, set) };
                    }
                    if selected_except && let Some(ref mut set) = k_exceptfds {
                        res += 1;
                        unsafe { FD_SET(index as _, set) };
                    }
                }
                if res > 0 {
                    return Ok(res as _);
                }

                Err(AxError::WouldBlock)
            }),
        )) {
            Ok(r) => r,
            Err(_) => Ok(0),
        }
    })?;

    // Write the kernel-local fd_sets back to user space via vm_write_slice.
    // This goes through access_user_memory + user_copy, which correctly handles
    // COW write-faults that may have been introduced by a concurrent fork.
    if let Some(ref set) = k_readfds {
        write_fd_set_to_user(readfds, set)?;
    }
    if let Some(ref set) = k_writefds {
        write_fd_set_to_user(writefds, set)?;
    }
    if let Some(ref set) = k_exceptfds {
        write_fd_set_to_user(exceptfds, set)?;
    }

    Ok(result)
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
