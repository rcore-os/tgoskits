use alloc::{boxed::Box, collections::BTreeMap};
use core::{
    ffi::c_int,
    mem,
    ptr::{self, NonNull},
    sync::atomic::{AtomicBool, Ordering},
};

use ax_errno::{LinuxError, LinuxResult};
use ax_sync::{PiMutex, SpinMutex};
use spin::LazyLock;

use crate::{ctypes, utils::check_null_mut_ptr};

const STATIC_MUTEX_SENTINEL: usize = usize::MAX;
static STATIC_MUTEX_INIT_LOCK: AtomicBool = AtomicBool::new(false);
static MUTEXES: LazyLock<SpinMutex<BTreeMap<usize, ForceSendSync<NonNull<PthreadMutex>>>>> =
    LazyLock::new(|| SpinMutex::new(BTreeMap::new()));

#[repr(C)]
pub struct PthreadMutex(PiMutex<()>);

impl PthreadMutex {
    const fn new() -> Self {
        Self(PiMutex::new(()))
    }

    fn lock(&self) -> LinuxResult {
        mem::forget(self.0.lock());
        Ok(())
    }

    fn try_lock(&self) -> LinuxResult {
        if let Some(guard) = self.0.try_lock() {
            mem::forget(guard);
            Ok(())
        } else {
            Err(LinuxError::EBUSY)
        }
    }

    fn unlock(&self) -> LinuxResult {
        unsafe { self.0.force_unlock() };
        Ok(())
    }
}

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

#[derive(Clone, Copy)]
struct ForceSendSync<T>(T);

unsafe impl<T> Send for ForceSendSync<T> {}
unsafe impl<T> Sync for ForceSendSync<T> {}

fn mutex_key(mutex: NonNull<ctypes::pthread_mutex_t>) -> usize {
    mutex.as_ptr() as usize
}

fn read_mutex_handle(mutex: NonNull<ctypes::pthread_mutex_t>) -> usize {
    unsafe { ptr::read_unaligned(mutex.as_ptr().cast::<usize>()) }
}

fn write_mutex_handle(mutex: NonNull<ctypes::pthread_mutex_t>, handle: usize) {
    unsafe { ptr::write_unaligned(mutex.as_ptr().cast::<usize>(), handle) }
}

fn create_mutex() -> NonNull<PthreadMutex> {
    let ptr = Box::into_raw(Box::new(PthreadMutex::new()));
    NonNull::new(ptr).expect("Box::into_raw never returns null")
}

fn ensure_mutex_initialized(mutex: NonNull<ctypes::pthread_mutex_t>) -> NonNull<PthreadMutex> {
    let handle = read_mutex_handle(mutex);
    if handle != 0 && handle != STATIC_MUTEX_SENTINEL {
        return NonNull::new(handle as *mut PthreadMutex).expect("stored pthread mutex handle");
    }

    with_static_mutex_init_lock(|| {
        let handle = read_mutex_handle(mutex);
        if handle != 0 && handle != STATIC_MUTEX_SENTINEL {
            return NonNull::new(handle as *mut PthreadMutex).expect("stored pthread mutex handle");
        }

        let inner = create_mutex();
        let handle = inner.as_ptr() as usize;
        write_mutex_handle(mutex, handle);
        MUTEXES
            .lock()
            .insert(mutex_key(mutex), ForceSendSync(inner));
        inner
    })
}

fn lock_mutex(mutex: NonNull<ctypes::pthread_mutex_t>) -> LinuxResult {
    unsafe { ensure_mutex_initialized(mutex).as_ref().lock() }
}

fn try_lock_mutex(mutex: NonNull<ctypes::pthread_mutex_t>) -> LinuxResult {
    unsafe { ensure_mutex_initialized(mutex).as_ref().try_lock() }
}

fn unlock_mutex(mutex: NonNull<ctypes::pthread_mutex_t>) -> LinuxResult {
    unsafe { ensure_mutex_initialized(mutex).as_ref().unlock() }
}

fn destroy_mutex(mutex: NonNull<ctypes::pthread_mutex_t>) -> LinuxResult {
    let handle = read_mutex_handle(mutex);
    if handle == 0 || handle == STATIC_MUTEX_SENTINEL {
        return Ok(());
    }

    if MUTEXES.lock().remove(&mutex_key(mutex)).is_some() {
        unsafe { drop(Box::from_raw(handle as *mut PthreadMutex)) };
        write_mutex_handle(mutex, 0);
        Ok(())
    } else {
        Err(LinuxError::EINVAL)
    }
}

/// Initialize a mutex.
pub fn sys_pthread_mutex_init(
    mutex: *mut ctypes::pthread_mutex_t,
    _attr: *const ctypes::pthread_mutexattr_t,
) -> c_int {
    debug!("sys_pthread_mutex_init <= {:#x}", mutex as usize);
    syscall_body!(sys_pthread_mutex_init, {
        check_null_mut_ptr(mutex)?;
        let mutex = NonNull::new(mutex).expect("mutex pointer was checked for null");
        let inner = create_mutex();
        write_mutex_handle(mutex, inner.as_ptr() as usize);
        MUTEXES
            .lock()
            .insert(mutex_key(mutex), ForceSendSync(inner));
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

/// Try locking the given mutex.
pub fn sys_pthread_mutex_trylock(mutex: *mut ctypes::pthread_mutex_t) -> c_int {
    debug!("sys_pthread_mutex_trylock <= {:#x}", mutex as usize);
    syscall_body!(sys_pthread_mutex_trylock, {
        check_null_mut_ptr(mutex)?;
        let mutex = NonNull::new(mutex).expect("mutex pointer was checked for null");
        try_lock_mutex(mutex)?;
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

/// Destroy the given mutex.
pub fn sys_pthread_mutex_destroy(mutex: *mut ctypes::pthread_mutex_t) -> c_int {
    debug!("sys_pthread_mutex_destroy <= {:#x}", mutex as usize);
    syscall_body!(sys_pthread_mutex_destroy, {
        check_null_mut_ptr(mutex)?;
        let mutex = NonNull::new(mutex).expect("mutex pointer was checked for null");
        destroy_mutex(mutex)?;
        Ok(0)
    })
}
