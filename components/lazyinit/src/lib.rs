#![cfg_attr(not(test), no_std)]
#![doc = include_str!("../README.md")]

use core::cell::UnsafeCell;
use core::fmt;
use core::hint::spin_loop;
use core::mem::MaybeUninit;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU8, Ordering};

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
                    let value = f();
                    unsafe { (*self.data.get()).as_mut_ptr().write(value) };
                    self.inited.store(INITED, Ordering::Release);
                    return Some(unsafe { self.force_get() });
                }
                Err(INITIALIZING) => {
                    while self.inited.load(Ordering::Acquire) == INITIALIZING {
                        spin_loop();
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
        self.force_get()
    }

    /// Get a mutable reference to the value without checking if it is initialized.
    ///
    /// # Safety
    ///
    /// Must be called after initialization.
    #[inline]
    pub unsafe fn get_mut_unchecked(&mut self) -> &mut T {
        debug_assert!(self.is_inited());
        self.force_get_mut()
    }

    #[inline]
    unsafe fn force_get(&self) -> &T {
        (*self.data.get()).assume_init_ref()
    }

    #[inline]
    unsafe fn force_get_mut(&mut self) -> &mut T {
        (*self.data.get()).assume_init_mut()
    }

    fn panic_message(&self) -> ! {
        panic!(
            "Use uninitialized value: {:?}",
            core::any::type_name::<Self>()
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

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
    fn lazyinit_get_unchecked() {
        static VALUE: LazyInit<u32> = LazyInit::new();
        VALUE.init_once(123);
        let v = unsafe { VALUE.get_unchecked() };
        assert_eq!(*v, 123);
    }

    #[test]
    fn lazyinit_get_mut_unchecked() {
        static mut VALUE: LazyInit<u32> = LazyInit::new();
        unsafe {
            VALUE.init_once(123);
        }
        let v = unsafe { VALUE.get_mut_unchecked() };
        *v += 3;
        assert_eq!(*v, 126);
    }
}
