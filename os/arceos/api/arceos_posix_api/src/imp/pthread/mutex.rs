#[cfg(feature = "lockdep")]
use core::sync::atomic::{AtomicBool, Ordering};
use core::{
    ffi::c_int,
    mem::{ManuallyDrop, size_of},
    ptr::NonNull,
};

use ax_errno::LinuxResult;
use ax_sync::Mutex;

use crate::{ctypes, utils::check_null_mut_ptr};

const _: () = assert!(size_of::<ctypes::pthread_mutex_t>() == size_of::<PthreadMutex>());
#[cfg(feature = "lockdep")]
const STATIC_MUTEX_SENTINEL: i64 = -1;
#[cfg(feature = "lockdep")]
static STATIC_MUTEX_INIT_LOCK: AtomicBool = AtomicBool::new(false);

#[repr(C)]
pub struct PthreadMutex(Mutex<()>);

impl PthreadMutex {
    const fn new() -> Self {
        Self(Mutex::new(()))
    }

    fn lock(&self) -> LinuxResult {
        let _guard = ManuallyDrop::new(self.0.lock());
        Ok(())
    }

    fn unlock(&self) -> LinuxResult {
        unsafe { self.0.force_unlock() };
        Ok(())
    }
}

#[cfg(feature = "lockdep")]
fn with_static_mutex_init_lock<R>(f: impl FnOnce() -> R) -> R {
    while STATIC_MUTEX_INIT_LOCK
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        while STATIC_MUTEX_INIT_LOCK.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
    }
    let result = f();
    STATIC_MUTEX_INIT_LOCK.store(false, Ordering::Release);
    result
}

#[cfg(feature = "lockdep")]
fn ensure_mutex_initialized(mutex: NonNull<ctypes::pthread_mutex_t>) {
    let words = unsafe {
        core::slice::from_raw_parts_mut(
            mutex.as_ptr().cast::<i64>(),
            size_of::<ctypes::pthread_mutex_t>() / size_of::<i64>(),
        )
    };
    if words.first().copied() != Some(STATIC_MUTEX_SENTINEL) {
        return;
    }

    with_static_mutex_init_lock(|| {
        if words.first().copied() == Some(STATIC_MUTEX_SENTINEL) {
            unsafe {
                mutex
                    .cast::<PthreadMutex>()
                    .as_ptr()
                    .write(PthreadMutex::new());
            }
        }
    });
}

fn lock_mutex(mutex: NonNull<ctypes::pthread_mutex_t>) -> LinuxResult {
    #[cfg(feature = "lockdep")]
    ensure_mutex_initialized(mutex);
    unsafe { mutex.cast::<PthreadMutex>().as_ref().lock() }
}

fn unlock_mutex(mutex: NonNull<ctypes::pthread_mutex_t>) -> LinuxResult {
    #[cfg(feature = "lockdep")]
    ensure_mutex_initialized(mutex);
    unsafe { mutex.cast::<PthreadMutex>().as_ref().unlock() }
}

/// Initialize a mutex.
pub fn sys_pthread_mutex_init(
    mutex: *mut ctypes::pthread_mutex_t,
    _attr: *const ctypes::pthread_mutexattr_t,
) -> c_int {
    debug!("sys_pthread_mutex_init <= {:#x}", mutex as usize);
    syscall_body!(sys_pthread_mutex_init, {
        check_null_mut_ptr(mutex)?;
        unsafe {
            mutex.cast::<PthreadMutex>().write(PthreadMutex::new());
        }
        Ok(0)
    })
}

/// Lock the given mutex.
pub fn sys_pthread_mutex_lock(mutex: *mut ctypes::pthread_mutex_t) -> c_int {
    debug!("sys_pthread_mutex_lock <= {:#x}", mutex as usize);
    syscall_body!(sys_pthread_mutex_lock, {
        check_null_mut_ptr(mutex)?;
        let mutex = NonNull::new(mutex).expect("mutex pointer was checked for null");
        lock_mutex(mutex)?;
        Ok(0)
    })
}

/// Unlock the given mutex.
pub fn sys_pthread_mutex_unlock(mutex: *mut ctypes::pthread_mutex_t) -> c_int {
    debug!("sys_pthread_mutex_unlock <= {:#x}", mutex as usize);
    syscall_body!(sys_pthread_mutex_unlock, {
        check_null_mut_ptr(mutex)?;
        let mutex = NonNull::new(mutex).expect("mutex pointer was checked for null");
        unlock_mutex(mutex)?;
        Ok(0)
    })
}
