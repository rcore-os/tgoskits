use core::time::Duration;

use axerrno::LinuxResult;
use axhal::time::{TimeValue, wall_time};
use axio::PollState;
use axsignal::SignalSet;
use linux_raw_sys::general::{
    POLLERR, POLLIN, POLLNVAL, POLLOUT, pollfd, sigset_t, timespec, timeval,
};

use crate::{
    file::{FD_TABLE, get_file_like},
    ptr::{UserConstPtr, UserPtr, nullable},
    time::TimeValueLike,
};

fn do_poll(fds: &mut [pollfd], timeout: Option<TimeValue>) -> LinuxResult<isize> {
    debug!("do_poll fds={:?} timeout={:?}", fds, timeout);

    let deadline = timeout.map(|t| wall_time() + t);

    loop {
        axnet::poll_interfaces();

        let mut res = 0;
        for fd in &mut *fds {
            let mut revents = 0;
            match get_file_like(fd.fd) {
                Ok(f) => match f.poll() {
                    Ok(state) => {
                        if (fd.events & POLLIN as i16) != 0 && state.readable {
                            revents |= POLLIN;
                        }
                        if (fd.events & POLLOUT as i16) != 0 && state.writable {
                            revents |= POLLOUT;
                        }
                    }
                    Err(e) => {
                        warn!("poll fd={} error: {:?}", fd.fd, e);
                        revents = POLLERR;
                    }
                },
                Err(_) => {
                    revents = POLLNVAL;
                }
            }
            fd.revents = revents as _;
            if revents != 0 {
                res += 1;
            }
        }

        if res > 0 {
            return Ok(res);
        }

        if deadline.is_some_and(|d| wall_time() >= d) {
            return Ok(0);
        }

        axtask::yield_now();
    }
}

pub fn sys_poll(fds: UserPtr<pollfd>, nfds: u32, timeout: i32) -> LinuxResult<isize> {
    let fds = fds.get_as_mut_slice(nfds as usize)?;
    let timeout = if timeout < 0 {
        None
    } else {
        Some(TimeValue::from_millis(timeout as u64))
    };
    do_poll(fds, timeout)
}

pub fn sys_ppoll(
    fds: UserPtr<pollfd>,
    nfds: u32,
    timeout: UserConstPtr<timespec>,
    _sigmask: UserConstPtr<sigset_t>,
) -> LinuxResult<isize> {
    let fds = fds.get_as_mut_slice(nfds as usize)?;
    let timeout = nullable!(timeout.get_as_ref())?.map(|ts| ts.to_time_value());
    // TODO: handle signal
    do_poll(fds, timeout)
}

fn do_select(
    nfds: u32,
    read_fds: UserPtr<u8>,
    write_fds: UserPtr<u8>,
    except_fds: UserPtr<u8>,
    timeout: Option<Duration>,
) -> LinuxResult<isize> {
    let num_words = nfds.div_ceil(32) as usize;
    let mut read_fds = nullable!(read_fds.get_as_mut_slice(num_words))?;
    let mut write_fds = nullable!(write_fds.get_as_mut_slice(num_words))?;
    let mut except_fds = nullable!(except_fds.get_as_mut_slice(num_words))?;
    if let Some(fds) = read_fds.as_mut() {
        fds.fill(0);
    }
    if let Some(fds) = write_fds.as_mut() {
        fds.fill(0);
    }
    if let Some(fds) = except_fds.as_mut() {
        fds.fill(0);
    }

    fn fill(
        nfds: u32,
        fds: &mut Option<&'static mut [u8]>,
        f: impl Fn(PollState) -> bool,
    ) -> LinuxResult<usize> {
        let Some(fds) = fds else { return Ok(0) };
        let fd_table = FD_TABLE.write();
        let mut num = 0;
        for fd in fd_table.ids() {
            if fd >= nfds as usize {
                break;
            }
            if f(fd_table.get(fd).unwrap().poll()?) {
                fds[fd / 8] |= 1 << (fd % 8);
                num += 1;
            }
        }
        Ok(num)
    }
    let deadline = timeout.map(|t| wall_time() + t);

    debug!(
        "select timeout: {:?} {} {} {} {}",
        timeout,
        nfds,
        read_fds.is_some(),
        write_fds.is_some(),
        except_fds.is_some()
    );

    loop {
        let num = fill(nfds, &mut read_fds, |state| state.readable)?
            + fill(nfds, &mut write_fds, |state| state.writable)?
            + fill(nfds, &mut except_fds, |_state| false /* TODO */)?;
        if num > 0 {
            return Ok(num as isize);
        }

        axtask::yield_now();
        if deadline.is_some_and(|d| wall_time() >= d) {
            return Ok(0);
        }
    }
}

pub fn sys_select(
    nfds: u32,
    read_fds: UserPtr<u8>,
    write_fds: UserPtr<u8>,
    except_fds: UserPtr<u8>,
    timeout: UserConstPtr<timeval>,
) -> LinuxResult<isize> {
    do_select(
        nfds,
        read_fds,
        write_fds,
        except_fds,
        nullable!(timeout.get_as_ref())?.map(|it| it.to_time_value()),
    )
}

pub fn sys_pselect6(
    nfds: u32,
    read_fds: UserPtr<u8>,
    write_fds: UserPtr<u8>,
    except_fds: UserPtr<u8>,
    timeout: UserConstPtr<timespec>,
    _sigmask: UserConstPtr<SignalSet>,
) -> LinuxResult<isize> {
    do_select(
        nfds,
        read_fds,
        write_fds,
        except_fds,
        nullable!(timeout.get_as_ref())?.map(|it| it.to_time_value()),
    )
}
