//! `epoll` implementation.

use alloc::{
    collections::{BTreeMap, btree_map::Entry},
    sync::{Arc, Weak},
};
use core::{ffi::c_int, time::Duration};

use ax_hal::time::monotonic_time;

use crate::{
    PosixError, PosixResult, ctypes,
    imp::fd_ops::{FileLike, add_file_like, get_file_like},
    sync::Mutex,
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
    events: Mutex<BTreeMap<usize, WatchedEvent>>,
}

struct WatchedEvent {
    file: Weak<dyn FileLike>,
    event: ctypes::epoll_event,
    last_readable: bool,
    last_writable: bool,
    last_read_version: u64,
    last_write_version: u64,
    disabled: bool,
}

unsafe impl Send for ctypes::epoll_event {}
unsafe impl Sync for ctypes::epoll_event {}

impl WatchedEvent {
    fn new(file: Arc<dyn FileLike>, event: ctypes::epoll_event) -> Self {
        Self {
            file: Arc::downgrade(&file),
            event,
            last_readable: false,
            last_writable: false,
            last_read_version: 0,
            last_write_version: 0,
            disabled: false,
        }
    }

    fn update(&mut self, file: Arc<dyn FileLike>, event: ctypes::epoll_event) {
        self.file = Arc::downgrade(&file);
        self.event = event;
        self.last_readable = false;
        self.last_writable = false;
        self.last_read_version = 0;
        self.last_write_version = 0;
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

    fn current_ready(&self) -> (u32, ax_io::PollState) {
        let Some(file) = self.file.upgrade() else {
            return (0, ax_io::PollState::default());
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
                (ready, state)
            }
            Err(_) => (
                ctypes::EPOLLERR,
                ax_io::PollState {
                    readable: false,
                    writable: false,
                    ..Default::default()
                },
            ),
        }
    }

    fn deliverable_events(&self, ready: u32, state: ax_io::PollState) -> u32 {
        if self.disabled {
            return 0;
        }
        // Edge-triggered delivery tracks each event class independently.
        //
        // EPOLLIN is a read-readiness wake: driven by the file's read-readiness
        // version (bumped on every read wake, e.g. each `eventfd` write, which
        // the async waker path relies on) OR by a fresh false->true readability
        // transition.
        //
        // EPOLLOUT mirrors it with the file's write-readiness version (bumped
        // when writability changes, e.g. a pipe ring buffer going Full ->
        // Normal) OR by a fresh false->true writability transition. The
        // directions are independent: an `eventfd` write bumps only the read
        // version, so it never spoofs a writable edge, and a single shared
        // version gating both classes would re-fire EPOLLOUT on every write
        // (see the review on the eventfd readiness PR).
        //
        // EPOLLERR / EPOLLHUP are always reported.
        let events = if self.is_edge_triggered() {
            let epollin_edge = state.readable
                && (!self.last_readable || state.read_readiness_version != self.last_read_version);
            let epollout_edge = state.writable
                && (!self.last_writable
                    || state.write_readiness_version != self.last_write_version);
            let mut e = ready & EPOLL_ERROR_EVENTS;
            if epollin_edge {
                e |= ready & EPOLL_READ_EVENTS;
            }
            if epollout_edge {
                e |= ready & EPOLL_WRITE_EVENTS;
            }
            e
        } else {
            ready
        };
        events & EPOLL_RETURN_EVENTS
    }
}

impl EpollInstance {
    pub fn new(flags: usize) -> PosixResult<Self> {
        validate_create1_flags(flags)?;
        Ok(Self {
            events: Mutex::new(BTreeMap::new()),
        })
    }

    fn from_fd(fd: c_int) -> PosixResult<Arc<Self>> {
        get_file_like(fd)?
            .into_any()
            .downcast::<EpollInstance>()
            .map_err(|_| PosixError::EINVAL)
    }

    fn control(
        &self,
        op: c_int,
        fd: c_int,
        event: Option<&ctypes::epoll_event>,
    ) -> PosixResult<usize> {
        match op as u32 {
            ctypes::EPOLL_CTL_ADD => {
                let event = *event.ok_or(PosixError::EFAULT)?;
                validate_event_flags(event.events)?;
                let file = get_file_like(fd)?;
                if is_epoll_file(&file) {
                    return Err(PosixError::ELOOP);
                }
                let mut events = self.events.lock();
                events.retain(|_, watch| !watch.is_closed());
                if let Entry::Vacant(e) = events.entry(fd as usize) {
                    e.insert(WatchedEvent::new(file, event));
                } else {
                    return Err(PosixError::EEXIST);
                }
            }
            ctypes::EPOLL_CTL_MOD => {
                let event = *event.ok_or(PosixError::EFAULT)?;
                validate_event_flags(event.events)?;
                let file = get_file_like(fd)?;
                if is_epoll_file(&file) {
                    return Err(PosixError::ELOOP);
                }
                let mut events = self.events.lock();
                events.retain(|_, watch| !watch.is_closed());
                if let Entry::Occupied(mut ocp) = events.entry(fd as usize) {
                    ocp.get_mut().update(file, event);
                } else {
                    return Err(PosixError::ENOENT);
                }
            }
            ctypes::EPOLL_CTL_DEL => {
                let mut events = self.events.lock();
                if let Entry::Occupied(ocp) = events.entry(fd as usize) {
                    ocp.remove_entry();
                } else {
                    return Err(PosixError::ENOENT);
                }
            }
            _ => {
                return Err(PosixError::EINVAL);
            }
        }
        Ok(0)
    }

    fn poll_all(&self, events: &mut [ctypes::epoll_event]) -> PosixResult<usize> {
        let mut ready_list = self.events.lock();
        ready_list.retain(|_, watch| !watch.is_closed());
        let mut events_num = 0;

        for watch in ready_list.values_mut() {
            if events_num == events.len() {
                break;
            }

            let (ready, state) = watch.current_ready();
            let deliverable = watch.deliverable_events(ready, state);
            watch.last_read_version = state.read_readiness_version;
            watch.last_write_version = state.write_readiness_version;
            watch.last_readable = state.readable;
            watch.last_writable = state.writable;
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
            let (ready, state) = watch.current_ready();
            watch.deliverable_events(ready, state) != 0
        })
    }
}

impl FileLike for EpollInstance {
    fn read(&self, _buf: &mut [u8]) -> PosixResult<usize> {
        Err(PosixError::EINVAL)
    }

    fn write(&self, _buf: &[u8]) -> PosixResult<usize> {
        Err(PosixError::EINVAL)
    }

    fn stat(&self) -> PosixResult<ctypes::stat> {
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

    fn poll(&self) -> PosixResult<ax_io::PollState> {
        Ok(ax_io::PollState {
            readable: self.has_ready_events(),
            writable: false,
            read_readiness_version: 0,
            write_readiness_version: 0,
        })
    }

    fn set_nonblocking(&self, _nonblocking: bool) -> PosixResult {
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
            return Err(PosixError::EINVAL);
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
            return Err(PosixError::EINVAL);
        }
        let event = match op as u32 {
            ctypes::EPOLL_CTL_ADD | ctypes::EPOLL_CTL_MOD => {
                if event.is_null() {
                    return Err(PosixError::EFAULT);
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
            return Err(PosixError::EINVAL);
        }
        if events.is_null() {
            return Err(PosixError::EFAULT);
        }
        let events = unsafe { core::slice::from_raw_parts_mut(events, maxevents as usize) };
        let deadline = (!timeout.is_negative())
            .then(|| monotonic_time() + Duration::from_millis(timeout as u64));
        let epoll_instance = EpollInstance::from_fd(epfd)?;
        loop {
            #[cfg(feature = "net")]
            ax_net::request_poll();
            let events_num = epoll_instance.poll_all(events)?;
            if events_num > 0 {
                return Ok(events_num as c_int);
            }

            if deadline.is_some_and(|ddl| monotonic_time() >= ddl) {
                debug!("    timeout!");
                return Ok(0);
            }
            crate::sys_sched_yield();
        }
    })
}

fn validate_create1_flags(flags: usize) -> PosixResult {
    if (flags as u32) & !EPOLL_CREATE1_SUPPORTED_FLAGS != 0 {
        return Err(PosixError::EINVAL);
    }
    Ok(())
}

fn validate_event_flags(events: u32) -> PosixResult {
    if events & !EPOLL_SUPPORTED_EVENTS != 0 {
        return Err(PosixError::EINVAL);
    }
    Ok(())
}

fn is_epoll_file(file: &Arc<dyn FileLike>) -> bool {
    file.clone().into_any().downcast::<EpollInstance>().is_ok()
}
