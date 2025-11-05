use core::time::Duration;

use axerrno::{AxError, AxResult};
use axpoll::IoEvents;
use axtask::future::Poller;
use bitflags::bitflags;
use linux_raw_sys::general::{
    EPOLL_CLOEXEC, EPOLL_CTL_ADD, EPOLL_CTL_DEL, EPOLL_CTL_MOD, epoll_event, timespec,
};
use starry_signal::SignalSet;

use crate::{
    file::{
        FileLike,
        epoll::{Epoll, EpollEvent, EpollFlags},
    },
    mm::{UserConstPtr, UserPtr, nullable},
    signal::with_replacen_blocked,
    syscall::signal::check_sigset_size,
    time::TimeValueLike,
};

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
        let event = event.get_as_ref()?;
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
    check_sigset_size(sigsetsize)?;
    debug!("sys_epoll_wait <= epfd: {epfd}, maxevents: {maxevents}, timeout: {timeout:?}");

    let epoll = Epoll::from_fd(epfd)?;

    if maxevents <= 0 {
        return Err(AxError::InvalidInput);
    }
    let events = events.get_as_mut_slice(maxevents as usize)?;

    with_replacen_blocked(
        nullable!(sigmask.get_as_ref())?.copied(),
        || match Poller::new(epoll.as_ref(), IoEvents::IN)
            .timeout(timeout)
            .poll(|| epoll.poll_events(events))
        {
            Ok(n) => Ok(n as isize),
            Err(AxError::TimedOut) => Ok(0),
            Err(e) => Err(e),
        },
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
