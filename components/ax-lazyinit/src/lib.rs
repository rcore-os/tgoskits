#![cfg_attr(not(test), no_std)]
#![doc = include_str!("../README.md")]

#[cfg(test)]
extern crate alloc;

use core::{
    cell::UnsafeCell,
    fmt,
    hint::spin_loop,
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicU8, Ordering},
};

/// Not initialized yet.
const UNINIT: u8 = 0;
/// Initialization in progress.
const INITIALIZING: u8 = 1;
/// Successfully initialized.
const INITED: u8 = 2;

/// A wrapper of a lazy initialized value.
///
/// It implements [`Deref`] and [`DerefMut`]. The caller must use the dereference
/// operation after initialization, otherwise it will panic.
pub struct LazyInit<T> {
    inited: AtomicU8,
    data: UnsafeCell<MaybeUninit<T>>,
}

unsafe impl<T: Send + Sync> Sync for LazyInit<T> {}
unsafe impl<T: Send> Send for LazyInit<T> {}

impl<T> LazyInit<T> {
    /// Creates a new uninitialized value.
    pub const fn new() -> Self {
        Self {
            inited: AtomicU8::new(UNINIT),
            data: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Initializes the value once and only once.
    ///
    /// # Panics
    ///
    /// Panics if the value is already initialized.
    pub fn init_once(&self, data: T) -> &T {
        self.call_once(|| data).expect("Already initialized")
    }

    /// Performs an initialization routine once and only once.
    ///
    /// If the value is already initialized, the function will not be called
    /// and a [`None`] will be returned.
    pub fn call_once<F>(&self, f: F) -> Option<&T>
    where
        F: FnOnce() -> T,
    {
        // Fast path check
        if self.is_inited() {
            return None;
        }
        loop {
            match self.inited.compare_exchange_weak(
                UNINIT,
                INITIALIZING,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    let mut reset = InitializationReset::new(&self.inited);
                    let value = f();
                    unsafe { (*self.data.get()).as_mut_ptr().write(value) };
                    self.inited.store(INITED, Ordering::Release);
                    reset.disarm();
                    return Some(unsafe { self.force_get() });
                }
                Err(INITIALIZING) => {
                    while self.inited.load(Ordering::Acquire) == INITIALIZING {
                        spin_loop();
                    }
                    if self.inited.load(Ordering::Acquire) == UNINIT {
                        continue;
                    }
                    return None;
                }
                Err(INITED) => {
                    return None;
                }
                Err(UNINIT) => {
                    continue;
                }
                _ => unreachable!(),
            }
        }
    }

    /// Gets a reference to the value, initializing it with `f` if needed.
    ///
    /// If another CPU or thread is initializing the value concurrently, this
    /// method waits until that initialization completes and then returns the
    /// initialized value. The initializer is executed at most once.
    pub fn get_or_init<F>(&self, f: F) -> &T
    where
        F: FnOnce() -> T,
    {
        if let Some(value) = self.call_once(f) {
            value
        } else {
            debug_assert!(self.is_inited());
            // SAFETY: call_once either initialized the value in this call or
            // observed another completed initialization.
            unsafe { self.force_get() }
        }
    }

    /// Checks whether the value is initialized.
    #[inline]
    pub fn is_inited(&self) -> bool {
        self.inited.load(Ordering::Acquire) == INITED
    }

    /// Gets a reference to the value.
    ///
    /// Returns [`None`] if the value is not initialized.
    pub fn get(&self) -> Option<&T> {
        if self.is_inited() {
            Some(unsafe { self.force_get() })
        } else {
            None
        }
    }

    /// Gets a mutable reference to the value.
    ///
    /// Returns [`None`] if the value is not initialized.
    pub fn get_mut(&mut self) -> Option<&mut T> {
        if self.is_inited() {
            Some(unsafe { self.force_get_mut() })
        } else {
            None
        }
    }

    /// Gets the reference to the value without checking if it is initialized.
    ///
    /// # Safety
    ///
    /// Must be called after initialization.
    #[inline]
    pub unsafe fn get_unchecked(&self) -> &T {
        debug_assert!(self.is_inited());
        unsafe { self.force_get() }
    }

    /// Get a mutable reference to the value without checking if it is initialized.
    ///
    /// # Safety
    ///
    /// Must be called after initialization.
    #[inline]
    pub unsafe fn get_mut_unchecked(&mut self) -> &mut T {
        debug_assert!(self.is_inited());
        unsafe { self.force_get_mut() }
    }

    #[inline]
    unsafe fn force_get(&self) -> &T {
        unsafe { (*self.data.get()).assume_init_ref() }
    }

    #[inline]
    unsafe fn force_get_mut(&mut self) -> &mut T {
        unsafe { (*self.data.get()).assume_init_mut() }
    }

    fn panic_message(&self) -> ! {
        panic!(
            "Use uninitialized value: {:?}",
            core::any::type_name::<Self>()
        )
    }
}

struct InitializationReset<'a> {
    state: &'a AtomicU8,
    armed: bool,
}

impl<'a> InitializationReset<'a> {
    fn new(state: &'a AtomicU8) -> Self {
        Self { state, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for InitializationReset<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.state.store(UNINIT, Ordering::Release);
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for LazyInit<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.get() {
            Some(s) => write!(f, "LazyInit {{ data: ")
                .and_then(|()| s.fmt(f))
                .and_then(|()| write!(f, "}}")),
            None => write!(f, "LazyInit {{ <uninitialized> }}"),
        }
    }
}

impl<T> Default for LazyInit<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Deref for LazyInit<T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        if self.is_inited() {
            unsafe { self.force_get() }
        } else {
            self.panic_message()
        }
    }
}

impl<T> DerefMut for LazyInit<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        if self.is_inited() {
            unsafe { self.force_get_mut() }
        } else {
            self.panic_message()
        }
    }
}

impl<T> Drop for LazyInit<T> {
    fn drop(&mut self) {
        if self.is_inited() {
            unsafe { core::ptr::drop_in_place((*self.data.get()).as_mut_ptr()) };
        }
    }
}

/// A value which can be initialized exactly once.
///
/// Unlike [`LazyInit::init_once`], repeated [`call_once`](Self::call_once)
/// calls are idempotent and return the value selected by the first successful
/// initializer.
#[repr(transparent)]
pub struct OnceLock<T>(LazyInit<T>);

impl<T> OnceLock<T> {
    /// Creates an empty cell.
    pub const fn new() -> Self {
        Self(LazyInit::new())
    }

    /// Returns the stored value, running `initializer` at most once.
    pub fn call_once<F>(&self, initializer: F) -> &T
    where
        F: FnOnce() -> T,
    {
        self.0.get_or_init(initializer)
    }

    /// Returns the stored value, or `None` before initialization completes.
    pub fn get(&self) -> Option<&T> {
        self.0.get()
    }

    /// Returns mutable access when initialized.
    pub fn get_mut(&mut self) -> Option<&mut T> {
        self.0.get_mut()
    }

    /// Returns whether initialization completed successfully.
    pub fn is_initialized(&self) -> bool {
        self.0.is_inited()
    }
}

impl<T> Default for OnceLock<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: fmt::Debug> fmt::Debug for OnceLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A value initialized on first access.
pub struct LazyLock<T, F = fn() -> T> {
    value: OnceLock<T>,
    initializer: UnsafeCell<Option<F>>,
}

// SAFETY: `OnceLock` publishes `T` once with release/acquire ordering. Only
// the thread which wins initialization takes `F` from the UnsafeCell.
unsafe impl<T: Send + Sync, F: Send> Sync for LazyLock<T, F> {}
// SAFETY: exclusive ownership permits moving both the cell and initializer.
unsafe impl<T: Send, F: Send> Send for LazyLock<T, F> {}

impl<T, F> LazyLock<T, F> {
    /// Creates a lazily initialized value.
    pub const fn new(initializer: F) -> Self {
        Self {
            value: OnceLock::new(),
            initializer: UnsafeCell::new(Some(initializer)),
        }
    }
}

impl<T, F: FnOnce() -> T> LazyLock<T, F> {
    fn force(&self) -> &T {
        self.value.call_once(|| {
            // SAFETY: OnceLock executes this closure on exactly one thread;
            // initialization owns the only access to the initializer slot.
            let initializer = unsafe { &mut *self.initializer.get() }
                .take()
                .expect("LazyLock initializer was consumed before publication");
            initializer()
        })
    }
}

impl<T, F: FnOnce() -> T> Deref for LazyLock<T, F> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.force()
    }
}

impl<T: fmt::Debug, F: FnOnce() -> T> fmt::Debug for LazyLock<T, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.value.get() {
            Some(value) => f.debug_tuple("LazyLock").field(value).finish(),
            None => f.debug_tuple("LazyLock").field(&"<uninitialized>").finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        thread,
        time::Duration,
    };

    use super::*;

    #[test]
    fn lazyinit_basic() {
        static VALUE: LazyInit<u32> = LazyInit::new();
        assert!(!VALUE.is_inited());
        assert_eq!(VALUE.get(), None);

        VALUE.init_once(233);
        assert!(VALUE.is_inited());
        assert_eq!(*VALUE, 233);
        assert_eq!(VALUE.get(), Some(&233));
    }

    #[test]
    #[should_panic]
    fn panic_on_deref_before_init() {
        static VALUE: LazyInit<u32> = LazyInit::new();
        let _ = *VALUE;
    }

    #[test]
    #[should_panic]
    fn panic_on_double_init() {
        static VALUE: LazyInit<u32> = LazyInit::new();
        VALUE.init_once(1);
        VALUE.init_once(2);
    }

    #[test]
    fn lazyinit_concurrent() {
        const N: usize = 16;
        static VALUE: LazyInit<usize> = LazyInit::new();

        let threads: Vec<_> = (0..N)
            .map(|i| {
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(10));
                    VALUE.call_once(|| i)
                })
            })
            .collect();

        let mut ok = 0;
        for (i, thread) in threads.into_iter().enumerate() {
            if thread.join().unwrap().is_some() {
                ok += 1;
                assert_eq!(*VALUE, i);
            }
        }
        assert_eq!(ok, 1);
    }

    #[test]
    fn lazyinit_get_or_init() {
        static VALUE: LazyInit<u32> = LazyInit::new();
        assert_eq!(*VALUE.get_or_init(|| 123), 123);
        assert_eq!(*VALUE.get_or_init(|| 456), 123);
    }

    #[test]
    fn lazyinit_get_unchecked() {
        static VALUE: LazyInit<u32> = LazyInit::new();
        VALUE.init_once(123);
        let v = unsafe { VALUE.get_unchecked() };
        assert_eq!(*v, 123);
    }

    #[test]
    fn lazyinit_get_mut_unchecked() {
        let mut value: LazyInit<u32> = LazyInit::new();
        value.init_once(123);
        let v = unsafe { value.get_mut_unchecked() };
        *v += 3;
        assert_eq!(*v, 126);
    }

    #[test]
    fn once_lock_returns_the_first_value() {
        static VALUE: OnceLock<u32> = OnceLock::new();

        assert_eq!(*VALUE.call_once(|| 123), 123);
        assert_eq!(*VALUE.call_once(|| 456), 123);
        assert_eq!(VALUE.get(), Some(&123));
        assert!(VALUE.is_initialized());
    }

    #[test]
    fn once_lock_retries_after_initializer_panic() {
        static VALUE: OnceLock<u32> = OnceLock::new();

        assert!(
            std::panic::catch_unwind(|| VALUE.call_once(|| panic!("initializer failed"))).is_err()
        );
        assert!(!VALUE.is_initialized());
        assert_eq!(*VALUE.call_once(|| 789), 789);
    }

    #[test]
    fn lazy_lock_initializes_once() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        static VALUE: LazyLock<u32> = LazyLock::new(|| {
            CALLS.fetch_add(1, Ordering::Relaxed);
            42
        });

        assert_eq!(*VALUE, 42);
        assert_eq!(*VALUE, 42);
        assert_eq!(CALLS.load(Ordering::Relaxed), 1);
    }
}
