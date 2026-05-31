use core::{
    mem::{MaybeUninit, size_of},
    time::Duration,
};

use ax_errno::{AxError, AxResult};
use ax_task::future::{self, block_on, poll_io};
use axpoll::IoEvents;
use bitflags::bitflags;
use linux_raw_sys::general::{
    EPOLL_CLOEXEC, EPOLL_CTL_ADD, EPOLL_CTL_DEL, EPOLL_CTL_MOD, epoll_event, timespec,
};
use starry_signal::SignalSet;
use starry_vm::{vm_read_slice, vm_write_slice};

use crate::{
    file::{
        FileLike,
        epoll::{Epoll, EpollEvent, EpollFlags},
    },
    mm::{UserConstPtr, UserPtr, check_access, nullable},
    syscall::signal::check_sigset_size,
    task::with_blocked_signals,
    time::TimeValueLike,
};

const EP_MAX_EVENTS: usize = i32::MAX as usize / size_of::<epoll_event>();

fn check_epoll_events_access(events: UserPtr<epoll_event>, maxevents: usize) -> AxResult<()> {
    let len = maxevents
        .checked_mul(size_of::<epoll_event>())
        .ok_or(AxError::BadAddress)?;
    let start = events.as_ptr() as usize;
    start.checked_add(len).ok_or(AxError::BadAddress)?;
    check_access(start, len)?;
    Ok(())
}

/// Reads a single `epoll_event` from user memory without requiring the user
/// pointer to satisfy `epoll_event`'s natural alignment.
///
/// Linux copies the `struct epoll_event` from user space with `copy_from_user`,
/// which performs a byte-wise copy and imposes no alignment requirement. On
/// architectures where `struct epoll_event` is NOT `__attribute__((packed))`
/// (everything except x86/x86_64), the C struct has 8-byte alignment, but
/// runtimes are free to back it with a less-strictly-aligned buffer. The Go
/// runtime, for instance, lays out its `epollevent` with `[8]byte data` and
/// thus only 4-byte alignment, so `&ev` passed to `epoll_ctl` can land at an
/// address that is `4 (mod 8)`. Reading through a typed `*const epoll_event`
/// (which the generic VM helpers reject when the pointer is unaligned) would
/// then fail with `EFAULT`. Copy at byte granularity to mirror Linux.
fn read_epoll_event(event: UserConstPtr<epoll_event>) -> AxResult<epoll_event> {
    let mut buf = MaybeUninit::<epoll_event>::uninit();
    let dst = unsafe {
        core::slice::from_raw_parts_mut(
            buf.as_mut_ptr().cast::<MaybeUninit<u8>>(),
            size_of::<epoll_event>(),
        )
    };
    vm_read_slice(event.address().as_ptr(), dst)?;
    // SAFETY: all bytes were just initialized by the copy above and any bit
    // pattern is a valid `epoll_event` (plain old data).
    Ok(unsafe { buf.assume_init() })
}

/// Writes a single `epoll_event` to the user `events` array slot `index`
/// without requiring the user pointer to satisfy `epoll_event`'s natural
/// alignment.
///
/// See [`read_epoll_event`] for why epoll user buffers may be under-aligned;
/// Linux's `__put_user` of each field has no alignment requirement, so we copy
/// at byte granularity to match.
fn write_epoll_event(
    events: UserPtr<epoll_event>,
    index: usize,
    event: &epoll_event,
) -> AxResult<()> {
    let dst = events.as_ptr().wrapping_add(index) as *mut u8;
    let src = unsafe {
        core::slice::from_raw_parts(
            event as *const epoll_event as *const u8,
            size_of::<epoll_event>(),
        )
    };
    vm_write_slice(dst, src)?;
    Ok(())
}

bitflags! {
    /// Flags for the `epoll_create` syscall.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct EpollCreateFlags: u32 {
        const CLOEXEC = EPOLL_CLOEXEC;
    }
}

pub fn sys_epoll_create1(flags: u32) -> AxResult<isize> {
    let flags = EpollCreateFlags::from_bits(flags).ok_or(AxError::InvalidInput)?;
    debug!("sys_epoll_create1 <= flags: {flags:?}");
    Epoll::new()
        .add_to_fd_table(flags.contains(EpollCreateFlags::CLOEXEC))
        .map(|fd| fd as isize)
}

pub fn sys_epoll_ctl(
    epfd: i32,
    op: u32,
    fd: i32,
    event: UserConstPtr<epoll_event>,
) -> AxResult<isize> {
    let epoll = Epoll::from_fd(epfd)?;
    debug!("sys_epoll_ctl <= epfd: {epfd}, op: {op}, fd: {fd}");

    let parse_event = || -> AxResult<(EpollEvent, EpollFlags)> {
        let event = read_epoll_event(event)?;
        let events = IoEvents::from_bits_truncate(event.events);
        let flags =
            EpollFlags::from_bits(event.events & !events.bits()).ok_or(AxError::InvalidInput)?;
        Ok((
            EpollEvent {
                events,
                user_data: event.data,
            },
            flags,
        ))
    };
    match op {
        EPOLL_CTL_ADD => {
            let (event, flags) = parse_event()?;
            epoll.add(fd, event, flags)?;
        }
        EPOLL_CTL_MOD => {
            let (event, flags) = parse_event()?;
            epoll.modify(fd, event, flags)?;
        }
        EPOLL_CTL_DEL => {
            epoll.delete(fd)?;
        }
        _ => return Err(AxError::InvalidInput),
    }
    Ok(0)
}

fn do_epoll_wait(
    epfd: i32,
    events: UserPtr<epoll_event>,
    maxevents: i32,
    timeout: Option<Duration>,
    sigmask: UserConstPtr<SignalSet>,
    sigsetsize: usize,
) -> AxResult<isize> {
    if !sigmask.is_null() {
        check_sigset_size(sigsetsize)?;
    }
    debug!("sys_epoll_wait <= epfd: {epfd}, maxevents: {maxevents}, timeout: {timeout:?}");

    let epoll = Epoll::from_fd(epfd)?;

    if maxevents <= 0 {
        return Err(AxError::InvalidInput);
    }
    let maxevents = maxevents as usize;
    if maxevents > EP_MAX_EVENTS {
        return Err(AxError::InvalidInput);
    }
    if events.is_null() {
        return Err(AxError::BadAddress);
    }
    check_epoll_events_access(events, maxevents)?;

    let count = with_blocked_signals(
        nullable!(sigmask.get_as_ref())?.copied(),
        || match block_on(future::timeout(
            timeout,
            poll_io(epoll.as_ref(), IoEvents::IN, false, || {
                epoll.poll_events_with(maxevents, |index, event| {
                    write_epoll_event(events, index, &event)?;
                    Ok(())
                })
            }),
        )) {
            Ok(r) => r.map(|n| n as _),
            Err(_) => Ok(0),
        },
    )?;

    Ok(count)
}

pub fn sys_epoll_wait(
    epfd: i32,
    events: UserPtr<epoll_event>,
    maxevents: i32,
    timeout: i32,
) -> AxResult<isize> {
    let timeout = match timeout {
        -1 => None,
        t if t >= 0 => Some(Duration::from_millis(t as u64)),
        _ => return Err(AxError::InvalidInput),
    };
    do_epoll_wait(
        epfd,
        events,
        maxevents,
        timeout,
        UserConstPtr::<SignalSet>::default(),
        0,
    )
}

pub fn sys_epoll_pwait(
    epfd: i32,
    events: UserPtr<epoll_event>,
    maxevents: i32,
    timeout: i32,
    sigmask: UserConstPtr<SignalSet>,
    sigsetsize: usize,
) -> AxResult<isize> {
    let timeout = match timeout {
        -1 => None,
        t if t >= 0 => Some(Duration::from_millis(t as u64)),
        _ => return Err(AxError::InvalidInput),
    };
    do_epoll_wait(epfd, events, maxevents, timeout, sigmask, sigsetsize)
}

pub fn sys_epoll_pwait2(
    epfd: i32,
    events: UserPtr<epoll_event>,
    maxevents: i32,
    timeout: UserConstPtr<timespec>,
    sigmask: UserConstPtr<SignalSet>,
    sigsetsize: usize,
) -> AxResult<isize> {
    let timeout = nullable!(timeout.get_as_ref())?
        .map(|ts| ts.try_into_time_value())
        .transpose()?;
    do_epoll_wait(epfd, events, maxevents, timeout, sigmask, sigsetsize)
}
