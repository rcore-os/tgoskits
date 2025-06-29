use core::{fmt, time::Duration};

use axerrno::{LinuxError, LinuxResult};
use axhal::time::wall_time;
use axsignal::SignalSet;
use bitmaps::Bitmap;
use linux_raw_sys::{
    general::{__FD_SETSIZE, __kernel_fd_set, timespec, timeval},
    select_macros::{FD_ISSET, FD_SET, FD_ZERO},
};

use crate::{
    file::get_file_like,
    ptr::{UserConstPtr, UserPtr, nullable},
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

#[derive(Debug)]
struct FdSets {
    nfds: usize,
    read: FdSet,
    write: FdSet,
    except: FdSet,
}

impl FdSets {
    fn new(
        nfds: usize,
        readfds: Option<&__kernel_fd_set>,
        writefds: Option<&__kernel_fd_set>,
        exceptfds: Option<&__kernel_fd_set>,
    ) -> Self {
        Self {
            nfds,
            read: FdSet::new(nfds, readfds),
            write: FdSet::new(nfds, writefds),
            except: FdSet::new(nfds, exceptfds),
        }
    }

    // TODO: unity select and poll implementation like asterinas:
    // https://github.com/asterinas/asterinas/blob/main/kernel/src/syscall/poll.rs
    fn poll(
        &self,
        mut readfds: Option<&mut __kernel_fd_set>,
        mut writefds: Option<&mut __kernel_fd_set>,
        mut exceptfds: Option<&mut __kernel_fd_set>,
    ) -> LinuxResult<usize> {
        unsafe {
            if let Some(readfds) = readfds.as_deref_mut() {
                FD_ZERO(readfds);
            }
            if let Some(writefds) = writefds.as_deref_mut() {
                FD_ZERO(writefds);
            }
            if let Some(exceptfds) = exceptfds.as_deref_mut() {
                FD_ZERO(exceptfds);
            }
        }

        let mut res = 0usize;
        for fd in &(self.read.0 | self.write.0 | self.except.0) {
            if fd >= self.nfds {
                break;
            }

            let f = get_file_like(fd as _)?;
            match f.poll() {
                Ok(state) => {
                    if state.readable
                        && self.read.0.get(fd)
                        && let Some(readfds) = readfds.as_deref_mut()
                    {
                        res += 1;
                        unsafe { FD_SET(fd as _, readfds) };
                    }
                    if state.writable
                        && self.write.0.get(fd)
                        && let Some(writefds) = writefds.as_deref_mut()
                    {
                        res += 1;
                        unsafe { FD_SET(fd as _, writefds) };
                    }
                }
                Err(e) => {
                    debug!("poll fd={} error: {:?}", fd, e);
                    if self.except.0.get(fd)
                        && let Some(exceptfds) = exceptfds.as_deref_mut()
                    {
                        res += 1;
                        unsafe { FD_SET(fd as _, exceptfds) };
                    }
                }
            }
        }

        Ok(res)
    }
}

fn do_select(
    nfds: u32,
    readfds: UserPtr<__kernel_fd_set>,
    writefds: UserPtr<__kernel_fd_set>,
    exceptfds: UserPtr<__kernel_fd_set>,
    timeout: Option<Duration>,
) -> LinuxResult<isize> {
    if nfds > __FD_SETSIZE {
        return Err(LinuxError::EINVAL);
    }

    let mut readfds = nullable!(readfds.get_as_mut())?;
    let mut writefds = nullable!(writefds.get_as_mut())?;
    let mut exceptfds = nullable!(exceptfds.get_as_mut())?;

    let sets = FdSets::new(
        nfds as usize,
        readfds.as_deref(),
        writefds.as_deref(),
        exceptfds.as_deref(),
    );

    debug!(
        "sys_select <= nfds: {} sets: {:?} timeout: {:?}",
        nfds, sets, timeout
    );

    let deadline = timeout.map(|t| wall_time() + t);

    loop {
        axnet::poll_interfaces();

        let res = sets.poll(
            readfds.as_deref_mut(),
            writefds.as_deref_mut(),
            exceptfds.as_deref_mut(),
        )?;
        if res > 0 {
            return Ok(res as _);
        }

        if res > 0 {
            return Ok(res as _);
        }

        axtask::yield_now();

        if deadline.is_some_and(|d| wall_time() >= d) {
            return Ok(0);
        }
    }
}

pub fn sys_select(
    nfds: u32,
    readfds: UserPtr<__kernel_fd_set>,
    writefds: UserPtr<__kernel_fd_set>,
    exceptfds: UserPtr<__kernel_fd_set>,
    timeout: UserConstPtr<timeval>,
) -> LinuxResult<isize> {
    do_select(
        nfds,
        readfds,
        writefds,
        exceptfds,
        nullable!(timeout.get_as_ref())?
            .map(|it| it.try_into_time_value())
            .transpose()?,
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
    _sigmask: UserConstPtr<SignalSetWithSize>,
) -> LinuxResult<isize> {
    // FIXME: process sigmask
    do_select(
        nfds,
        readfds,
        writefds,
        exceptfds,
        nullable!(timeout.get_as_ref())?
            .map(|ts| ts.try_into_time_value())
            .transpose()?,
    )
}
