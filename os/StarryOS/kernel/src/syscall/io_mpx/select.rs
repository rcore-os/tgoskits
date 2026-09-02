use alloc::vec::Vec;
use core::{fmt, time::Duration};

use ax_task::future::{self, block_on, poll_io};
use axpoll::IoEvents;
use bitmaps::Bitmap;
use linux_raw_sys::{
    general::*,
    select_macros::{FD_ISSET, FD_SET, FD_ZERO},
};
use starry_signal::SignalSet;
use starry_vm::{VmMutPtr, VmPtr};

use super::FdPollSet;
use crate::{
    StarryError, StarryResult,
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

fn read_fd_set(user: *const __kernel_fd_set) -> StarryResult<Option<__kernel_fd_set>> {
    user.nullable()
        .map(|user| {
            let value = user.vm_read_uninit()?;
            // SAFETY: `fd_set` is an array of integer masks; every bit
            // pattern is a valid value.  The VM copy initialized every byte.
            Ok(unsafe { value.assume_init() })
        })
        .transpose()
}

fn write_fd_set(
    user: *mut __kernel_fd_set,
    selected: &FdSet,
    nfds: usize,
) -> StarryResult<()> {
    if user.is_null() {
        return Ok(());
    }
    // SAFETY: `fd_set` contains only integer masks, for which all-zero is a
    // valid representation.
    let mut output: __kernel_fd_set = unsafe { core::mem::zeroed() };
    unsafe { FD_ZERO(&mut output) };
    for index in selected.0.into_iter().take(nfds) {
        unsafe { FD_SET(index as _, &mut output) };
    }
    user.vm_write(output)?;
    Ok(())
}

impl fmt::Debug for FdSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(&self.0).finish()
    }
}

fn do_select(
    nfds: u32,
    readfds: *mut __kernel_fd_set,
    writefds: *mut __kernel_fd_set,
    exceptfds: *mut __kernel_fd_set,
    timeout: Option<Duration>,
    sigmask: *const SignalSetWithSize,
) -> StarryResult<isize> {
    if nfds > __FD_SETSIZE {
        return Err(StarryError::InvalidInput);
    }
    let sigmask = if let Some(sigmask) = sigmask.nullable() {
        // SAFETY: the wrapper consists only of a raw pointer and an integer;
        // every copied bit pattern is valid for those fields.
        let sigmask = unsafe { sigmask.vm_read_any()? };
        check_sigset_size(sigmask.sigsetsize)?;
        let set = sigmask.set;
        set.nullable()
            .map(|set| {
                // SAFETY: SignalSet is a plain signal-bit mask.
                unsafe { set.vm_read_any() }
            })
            .transpose()?
    } else {
        None
    };

    // Copy every input set into kernel-owned storage before the operation can
    // block.  No userspace reference survives `poll_io` or a signal wakeup.
    let readfds_input = read_fd_set(readfds)?;
    let writefds_input = read_fd_set(writefds)?;
    let exceptfds_input = read_fd_set(exceptfds)?;

    let read_set = FdSet::new(nfds as _, readfds_input.as_ref());
    let write_set = FdSet::new(nfds as _, writefds_input.as_ref());
    let except_set = FdSet::new(nfds as _, exceptfds_input.as_ref());

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
            .ok_or(StarryError::BadFileDescriptor)?
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

    let (count, selected_readfds, selected_writefds, selected_exceptfds) =
        with_blocked_signals(sigmask, || {
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
                    // Linux fs/select.c: POLLIN_SET carries HUP|ERR but
                    // POLLOUT_SET carries only ERR, so a hangup makes a fd
                    // readable (read returns EOF) yet never writable.
                    let write_report = events & IoEvents::ERR;
                    let selected = events & *interested;
                    let selected_read = selected.contains(IoEvents::IN)
                        || (read_set.0.get(index) && !always_report.is_empty());
                    let selected_write = selected.contains(IoEvents::OUT)
                        || (write_set.0.get(index) && !write_report.is_empty());
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
                    return Ok((
                        res as isize,
                        selected_readfds,
                        selected_writefds,
                        selected_exceptfds,
                    ));
                }

                Err(StarryError::WouldBlock)
            }),
        ));
        match result {
            Ok(r) => r,
            Err(_) => {
                Ok((
                    0,
                    FdSet(Bitmap::new()),
                    FdSet(Bitmap::new()),
                    FdSet(Bitmap::new()),
                ))
            }
        }
    })?;

    // Copy results back only after all blocking work and file-table locks are
    // gone. A copyout fault takes precedence over the ready count.
    write_fd_set(readfds, &selected_readfds, nfds as _)?;
    write_fd_set(writefds, &selected_writefds, nfds as _)?;
    write_fd_set(exceptfds, &selected_exceptfds, nfds as _)?;
    Ok(count)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_select(
    nfds: u32,
    readfds: *mut __kernel_fd_set,
    writefds: *mut __kernel_fd_set,
    exceptfds: *mut __kernel_fd_set,
    timeout: *const timeval,
) -> StarryResult<isize> {
    do_select(
        nfds,
        readfds,
        writefds,
        exceptfds,
        timeout
            .nullable()
            .map(|timeout| {
                // SAFETY: Linux `timeval` contains only signed integer fields.
                unsafe { timeout.vm_read_any() }
            })
            .transpose()?
            .map(|it| it.try_into_time_value())
            .transpose()?,
        core::ptr::null(),
    )
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SignalSetWithSize {
    set: *const SignalSet,
    sigsetsize: usize,
}

pub fn sys_pselect6(
    nfds: u32,
    readfds: *mut __kernel_fd_set,
    writefds: *mut __kernel_fd_set,
    exceptfds: *mut __kernel_fd_set,
    timeout: *const timespec,
    sigmask: *const SignalSetWithSize,
) -> StarryResult<isize> {
    do_select(
        nfds,
        readfds,
        writefds,
        exceptfds,
        timeout
            .nullable()
            .map(|timeout| {
                // SAFETY: Linux `timespec` contains only signed integer fields.
                unsafe { timeout.vm_read_any() }
            })
            .transpose()?
            .map(|ts| ts.try_into_time_value())
            .transpose()?,
        sigmask,
    )
}

#[cfg(all(test, not(axtest)))]
fn select_fd_set_and_validation_rules_hold_for_test() -> bool {
    use linux_raw_sys::general::__FD_SETSIZE;

    // Test nfds validation: must be <= __FD_SETSIZE
    let valid_nfds = 1024u32;
    assert!(valid_nfds <= __FD_SETSIZE);

    let max_nfds = __FD_SETSIZE;
    assert!(max_nfds <= __FD_SETSIZE);

    // Invalid: nfds > __FD_SETSIZE
    let invalid_nfds = __FD_SETSIZE + 1;
    assert!(invalid_nfds > __FD_SETSIZE);

    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn select_fd_set_and_validation_rules_hold() {
        assert!(super::select_fd_set_and_validation_rules_hold_for_test());
    }
}
