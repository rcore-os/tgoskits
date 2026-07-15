use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    ops::Deref,
    sync::atomic::{AtomicU8, Ordering},
};

const UNINIT: u8 = 0;
const INITIALIZING: u8 = 1;
const READY: u8 = 2;

pub struct StaticCell<T> {
    state: AtomicU8,
    value: UnsafeCell<MaybeUninit<T>>,
}

// SAFETY: shared readers are admitted only after READY observes the release
// publication, and T is Sync. Initialization requires T: Send because the
// initializing CPU may differ from later readers.
unsafe impl<T: Send + Sync> Sync for StaticCell<T> {}
// SAFETY: moving exclusive ownership of a cell moves at most one T.
unsafe impl<T: Send> Send for StaticCell<T> {}

impl<T> StaticCell<T> {
    pub const fn uninit() -> Self {
        StaticCell {
            state: AtomicU8::new(UNINIT),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    pub const fn new(val: T) -> Self {
        StaticCell {
            state: AtomicU8::new(READY),
            value: UnsafeCell::new(MaybeUninit::new(val)),
        }
    }

    pub fn init(&self, val: T) {
        self.init_with_before_publish(val, || {});
    }

    fn init_with_before_publish(&self, val: T, before_publish: impl FnOnce()) {
        if self
            .state
            .compare_exchange(UNINIT, INITIALIZING, Ordering::Acquire, Ordering::Acquire)
            .is_err()
        {
            panic!(
                "LazyStatic {} already initialized",
                core::any::type_name::<T>()
            );
        }
        // SAFETY: this initializer exclusively owns the INITIALIZING state;
        // readers accept only READY and therefore cannot observe this write
        // until the release store below.
        unsafe { (*self.value.get()).as_mut_ptr().write(val) };
        before_publish();
        self.state.store(READY, Ordering::Release);
    }

    pub fn is_init(&self) -> bool {
        self.state.load(Ordering::Acquire) == READY
    }

    pub fn get_initialized(&self) -> Option<&T> {
        if !self.is_init() {
            return None;
        }
        // SAFETY: the acquire load observed the initializer's READY release,
        // which occurs only after the value has been fully written.
        Some(unsafe { &*(*self.value.get()).as_ptr() })
    }

    /// Initializes the value when no concurrent reader or initializer exists.
    ///
    /// # Safety
    ///
    /// The caller must guarantee exclusive single-CPU access until this
    /// function returns. No other CPU may read, initialize, or update the cell.
    pub unsafe fn init_single_core(&self, val: T) {
        if self.state.load(Ordering::Relaxed) != UNINIT {
            panic!(
                "LazyStatic {} already initialized",
                core::any::type_name::<T>()
            );
        }
        // SAFETY: the caller guarantees exclusive access and the state check
        // proves this storage has not previously been initialized.
        unsafe { (*self.value.get()).as_mut_ptr().write(val) };
        self.state.store(READY, Ordering::Relaxed);
    }

    /// Mutates an initialized value under caller-provided exclusion.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that the value is initialized and that no
    /// reader or other updater can access it for the duration of `f`.
    pub unsafe fn update<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        if !self.is_init() {
            panic!("LazyStatic {} not initialized", core::any::type_name::<T>());
        }
        // SAFETY: initialization was observed and the caller guarantees unique
        // access for the duration of the returned mutable borrow.
        let val = unsafe { &mut *(*self.value.get()).as_mut_ptr() };
        f(val)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    use super::StaticCell;

    #[test]
    fn initialization_is_not_published_before_the_value_is_written() {
        let cell = Arc::new(StaticCell::<usize>::uninit());
        let initializer_entered = Arc::new(Barrier::new(2));
        let allow_publish = Arc::new(Barrier::new(2));

        let initializer = {
            let cell = Arc::clone(&cell);
            let initializer_entered = Arc::clone(&initializer_entered);
            let allow_publish = Arc::clone(&allow_publish);
            thread::spawn(move || {
                cell.init_with_before_publish(42, || {
                    initializer_entered.wait();
                    allow_publish.wait();
                });
            })
        };

        initializer_entered.wait();
        assert!(
            !cell.is_init(),
            "an unpublished value must not be observable"
        );
        allow_publish.wait();
        initializer.join().unwrap();
        assert_eq!(**cell, 42);
    }

    #[test]
    fn initialized_lookup_does_not_shadow_inner_get_methods() {
        let cell: StaticCell<std::boxed::Box<[usize]>> =
            StaticCell::new(std::boxed::Box::from([1, 2, 3]));

        assert_eq!(cell.get(1), Some(&2));
        assert_eq!(cell.get_initialized().map(|values| values.len()), Some(3));
    }
}

impl<T> Deref for StaticCell<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.get_initialized()
            .unwrap_or_else(|| panic!("LazyStatic {} not initialized", core::any::type_name::<T>()))
    }
}
