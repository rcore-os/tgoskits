//! `epoll` implementation.

use alloc::{
    collections::{BTreeMap, btree_map::Entry},
    sync::{Arc, Weak},
};
use core::{ffi::c_int, time::Duration};

use ax_errno::{LinuxError, LinuxResult};
use ax_hal::time::wall_time;
use ax_sync::PiMutex;

use crate::{
    ctypes,
    imp::fd_ops::{FileLike, add_file_like, get_file_like},
};

const EPOLL_READ_EVENTS: u32 =
    ctypes::EPOLLIN | ctypes::EPOLLPRI | ctypes::EPOLLRDNORM | ctypes::EPOLLRDBAND;
const EPOLL_WRITE_EVENTS: u32 = ctypes::EPOLLOUT | ctypes::EPOLLWRNORM | ctypes::EPOLLWRBAND;
const EPOLL_ERROR_EVENTS: u32 = ctypes::EPOLLERR | ctypes::EPOLLHUP | ctypes::EPOLLRDHUP;
const EPOLL_RETURN_EVENTS: u32 = EPOLL_READ_EVENTS | EPOLL_WRITE_EVENTS | EPOLL_ERROR_EVENTS;
const EPOLL_BEHAVIOR_FLAGS: u32 = ctypes::EPOLLET | ctypes::EPOLLONESHOT;
const EPOLL_SUPPORTED_EVENTS: u32 = EPOLL_RETURN_EVENTS | EPOLL_BEHAVIOR_FLAGS;
const EPOLL_CREATE1_SUPPORTED_FLAGS: u32 = ctypes::EPOLL_CLOEXEC;

pub struct EpollInstance {
    events: PiMutex<BTreeMap<usize, WatchedEvent>>,
}

struct WatchedEvent {
    file: Weak<dyn FileLike>,
    event: ctypes::epoll_event,
    last_ready: u32,
    last_readiness_version: u64,
    disabled: bool,
}

unsafe impl Send for ctypes::epoll_event {}
unsafe impl Sync for ctypes::epoll_event {}

impl WatchedEvent {
    fn new(file: Arc<dyn FileLike>, event: ctypes::epoll_event) -> Self {
        Self {
            file: Arc::downgrade(&file),
            event,
            last_ready: 0,
            last_readiness_version: 0,
            disabled: false,
        }
    }

    fn update(&mut self, file: Arc<dyn FileLike>, event: ctypes::epoll_event) {
        self.file = Arc::downgrade(&file);
        self.event = event;
        self.last_ready = 0;
        self.last_readiness_version = 0;
        self.disabled = false;
    }

    fn is_closed(&self) -> bool {
        self.file.strong_count() == 0
    }

    fn is_edge_triggered(&self) -> bool {
        self.event.events & ctypes::EPOLLET != 0
    }

    fn is_oneshot(&self) -> bool {
        self.event.events & ctypes::EPOLLONESHOT != 0
    }

    fn current_ready(&self) -> (u32, u64) {
        let Some(file) = self.file.upgrade() else {
            return (0, 0);
        };
        match file.poll() {
            Ok(state) => {
                let mut ready = 0;
                let interest = self.event.events;
                if state.readable {
                    ready |= interest & EPOLL_READ_EVENTS;
                }
                if state.writable {
                    ready |= interest & EPOLL_WRITE_EVENTS;
                }
                (ready, state.readiness_version)
            }
            Err(_) => (ctypes::EPOLLERR, 0),
        }
    }

    fn deliverable_events(&self, ready: u32, readiness_version: u64) -> u32 {
        if self.disabled {
            return 0;
        }
        let events = if self.is_edge_triggered() {
            if readiness_version == self.last_readiness_version {
                ready & !self.last_ready
            } else {
                ready
            }
        } else {
            ready
        };
        events & EPOLL_RETURN_EVENTS
    }
}

impl EpollInstance {
    pub fn new(flags: usize) -> LinuxResult<Self> {
        validate_create1_flags(flags)?;
        Ok(Self {
            events: PiMutex::new(BTreeMap::new()),
        })
    }

    fn from_fd(fd: c_int) -> LinuxResult<Arc<Self>> {
        get_file_like(fd)?
            .into_any()
            .downcast::<EpollInstance>()
            .map_err(|_| LinuxError::EINVAL)
    }

    fn control(
        &self,
        op: c_int,
        fd: c_int,
        event: Option<&ctypes::epoll_event>,
    ) -> LinuxResult<usize> {
        match op as u32 {
            ctypes::EPOLL_CTL_ADD => {
                let event = *event.ok_or(LinuxError::EFAULT)?;
                validate_event_flags(event.events)?;
                let file = get_file_like(fd)?;
                if is_epoll_file(&file) {
                    return Err(LinuxError::ELOOP);
                }
                let mut events = self.events.lock();
                events.retain(|_, watch| !watch.is_closed());
                if let Entry::Vacant(e) = events.entry(fd as usize) {
                    e.insert(WatchedEvent::new(file, event));
                } else {
                    return Err(LinuxError::EEXIST);
                }
            }
            ctypes::EPOLL_CTL_MOD => {
                let event = *event.ok_or(LinuxError::EFAULT)?;
                validate_event_flags(event.events)?;
                let file = get_file_like(fd)?;
                if is_epoll_file(&file) {
                    return Err(LinuxError::ELOOP);
                }
                let mut events = self.events.lock();
                events.retain(|_, watch| !watch.is_closed());
                if let Entry::Occupied(mut ocp) = events.entry(fd as usize) {
                    ocp.get_mut().update(file, event);
                } else {
                    return Err(LinuxError::ENOENT);
                }
            }
            ctypes::EPOLL_CTL_DEL => {
                let mut events = self.events.lock();
                if let Entry::Occupied(ocp) = events.entry(fd as usize) {
                    ocp.remove_entry();
                } else {
                    return Err(LinuxError::ENOENT);
                }
            }
            _ => {
                return Err(LinuxError::EINVAL);
            }
        }
        Ok(0)
    }

    fn poll_all(&self, events: &mut [ctypes::epoll_event]) -> LinuxResult<usize> {
        let mut ready_list = self.events.lock();
        ready_list.retain(|_, watch| !watch.is_closed());
        let mut events_num = 0;

        for watch in ready_list.values_mut() {
            if events_num == events.len() {
                break;
            }

            let (ready, readiness_version) = watch.current_ready();
            let deliverable = watch.deliverable_events(ready, readiness_version);
            watch.last_ready = ready;
            watch.last_readiness_version = readiness_version;
            if deliverable == 0 {
                continue;
            }

            events[events_num].events = deliverable;
            events[events_num].data = watch.event.data;
            events_num += 1;

            if watch.is_oneshot() {
                watch.disabled = true;
            }
        }
        Ok(events_num)
    }

    fn has_ready_events(&self) -> bool {
        let mut ready_list = self.events.lock();
        ready_list.retain(|_, watch| !watch.is_closed());
        ready_list.values().any(|watch| {
            let (ready, readiness_version) = watch.current_ready();
            watch.deliverable_events(ready, readiness_version) != 0
        })
    }
}

impl FileLike for EpollInstance {
    fn read(&self, _buf: &mut [u8]) -> LinuxResult<usize> {
        Err(LinuxError::EINVAL)
    }

    fn write(&self, _buf: &[u8]) -> LinuxResult<usize> {
        Err(LinuxError::EINVAL)
    }

    fn stat(&self) -> LinuxResult<ctypes::stat> {
        let st_mode = 0o600u32; // rw-------
        Ok(ctypes::stat {
            st_ino: 1,
            st_nlink: 1,
            st_mode,
            ..Default::default()
        })
    }

    fn into_any(self: Arc<Self>) -> alloc::sync::Arc<dyn core::any::Any + Send + Sync> {
        self
    }

    fn poll(&self) -> LinuxResult<ax_io::PollState> {
        Ok(ax_io::PollState {
            readable: self.has_ready_events(),
            writable: false,
            readiness_version: 0,
        })
    }

    fn set_nonblocking(&self, _nonblocking: bool) -> LinuxResult {
        Ok(())
    }
}

/// Creates a new epoll instance with creation flags.
pub fn sys_epoll_create1(flags: c_int) -> c_int {
    debug!("sys_epoll_create1 <= {flags}");
    syscall_body!(sys_epoll_create1, {
        let epoll_instance = EpollInstance::new(flags as usize)?;
        add_file_like(Arc::new(epoll_instance))
    })
}

/// Creates a new epoll instance.
///
/// It returns a file descriptor referring to the new epoll instance.
pub fn sys_epoll_create(size: c_int) -> c_int {
    debug!("sys_epoll_create <= {size}");
    syscall_body!(sys_epoll_create, {
        if size <= 0 {
            return Err(LinuxError::EINVAL);
        }
        let epoll_instance = EpollInstance::new(0)?;
        add_file_like(Arc::new(epoll_instance))
    })
}

/// Control interface for an epoll file descriptor
pub unsafe fn sys_epoll_ctl(
    epfd: c_int,
    op: c_int,
    fd: c_int,
    event: *mut ctypes::epoll_event,
) -> c_int {
    debug!("sys_epoll_ctl <= epfd: {epfd} op: {op} fd: {fd}");
    syscall_body!(sys_epoll_ctl, {
        if epfd == fd {
            return Err(LinuxError::EINVAL);
        }
        let event = match op as u32 {
            ctypes::EPOLL_CTL_ADD | ctypes::EPOLL_CTL_MOD => {
                if event.is_null() {
                    return Err(LinuxError::EFAULT);
                }
                Some(unsafe { &*event })
            }
            ctypes::EPOLL_CTL_DEL => None,
            _ => None,
        };
        let ret = EpollInstance::from_fd(epfd)?.control(op, fd, event)? as c_int;
        Ok(ret)
    })
}

/// Waits for events on the epoll instance referred to by the file descriptor epfd.
pub unsafe fn sys_epoll_wait(
    epfd: c_int,
    events: *mut ctypes::epoll_event,
    maxevents: c_int,
    timeout: c_int,
) -> c_int {
    debug!("sys_epoll_wait <= epfd: {epfd}, maxevents: {maxevents}, timeout: {timeout}");

    syscall_body!(sys_epoll_wait, {
        if maxevents <= 0 {
            return Err(LinuxError::EINVAL);
        }
        if events.is_null() {
            return Err(LinuxError::EFAULT);
        }
        let events = unsafe { core::slice::from_raw_parts_mut(events, maxevents as usize) };
        let deadline =
            (!timeout.is_negative()).then(|| wall_time() + Duration::from_millis(timeout as u64));
        let epoll_instance = EpollInstance::from_fd(epfd)?;
        loop {
            #[cfg(feature = "net")]
            ax_net::request_poll();
            let events_num = epoll_instance.poll_all(events)?;
            if events_num > 0 {
                return Ok(events_num as c_int);
            }

            if deadline.is_some_and(|ddl| wall_time() >= ddl) {
                debug!("    timeout!");
                return Ok(0);
            }
            crate::sys_sched_yield();
        }
    })
}

fn validate_create1_flags(flags: usize) -> LinuxResult {
    if (flags as u32) & !EPOLL_CREATE1_SUPPORTED_FLAGS != 0 {
        return Err(LinuxError::EINVAL);
    }
    Ok(())
}

fn validate_event_flags(events: u32) -> LinuxResult {
    if events & !EPOLL_SUPPORTED_EVENTS != 0 {
        return Err(LinuxError::EINVAL);
    }
    Ok(())
}

fn is_epoll_file(file: &Arc<dyn FileLike>) -> bool {
    file.clone().into_any().downcast::<EpollInstance>().is_ok()
}
