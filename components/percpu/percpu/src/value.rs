use core::marker::PhantomData;

use crate::{BoundCpuPin, CpuIndex, CpuPin, PerCpuError};

/// Provider generated for one concrete symbol in the per-CPU template.
///
/// # Safety
///
/// Every returned pointer must address the declared `T` in the selected CPU's
/// live area. Implementations must not turn a [`BoundCpuPin`] into a stronger
/// aliasing or IRQ-exclusion guarantee. Providers used with
/// [`PrimitiveAccess`] must address the corresponding `core::sync::atomic`
/// representation, including its alignment requirement.
#[doc(hidden)]
pub unsafe trait PerCpuSymbol<T> {
    fn symbol_vma() -> usize;
    fn offset() -> usize;
    fn current_ptr(pin: &BoundCpuPin<'_>) -> *const T;
    unsafe fn current_ptr_unchecked() -> *const T;
    fn remote_ptr(cpu_index: CpuIndex) -> Result<*const T, PerCpuError>;
}

/// Marker selecting reference-based access for a non-primitive CPU-local
/// object.
#[doc(hidden)]
pub enum ObjectAccess {}

/// Marker selecting instantaneous value access for a primitive CPU-local
/// object.
#[doc(hidden)]
pub enum PrimitiveAccess {}

type PerCpuMarker<T, S, A> = fn() -> (T, S, A);

/// Typed descriptor for one symbol replicated in every CPU-local area.
///
/// `S` is a macro-generated symbol provider and `A` selects the safe access
/// surface. The descriptor owns no runtime value and therefore imposes no
/// `Send` or `Sync` bound on the CPU-owned `T`.
pub struct PerCpu<T, S, A = ObjectAccess> {
    _marker: PhantomData<PerCpuMarker<T, S, A>>,
}

impl<T, S, A> PerCpu<T, S, A>
where
    S: PerCpuSymbol<T>,
{
    /// Creates the zero-sized descriptor for its macro-generated symbol.
    #[doc(hidden)]
    pub const fn new() -> Self {
        Self {
            _marker: PhantomData,
        }
    }

    /// Returns this symbol's link-time virtual address.
    #[inline]
    pub fn symbol_vma(&self) -> usize {
        S::symbol_vma()
    }

    /// Returns this symbol's byte offset in one CPU-local area.
    #[inline]
    pub fn offset(&self) -> usize {
        S::offset()
    }

    /// Returns a raw pointer whose address remains stable while `pin` lives.
    #[inline]
    pub fn current_ptr(&self, pin: &BoundCpuPin<'_>) -> *const T {
        S::current_ptr(pin)
    }

    /// Returns a current-CPU pointer under a caller-provided pinning invariant.
    ///
    /// # Safety
    ///
    /// The current execution context must remain on one CPU until the pointer
    /// is no longer used.
    #[inline]
    pub unsafe fn current_ptr_unchecked(&self) -> *const T {
        // SAFETY: forwarded caller contract matches the symbol provider.
        unsafe { S::current_ptr_unchecked() }
    }

    /// Returns a current-CPU shared reference under raw synchronization rules.
    ///
    /// # Safety
    ///
    /// The caller must prevent migration and all conflicting mutation for the
    /// returned borrow. Prefer `with_current_ref` when that API is available.
    #[inline]
    pub unsafe fn current_ref_raw(&self) -> &T {
        // SAFETY: forwarded caller contract covers pointer validity and aliasing.
        unsafe { &*self.current_ptr_unchecked() }
    }

    /// Returns a current-CPU mutable reference under raw synchronization rules.
    ///
    /// # Safety
    ///
    /// The caller must prevent migration, IRQ re-entry, and every local or
    /// remote alias for the complete borrow.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn current_ref_mut_raw(&self) -> &mut T {
        // SAFETY: forwarded caller contract covers validity and exclusivity.
        unsafe { &mut *(self.current_ptr_unchecked() as *mut T) }
    }

    /// Mutates the current CPU's value without allowing its borrow to escape.
    ///
    /// # Safety
    ///
    /// [`CpuPin`] proves only address stability. The caller must additionally
    /// guarantee exclusive access, including against nested calls, hard IRQs,
    /// and remote CPU access, for the complete closure.
    pub unsafe fn with_current_mut_raw<R>(
        &self,
        pin: &CpuPin,
        operation: impl for<'value> FnOnce(&'value mut T) -> R,
    ) -> R {
        let _ = pin;
        // SAFETY: forwarded caller contract establishes both the current-area
        // binding and the unique borrow for this unchecked compatibility path.
        operation(unsafe { &mut *(self.current_ptr_unchecked() as *mut T) })
    }

    /// Returns a pointer to another installed CPU's instance.
    #[inline]
    pub fn remote_ptr(&self, cpu_index: CpuIndex) -> Result<*const T, PerCpuError> {
        S::remote_ptr(cpu_index)
    }

    /// Returns another CPU's shared reference.
    ///
    /// # Safety
    ///
    /// The caller must keep the remote area live and prevent conflicting
    /// mutation for the returned borrow.
    #[inline]
    pub unsafe fn remote_ref_raw(&self, cpu_index: CpuIndex) -> Result<&T, PerCpuError> {
        // SAFETY: forwarded caller contract covers the remote borrow.
        Ok(unsafe { &*self.remote_ptr(cpu_index)? })
    }

    /// Returns another CPU's mutable reference.
    ///
    /// # Safety
    ///
    /// The caller must provide exclusive access to the remote instance for the
    /// returned borrow.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn remote_ref_mut_raw(&self, cpu_index: CpuIndex) -> Result<&mut T, PerCpuError> {
        // SAFETY: forwarded caller contract covers the remote exclusive borrow.
        Ok(unsafe { &mut *(self.remote_ptr(cpu_index)? as *mut T) })
    }
}

impl<T, S> PerCpu<T, S, ObjectAccess>
where
    T: Sync,
    S: PerCpuSymbol<T>,
{
    /// Borrows a shared current-CPU value without allowing it to escape.
    ///
    /// This method is intentionally unavailable for owner-only `!Sync` values
    /// and for primitive descriptors that also expose safe writes.
    pub fn with_current_ref<R>(
        &self,
        pin: &BoundCpuPin<'_>,
        operation: impl for<'value> FnOnce(&'value T) -> R,
    ) -> R {
        // SAFETY: T: Sync permits shared observation, CpuPin fixes the address,
        // and no safe mutable API exists for ObjectAccess.
        operation(unsafe { &*self.current_ptr(pin) })
    }
}

mod primitive {
    use core::sync::atomic::{
        AtomicBool, AtomicU8, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering,
    };

    pub trait Sealed: Copy {
        /// Loads from the macro-generated atomic representation.
        ///
        /// # Safety
        ///
        /// `pointer` must be aligned for and point to the matching atomic type.
        unsafe fn load(pointer: *const Self) -> Self;

        /// Stores into the macro-generated atomic representation.
        ///
        /// # Safety
        ///
        /// `pointer` must be aligned for and point to the matching atomic type.
        unsafe fn store(pointer: *mut Self, value: Self);
    }

    macro_rules! impl_atomic_primitive {
        ($value:ty, $atomic:ty) => {
            impl Sealed for $value {
                #[inline]
                unsafe fn load(pointer: *const Self) -> Self {
                    // SAFETY: the macro stores this primitive as the matching
                    // atomic type and the provider preserves that address.
                    unsafe { &*pointer.cast::<$atomic>() }.load(Ordering::Relaxed)
                }

                #[inline]
                unsafe fn store(pointer: *mut Self, value: Self) {
                    // SAFETY: the macro stores this primitive as the matching
                    // atomic type and the provider preserves that address.
                    unsafe { &*pointer.cast::<$atomic>() }.store(value, Ordering::Relaxed);
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

impl<T, S> PerCpu<T, S, PrimitiveAccess>
where
    T: primitive::Sealed,
    S: PerCpuSymbol<T>,
{
    /// Copies the primitive value on the CPU proven by `pin`.
    ///
    /// This is a relaxed atomic load so hard-IRQ re-entry cannot create a Rust
    /// data race. It intentionally provides no inter-variable ordering.
    ///
    /// ```compile_fail
    /// use ax_percpu::def_percpu;
    ///
    /// #[def_percpu]
    /// static VALUE: usize = 0;
    ///
    /// // SAFETY: this only proves that the execution context cannot migrate.
    /// let migration_pin = unsafe { ax_percpu::CpuPin::new_unchecked() };
    /// let _ = VALUE.read_current(&migration_pin);
    /// ```
    #[inline]
    pub fn read_current(&self, pin: &BoundCpuPin<'_>) -> T {
        // SAFETY: PrimitiveAccess providers use the corresponding atomic
        // representation and CpuPin keeps the address on one CPU.
        unsafe { T::load(self.current_ptr(pin)) }
    }

    /// Replaces the primitive value on the CPU proven by `pin`.
    ///
    /// This is a relaxed atomic store so hard-IRQ re-entry cannot create a Rust
    /// data race. It intentionally provides no inter-variable ordering.
    #[inline]
    pub fn write_current(&self, pin: &BoundCpuPin<'_>, value: T) {
        // SAFETY: PrimitiveAccess providers use the corresponding atomic
        // representation and no safe reference API exists for this class.
        unsafe { T::store(self.current_ptr(pin) as *mut T, value) }
    }

    /// Copies the primitive value under a raw pinning contract.
    ///
    /// # Safety
    ///
    /// The caller must prevent migration and conflicting access.
    #[inline]
    pub unsafe fn read_current_raw(&self) -> T {
        // SAFETY: the provider uses the matching atomic representation and the
        // forwarded caller contract covers address stability.
        unsafe { T::load(self.current_ptr_unchecked()) }
    }

    /// Replaces the primitive value under a raw pinning contract.
    ///
    /// # Safety
    ///
    /// The caller must prevent migration and conflicting access.
    #[inline]
    pub unsafe fn write_current_raw(&self, value: T) {
        // SAFETY: the provider uses the matching atomic representation and the
        // forwarded caller contract covers address stability.
        unsafe { T::store(self.current_ptr_unchecked() as *mut T, value) }
    }
}
