use core::{marker::PhantomData, ptr::NonNull};

use cpu_local::{CpuPin, ExclusiveCpu};

use crate::PerCpuArea;

/// Provider generated for one concrete symbol in the per-CPU template.
///
/// # Safety
///
/// Every returned pointer must address the declared `T` in the selected live
/// area. Primitive providers must use the matching atomic representation.
#[doc(hidden)]
pub unsafe trait PerCpuSymbol<T> {
    fn offset() -> usize;
    fn current_ptr(pin: &CpuPin<'_>) -> NonNull<T>;
    fn remote_ptr(area: PerCpuArea) -> NonNull<T>;
}

/// Marker implemented only by macro-generated object symbols.
///
/// # Safety
///
/// The provider's storage must contain a live `T` in every initialized area.
#[doc(hidden)]
pub unsafe trait PerCpuObjectSymbol<T>: PerCpuSymbol<T> {}

/// Marker implemented only by macro-generated atomic scalar symbols.
///
/// # Safety
///
/// The provider's storage must use the atomic representation paired with `T`.
#[doc(hidden)]
pub unsafe trait PerCpuPrimitiveSymbol<T>: PerCpuSymbol<T> {}

type PerCpuMarker<T, S> = fn() -> (T, S);

/// Typed descriptor for one symbol replicated in every runtime CPU area.
pub struct PerCpu<T, S> {
    _marker: PhantomData<PerCpuMarker<T, S>>,
}

impl<T, S> PerCpu<T, S>
where
    S: PerCpuSymbol<T>,
{
    /// Creates the zero-sized descriptor for a macro-generated symbol.
    #[doc(hidden)]
    pub const fn new() -> Self {
        Self {
            _marker: PhantomData,
        }
    }

    /// Returns this symbol's byte offset in one area.
    pub fn offset(&self) -> usize {
        S::offset()
    }

    /// Returns a typed pointer whose address is stable for `pin`.
    pub fn current_ptr(&self, pin: &CpuPin<'_>) -> NonNull<T> {
        S::current_ptr(pin)
    }

    /// Returns a typed pointer in an explicitly selected remote area.
    ///
    /// The caller remains responsible for synchronization before dereference.
    pub fn remote_ptr(&self, area: PerCpuArea) -> NonNull<T> {
        S::remote_ptr(area)
    }
}

impl<T, S> PerCpu<T, S>
where
    S: PerCpuObjectSymbol<T>,
{
    /// Mutates the current CPU's object without allowing its borrow to escape.
    pub fn with_current_mut<R>(
        &self,
        exclusive: &ExclusiveCpu<'_>,
        operation: impl for<'value> FnOnce(&'value mut T) -> R,
    ) -> R {
        // SAFETY: ExclusiveCpu proves local and remote alias exclusion for the
        // closure, while its area fixes the generated address.
        let mut pointer =
            unsafe { NonNull::new_unchecked((exclusive.area().base() + S::offset()) as *mut T) };
        operation(unsafe { pointer.as_mut() })
    }
}

impl<T, S> PerCpu<T, S>
where
    T: Sync,
    S: PerCpuObjectSymbol<T>,
{
    /// Borrows a shared current-CPU object for one non-escaping callback.
    pub fn with_current<R>(
        &self,
        pin: &CpuPin<'_>,
        operation: impl for<'value> FnOnce(&'value T) -> R,
    ) -> R {
        // SAFETY: T: Sync permits shared observation and the pin fixes address.
        operation(unsafe { S::current_ptr(pin).as_ref() })
    }
}

mod primitive {
    use core::{
        ptr::NonNull,
        sync::atomic::{
            AtomicBool, AtomicU8, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering,
        },
    };

    pub trait Sealed: Copy {
        unsafe fn load(pointer: NonNull<Self>) -> Self;
        unsafe fn store(pointer: NonNull<Self>, value: Self);
    }

    macro_rules! impl_atomic_primitive {
        ($value:ty, $atomic:ty) => {
            impl Sealed for $value {
                unsafe fn load(pointer: NonNull<Self>) -> Self {
                    unsafe { pointer.cast::<$atomic>().as_ref() }.load(Ordering::Relaxed)
                }

                unsafe fn store(pointer: NonNull<Self>, value: Self) {
                    unsafe { pointer.cast::<$atomic>().as_ref() }.store(value, Ordering::Relaxed);
                }
            }
        };
    }

    impl_atomic_primitive!(bool, AtomicBool);
    impl_atomic_primitive!(u8, AtomicU8);
    impl_atomic_primitive!(u16, AtomicU16);
    impl_atomic_primitive!(u32, AtomicU32);
    impl_atomic_primitive!(u64, AtomicU64);
    impl_atomic_primitive!(usize, AtomicUsize);
}

impl<T, S> PerCpu<T, S>
where
    T: primitive::Sealed,
    S: PerCpuPrimitiveSymbol<T>,
{
    /// Loads the current CPU's atomic scalar with relaxed ordering.
    pub fn read_current(&self, pin: &CpuPin<'_>) -> T {
        unsafe { T::load(S::current_ptr(pin)) }
    }

    /// Stores the current CPU's atomic scalar with relaxed ordering.
    pub fn write_current(&self, pin: &CpuPin<'_>, value: T) {
        unsafe { T::store(S::current_ptr(pin), value) }
    }
}
