use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    ops::Deref,
    sync::atomic::{AtomicBool, Ordering},
};

/// A statically allocated value initialized by the boot CPU before SMP starts.
///
/// `StaticCell` models one specific phase transition: the boot CPU owns the
/// value while the other CPUs are offline, then publishes it for immutable
/// access by all CPUs. It deliberately does not provide runtime mutation or
/// concurrent lazy initialization. Use a lock or a concurrent once primitive
/// when initialization can race with another CPU.
///
/// The contained value must itself be safe to share immutably. In particular,
/// a non-[`Sync`] value does not become shareable merely because it is stored
/// in a `StaticCell`:
///
/// ```compile_fail
/// use core::cell::Cell;
/// use kernutil::StaticCell;
///
/// static INVALID: StaticCell<Cell<u32>> = StaticCell::new(Cell::new(0));
/// ```
pub struct StaticCell<T> {
    initialized: AtomicBool,
    value: UnsafeCell<MaybeUninit<T>>,
}

// SAFETY: initialization publishes a fully written value with Release
// ordering. All safe access observes that publication with Acquire ordering
// and returns only `&T`, which may cross CPUs exactly when `T: Sync`.
unsafe impl<T: Send + Sync> Sync for StaticCell<T> {}
// SAFETY: exclusive ownership of the cell permits moving its contained value
// exactly when the value itself may move between threads.
unsafe impl<T: Send> Send for StaticCell<T> {}

impl<T> StaticCell<T> {
    /// Creates an uninitialized boot cell.
    pub const fn uninit() -> Self {
        Self {
            initialized: AtomicBool::new(false),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Creates a boot cell whose value is initialized in the static image.
    pub const fn new(val: T) -> Self {
        Self {
            initialized: AtomicBool::new(true),
            value: UnsafeCell::new(MaybeUninit::new(val)),
        }
    }

    /// Initializes and publishes the value before secondary CPUs are released.
    ///
    /// This path uses only an ordinary write followed by a Release store. It
    /// therefore remains usable in architecture boot phases where exclusive
    /// atomic instructions are not yet available.
    ///
    /// # Safety
    ///
    /// The caller must be the only CPU or thread that can initialize this cell,
    /// and must call this method before any secondary CPU or concurrent thread
    /// can attempt initialization. After this method returns, the stored value
    /// may only be accessed through immutable references; any interior
    /// mutability in `T` must provide its own synchronization.
    ///
    /// # Panics
    ///
    /// Panics if the cell was already initialized.
    pub unsafe fn init_before_smp(&self, value: T) -> &T {
        if self.initialized.load(Ordering::Relaxed) {
            panic!(
                "StaticCell {} is already initialized",
                core::any::type_name::<T>()
            );
        }
        // SAFETY: the caller guarantees exclusive boot-time initialization.
        unsafe { (*self.value.get()).as_mut_ptr().write(value) };
        self.initialized.store(true, Ordering::Release);
        // SAFETY: the value was written before the Release publication above.
        unsafe { (*self.value.get()).assume_init_ref() }
    }

    /// Returns whether boot-time publication has completed.
    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    /// Returns the published immutable value.
    pub fn get(&self) -> Option<&T> {
        if self.is_initialized() {
            // SAFETY: the Acquire load observed either static initialization or
            // the Release store after the boot CPU wrote the complete value.
            Some(unsafe { (*self.value.get()).assume_init_ref() })
        } else {
            None
        }
    }
}

impl<T> Deref for StaticCell<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.get().unwrap_or_else(|| {
            panic!(
                "StaticCell {} is not initialized",
                core::any::type_name::<T>()
            )
        })
    }
}

impl<T> Drop for StaticCell<T> {
    fn drop(&mut self) {
        if *self.initialized.get_mut() {
            // SAFETY: `&mut self` excludes all references, and the state says
            // the slot contains a fully initialized value.
            unsafe { self.value.get_mut().assume_init_drop() };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::StaticCell;

    #[test]
    fn publication_precedes_cross_thread_immutable_access() {
        let value: StaticCell<[u32; 4]> = StaticCell::uninit();
        // SAFETY: the test initializes the cell on this thread before spawning
        // any reader, matching the boot-CPU-before-SMP contract.
        unsafe { value.init_before_smp([2, 3, 5, 7]) };

        thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| assert_eq!(value.get(), Some(&[2, 3, 5, 7])));
            }
        });
    }
}
