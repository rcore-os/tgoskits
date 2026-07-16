use alloc::{boxed::Box, collections::BTreeMap, sync::Arc};
use core::{
    cell::UnsafeCell,
    ffi::{c_int, c_void},
    sync::atomic::{AtomicBool, Ordering},
};

use ax_errno::{LinuxError, LinuxResult};
use ax_kspin::SpinRwLock as RwLock;
use ax_runtime::task::ThreadHandle;
use spin::LazyLock;

use crate::ctypes;

pub mod mutex;

static TID_TO_PTHREAD: LazyLock<RwLock<BTreeMap<u64, ForceSendSync<ctypes::pthread_t>>>> =
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
            join_claim: JoinClaim::new(),
        };
        let ptr = Box::into_raw(Box::new(main_thread)) as *mut c_void;
        map.insert(main_tid, ForceSendSync(ptr));
        RwLock::new(map)
    });

struct Packet<T> {
    result: UnsafeCell<T>,
}

unsafe impl<T> Send for Packet<T> {}
unsafe impl<T> Sync for Packet<T> {}

pub struct Pthread {
    inner: ThreadHandle,
    retval: Arc<Packet<*mut c_void>>,
    join_claim: JoinClaim,
}

impl Pthread {
    fn create(
        _attr: *const ctypes::pthread_attr_t,
        start_routine: extern "C" fn(arg: *mut c_void) -> *mut c_void,
        arg: *mut c_void,
    ) -> LinuxResult<ctypes::pthread_t> {
        let arg_wrapper = ForceSendSync(arg);

        let my_packet: Arc<Packet<*mut c_void>> = Arc::new(Packet {
            result: UnsafeCell::new(core::ptr::null_mut()),
        });
        let their_packet = my_packet.clone();
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
        };

        let task_inner = ax_runtime::task::spawn_raw(
            main,
            alloc::string::String::new(),
            crate::config::TASK_STACK_SIZE,
        )
        .map_err(|error| {
            warn!("failed to spawn pthread scheduler task: {error}");
            LinuxError::EAGAIN
        })?;
        let tid = task_inner.id().as_u64();
        let thread = Pthread {
            inner: task_inner,
            retval: my_packet,
            join_claim: JoinClaim::new(),
        };
        let ptr = Box::into_raw(Box::new(thread)) as *mut c_void;
        TID_TO_PTHREAD.write().insert(tid, ForceSendSync(ptr));
        registered.store(true, Ordering::Release);
        Ok(ptr)
    }

    fn current_ptr() -> *mut Pthread {
        let tid = ax_runtime::task::current_thread_id()
            .unwrap_or_else(|error| panic!("current pthread task is unavailable: {error}"))
            .as_u64();
        match TID_TO_PTHREAD.read().get(&tid) {
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
                thread.join_claim.release();
                warn!("failed to join pthread scheduler task: {error}");
                return Err(LinuxError::EAGAIN);
            }
        };

        let tid = thread.inner.id().as_u64();
        let retval = unsafe { *thread.retval.result.get() };
        let removed = {
            let mut threads = TID_TO_PTHREAD.write();
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
            thread.join_claim.release();
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

    fn claim_join(ptr: ctypes::pthread_t) -> LinuxResult<&'static Pthread> {
        let threads = TID_TO_PTHREAD.read();
        let registered = threads
            .values()
            .find(|registered| core::ptr::eq(registered.0, ptr))
            .ok_or(LinuxError::ESRCH)?;
        // SAFETY: the read guard prevents a successful joiner from removing
        // and freeing this registered allocation until after the atomic claim.
        let thread = unsafe { &*(registered.0 as *const Pthread) };
        if !thread.join_claim.try_acquire() {
            return Err(LinuxError::EINVAL);
        }
        Ok(thread)
    }
}

struct JoinClaim(AtomicBool);

impl JoinClaim {
    const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    fn try_acquire(&self) -> bool {
        self.0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn release(&self) {
        self.0.store(false, Ordering::Release);
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
        let ptr = Pthread::create(attr, start_routine, arg)?;
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

#[derive(Clone, Copy)]
struct ForceSendSync<T>(T);

unsafe impl<T> Send for ForceSendSync<T> {}
unsafe impl<T> Sync for ForceSendSync<T> {}

#[cfg(test)]
mod tests {
    use super::JoinClaim;

    #[test]
    fn join_claim_is_exclusive_and_can_be_retried_after_release() {
        let claim = JoinClaim::new();
        assert!(claim.try_acquire());
        assert!(!claim.try_acquire());

        claim.release();
        assert!(claim.try_acquire());
    }
}
