use alloc::{boxed::Box, collections::BTreeMap, sync::Arc};
use core::{
    cell::UnsafeCell,
    ffi::{c_int, c_void},
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
};

use ax_errno::{LinuxError, LinuxResult};
use ax_lazyinit::LazyLock;
use ax_runtime::task::ThreadHandle;

use crate::{ctypes, sync::Mutex};

pub mod mutex;

static TID_TO_PTHREAD: LazyLock<Mutex<BTreeMap<u64, ForceSendSync<ctypes::pthread_t>>>> =
    LazyLock::new(|| {
        let mut map = BTreeMap::new();
        let main_task = ax_runtime::task::current_thread_handle()
            .unwrap_or_else(|error| panic!("main pthread task is unavailable: {error}"));
        let main_tid = main_task.id().as_u64();
        let main_thread = Pthread {
            inner: main_task.clone(),
            retval: Arc::new(Packet {
                result: UnsafeCell::new(core::ptr::null_mut()),
            }),
            join_state: Arc::new(PthreadJoinState::new()),
        };
        let ptr = Box::into_raw(Box::new(main_thread)) as *mut c_void;
        map.insert(main_tid, ForceSendSync(ptr));
        Mutex::new(map)
    });

struct Packet<T> {
    result: UnsafeCell<T>,
}

unsafe impl<T> Send for Packet<T> {}
unsafe impl<T> Sync for Packet<T> {}

pub struct Pthread {
    inner: ThreadHandle,
    retval: Arc<Packet<*mut c_void>>,
    join_state: Arc<PthreadJoinState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PthreadCreateOptions {
    stack_size: usize,
}

impl PthreadCreateOptions {
    /// Decodes the stack option from the pthread ABI object.
    ///
    /// # Safety
    ///
    /// A non-null `attr` must point to an initialized `pthread_attr_t` that
    /// remains readable for this call.
    unsafe fn from_attr(attr: *const ctypes::pthread_attr_t) -> Self {
        if attr.is_null() {
            return Self {
                stack_size: crate::config::TASK_STACK_SIZE,
            };
        }

        // SAFETY: upheld by the caller. `_a_stacksize` is `__s[0]` in the
        // pthread ABI header from which `ctypes::pthread_attr_t` is generated.
        let stack_size = unsafe { (*attr).__u.__s[0] as usize };
        Self { stack_size }
    }
}

fn spawn_pthread_with<F, R>(
    options: PthreadCreateOptions,
    entry: F,
    spawn: impl FnOnce(F, alloc::string::String, usize) -> R,
) -> R {
    spawn(entry, alloc::string::String::new(), options.stack_size)
}

impl Pthread {
    /// # Safety
    ///
    /// A non-null `attr` must point to an initialized `pthread_attr_t` that
    /// remains readable until this function returns.
    unsafe fn create(
        attr: *const ctypes::pthread_attr_t,
        start_routine: extern "C" fn(arg: *mut c_void) -> *mut c_void,
        arg: *mut c_void,
    ) -> LinuxResult<ctypes::pthread_t> {
        // SAFETY: inherited from this function's contract.
        let options = unsafe { PthreadCreateOptions::from_attr(attr) };
        let arg_wrapper = ForceSendSync(arg);

        let my_packet: Arc<Packet<*mut c_void>> = Arc::new(Packet {
            result: UnsafeCell::new(core::ptr::null_mut()),
        });
        let their_packet = my_packet.clone();
        let join_state = Arc::new(PthreadJoinState::new());
        let child_join_state = Arc::clone(&join_state);
        let registered = Arc::new(AtomicBool::new(false));
        let child_registered = registered.clone();

        let main = move || {
            while !child_registered.load(Ordering::Acquire) {
                if let Err(error) = ax_runtime::task::yield_current_cpu() {
                    panic!("pthread registration yield failed: {error}");
                }
            }
            let arg = arg_wrapper;
            let ret = start_routine(arg.0);
            unsafe { *their_packet.result.get() = ret };
            drop(their_packet);
            if child_join_state.complete() {
                Self::reap_current_detached();
            }
        };

        let task_inner =
            spawn_pthread_with(options, main, ax_runtime::task::spawn_raw).map_err(|error| {
                warn!("failed to spawn pthread scheduler task: {error}");
                LinuxError::EAGAIN
            })?;
        let tid = task_inner.id().as_u64();
        let thread = Pthread {
            inner: task_inner,
            retval: my_packet,
            join_state,
        };
        let ptr = Box::into_raw(Box::new(thread)) as *mut c_void;
        TID_TO_PTHREAD.lock().insert(tid, ForceSendSync(ptr));
        registered.store(true, Ordering::Release);
        Ok(ptr)
    }

    fn current_ptr() -> *mut Pthread {
        let tid = ax_runtime::task::current_thread_id()
            .unwrap_or_else(|error| panic!("current pthread task is unavailable: {error}"))
            .as_u64();
        match TID_TO_PTHREAD.lock().get(&tid) {
            None => core::ptr::null_mut(),
            Some(ptr) => ptr.0 as *mut Pthread,
        }
    }

    fn current() -> Option<&'static Pthread> {
        unsafe { core::ptr::NonNull::new(Self::current_ptr()).map(|ptr| ptr.as_ref()) }
    }

    #[track_caller]
    fn exit_current(retval: *mut c_void) -> ! {
        let thread = Self::current().expect("fail to get current thread");
        unsafe { *thread.retval.result.get() = retval };
        let join_state = Arc::clone(&thread.join_state);
        if join_state.complete() {
            Self::reap_current_detached();
        }
        ax_runtime::task::exit_current(0)
    }

    #[track_caller]
    fn join(ptr: ctypes::pthread_t) -> LinuxResult<*mut c_void> {
        if core::ptr::eq(ptr, Self::current_ptr() as _) {
            return Err(LinuxError::EDEADLK);
        }

        let thread = Self::claim_join(ptr)?;
        let scheduler_exit_code = match ax_runtime::task::wait_thread(&thread.inner) {
            Ok(exit_code) => exit_code,
            Err(error) => {
                thread.join_state.release_join();
                warn!("failed to join pthread scheduler task: {error}");
                return Err(LinuxError::EAGAIN);
            }
        };

        let tid = thread.inner.id().as_u64();
        let retval = unsafe { *thread.retval.result.get() };
        let removed = {
            let mut threads = TID_TO_PTHREAD.lock();
            if threads
                .get(&tid)
                .is_some_and(|registered| core::ptr::eq(registered.0, ptr))
            {
                threads.remove(&tid);
                true
            } else {
                false
            }
        };
        if !removed {
            thread.join_state.release_join();
            return Err(LinuxError::ESRCH);
        }

        // SAFETY: `claim_join` proved this exact allocation was registered and
        // granted this caller the unique join claim. The target has exited and
        // the map entry was removed above, so no current-thread lookup can
        // access the allocation after ownership is reconstructed here.
        let thread = unsafe { Box::from_raw(ptr as *mut Pthread) };
        let Pthread { inner, .. } = *thread;
        let reaped_exit_code = ax_runtime::task::join_thread(inner)
            .unwrap_or_else(|error| panic!("failed to reap an exited pthread: {error}"));
        assert_eq!(
            reaped_exit_code, scheduler_exit_code,
            "pthread exit code changed between wait and reap"
        );
        Ok(retval)
    }

    fn detach(ptr: ctypes::pthread_t) -> LinuxResult {
        let (tid, reap_completed) = {
            let threads = TID_TO_PTHREAD.lock();
            let registered = threads
                .values()
                .find(|registered| core::ptr::eq(registered.0, ptr))
                .ok_or(LinuxError::ESRCH)?;
            // SAFETY: the map read guard prevents removal of this allocation
            // while the detach state transition and ID read are in progress.
            let thread = unsafe { &*(registered.0 as *const Pthread) };
            let reap_completed = thread.join_state.detach()?;
            (thread.inner.id().as_u64(), reap_completed)
        };

        if reap_completed {
            let thread = Self::remove_registered(tid, ptr).ok_or(LinuxError::ESRCH)?;
            drop(thread);
        }
        Ok(())
    }

    fn claim_join(ptr: ctypes::pthread_t) -> LinuxResult<&'static Pthread> {
        let threads = TID_TO_PTHREAD.lock();
        let registered = threads
            .values()
            .find(|registered| core::ptr::eq(registered.0, ptr))
            .ok_or(LinuxError::ESRCH)?;
        // SAFETY: the read guard prevents a successful joiner from removing
        // and freeing this registered allocation until after the atomic claim.
        let thread = unsafe { &*(registered.0 as *const Pthread) };
        if !thread.join_state.claim_join() {
            return Err(LinuxError::EINVAL);
        }
        Ok(thread)
    }

    fn reap_current_detached() {
        let tid = ax_runtime::task::current_thread_id()
            .unwrap_or_else(|error| panic!("current pthread task is unavailable: {error}"))
            .as_u64();
        let ptr = TID_TO_PTHREAD
            .lock()
            .get(&tid)
            .unwrap_or_else(|| panic!("detached pthread {tid} is not registered"))
            .0;
        let thread = Self::remove_registered(tid, ptr)
            .unwrap_or_else(|| panic!("detached pthread {tid} changed during exit"));
        drop(thread);
    }

    fn remove_registered(tid: u64, ptr: ctypes::pthread_t) -> Option<Box<Pthread>> {
        let mut threads = TID_TO_PTHREAD.lock();
        if !threads
            .get(&tid)
            .is_some_and(|registered| core::ptr::eq(registered.0, ptr))
        {
            return None;
        }
        threads.remove(&tid);
        // SAFETY: removing the exact registered pointer transfers the map's
        // allocation ownership to this caller. The join state guarantees that
        // only the successful joiner or detached-exit side can reach here.
        Some(unsafe { Box::from_raw(ptr as *mut Pthread) })
    }
}

const PTHREAD_JOINABLE: u8 = 0;
const PTHREAD_JOINING: u8 = 1;
const PTHREAD_DETACHED: u8 = 2;
const PTHREAD_COMPLETED: u8 = 3;
const PTHREAD_JOINING_COMPLETED: u8 = 4;

struct PthreadJoinState(AtomicU8);

impl PthreadJoinState {
    const fn new() -> Self {
        Self(AtomicU8::new(PTHREAD_JOINABLE))
    }

    fn claim_join(&self) -> bool {
        let mut state = self.0.load(Ordering::Acquire);
        loop {
            let next = match state {
                PTHREAD_JOINABLE => PTHREAD_JOINING,
                PTHREAD_COMPLETED => PTHREAD_JOINING_COMPLETED,
                PTHREAD_JOINING | PTHREAD_JOINING_COMPLETED | PTHREAD_DETACHED => return false,
                _ => panic!("invalid pthread join state {state}"),
            };
            match self
                .0
                .compare_exchange_weak(state, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(observed) => state = observed,
            }
        }
    }

    fn release_join(&self) {
        let mut state = self.0.load(Ordering::Acquire);
        loop {
            let next = match state {
                PTHREAD_JOINING => PTHREAD_JOINABLE,
                PTHREAD_JOINING_COMPLETED => PTHREAD_COMPLETED,
                _ => panic!("released unclaimed pthread join state {state}"),
            };
            match self
                .0
                .compare_exchange_weak(state, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(observed) => state = observed,
            }
        }
    }

    /// Publishes that the start routine and its libc teardown have completed.
    ///
    /// Returns `true` when the thread was detached and therefore owns removal
    /// of its pthread registration before entering the scheduler exit path.
    fn complete(&self) -> bool {
        let mut state = self.0.load(Ordering::Acquire);
        loop {
            let next = match state {
                PTHREAD_JOINABLE => PTHREAD_COMPLETED,
                PTHREAD_JOINING => PTHREAD_JOINING_COMPLETED,
                PTHREAD_DETACHED => return true,
                PTHREAD_COMPLETED | PTHREAD_JOINING_COMPLETED => {
                    panic!("pthread completion published twice")
                }
                _ => panic!("invalid pthread join state {state}"),
            };
            match self
                .0
                .compare_exchange_weak(state, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return false,
                Err(observed) => state = observed,
            }
        }
    }

    /// Marks a joinable pthread detached.
    ///
    /// Returns `true` when the start routine already completed, making the
    /// detaching caller responsible for reclaiming the registration.
    fn detach(&self) -> LinuxResult<bool> {
        let mut state = self.0.load(Ordering::Acquire);
        loop {
            let reap_completed = match state {
                PTHREAD_JOINABLE => false,
                PTHREAD_COMPLETED => true,
                PTHREAD_JOINING | PTHREAD_JOINING_COMPLETED | PTHREAD_DETACHED => {
                    return Err(LinuxError::EINVAL);
                }
                _ => panic!("invalid pthread join state {state}"),
            };
            match self.0.compare_exchange_weak(
                state,
                PTHREAD_DETACHED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(reap_completed),
                Err(observed) => state = observed,
            }
        }
    }
}

/// Returns the `pthread` struct of current thread.
pub fn sys_pthread_self() -> ctypes::pthread_t {
    Pthread::current().expect("fail to get current thread") as *const Pthread as _
}

/// Create a new thread with the given entry point and argument.
///
/// If successful, it stores the pointer to the newly created `struct __pthread`
/// in `res` and returns 0.
pub unsafe fn sys_pthread_create(
    res: *mut ctypes::pthread_t,
    attr: *const ctypes::pthread_attr_t,
    start_routine: extern "C" fn(arg: *mut c_void) -> *mut c_void,
    arg: *mut c_void,
) -> c_int {
    debug!(
        "sys_pthread_create <= {:#x}, {:#x}",
        start_routine as usize, arg as usize
    );
    syscall_body!(sys_pthread_create, {
        // SAFETY: the C ABI caller owns the optional initialized attribute
        // object for the duration of this synchronous call.
        let ptr = unsafe { Pthread::create(attr, start_routine, arg) }?;
        unsafe { core::ptr::write(res, ptr) };
        Ok(0)
    })
}

/// Exits the current thread. The value `retval` will be returned to the joiner.
#[track_caller]
pub fn sys_pthread_exit(retval: *mut c_void) -> ! {
    debug!("sys_pthread_exit <= {:#x}", retval as usize);
    Pthread::exit_current(retval);
}

/// Waits for the given thread to exit, and stores the return value in `retval`.
#[track_caller]
pub unsafe fn sys_pthread_join(thread: ctypes::pthread_t, retval: *mut *mut c_void) -> c_int {
    debug!("sys_pthread_join <= {:#x}", retval as usize);
    syscall_body!(sys_pthread_join, {
        let ret = Pthread::join(thread)?;
        if !retval.is_null() {
            unsafe { core::ptr::write(retval, ret) };
        }
        Ok(0)
    })
}

/// Marks a joinable thread detached so its resources are reclaimed on exit.
pub fn sys_pthread_detach(thread: ctypes::pthread_t) -> c_int {
    debug!("sys_pthread_detach <= {:#x}", thread as usize);
    syscall_body!(sys_pthread_detach, {
        Pthread::detach(thread)?;
        Ok(0)
    })
}

#[derive(Clone, Copy)]
struct ForceSendSync<T>(T);

unsafe impl<T> Send for ForceSendSync<T> {}
unsafe impl<T> Sync for ForceSendSync<T> {}

#[cfg(test)]
mod tests {
    use ax_errno::LinuxError;

    use super::{PthreadCreateOptions, PthreadJoinState, spawn_pthread_with};

    #[test]
    fn pthread_create_forwards_the_attribute_stack_size_to_the_runtime() {
        let mut attr = crate::ctypes::pthread_attr_t::default();
        let requested_stack_size = crate::config::TASK_STACK_SIZE + 0x4000;
        // SAFETY: selecting the `__s` union member initializes the exact ABI
        // word consumed by `PthreadCreateOptions::from_attr`.
        unsafe { attr.__u.__s[0] = requested_stack_size as _ };

        // SAFETY: `attr` is an initialized local value with the ABI layout
        // generated from this crate's public pthread header.
        let options = unsafe { PthreadCreateOptions::from_attr(&attr) };
        let forwarded_stack_size = spawn_pthread_with(options, (), |(), name, stack_size| {
            assert!(name.is_empty());
            stack_size
        });

        assert_eq!(forwarded_stack_size, requested_stack_size);
    }

    #[test]
    fn null_pthread_attributes_use_the_runtime_default_stack_size() {
        // SAFETY: null requests the documented default options.
        let options = unsafe { PthreadCreateOptions::from_attr(core::ptr::null()) };
        let forwarded_stack_size =
            spawn_pthread_with(options, (), |(), _name, stack_size| stack_size);

        assert_eq!(forwarded_stack_size, crate::config::TASK_STACK_SIZE);
    }

    #[test]
    fn join_claim_is_exclusive_and_can_be_retried_after_release() {
        let state = PthreadJoinState::new();
        assert!(state.claim_join());
        assert!(!state.claim_join());

        state.release_join();
        assert!(state.claim_join());
    }

    #[test]
    fn detached_thread_exit_requests_reaping() {
        let state = PthreadJoinState::new();
        assert_eq!(state.detach(), Ok(false));
        assert!(state.complete());
        assert_eq!(state.detach(), Err(LinuxError::EINVAL));
        assert!(!state.claim_join());
    }

    #[test]
    fn detaching_completed_thread_requests_reaping() {
        let state = PthreadJoinState::new();
        assert!(!state.complete());
        assert_eq!(state.detach(), Ok(true));
        assert!(!state.claim_join());
    }

    #[test]
    fn failed_join_preserves_concurrent_completion() {
        let state = PthreadJoinState::new();
        assert!(state.claim_join());
        assert!(!state.complete());

        state.release_join();
        assert_eq!(state.detach(), Ok(true));
    }
}
