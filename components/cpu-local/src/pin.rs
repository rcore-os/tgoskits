use core::{marker::PhantomData, ptr::NonNull};

use crate::{CpuAreaRef, CpuLocalError, register};

/// Scoped proof that execution cannot migrate away from one validated CPU.
///
/// The token can only be created by [`with_cpu_pin`]. Its invariant lifetime
/// and higher-ranked callback prevent it from escaping the caller's migration
/// guard or offline-CPU critical section.
#[must_use = "CPU-local access is valid only while this pin remains in scope"]
#[derive(Debug)]
pub struct CpuPin<'scope> {
    area: CpuAreaRef,
    _scope: PhantomData<&'scope mut &'scope ()>,
    _not_send_or_sync: PhantomData<*mut ()>,
}

impl CpuPin<'_> {
    /// Returns the initialized CPU area validated when this pin was created.
    pub const fn area(&self) -> CpuAreaRef {
        self.area
    }
}

/// Scoped proof of exclusive local access to CPU-owned mutable state.
///
/// In addition to migration exclusion, the caller that creates this token has
/// excluded local IRQ/re-entry and every conflicting remote access.
#[must_use = "mutable CPU-local access is valid only while this token remains in scope"]
#[derive(Debug)]
pub struct ExclusiveCpu<'pin> {
    area: CpuAreaRef,
    _scope: PhantomData<&'pin mut &'pin ()>,
    _not_send_or_sync: PhantomData<*mut ()>,
}

/// Scoped selection of the architecture-owned current CPU area.
///
/// This capability is intentionally weaker than [`CpuPin`]: it does not
/// validate current execution-context publication. It exists for low-level
/// owner boundaries that must select CPU-owned state before a pin can be
/// constructed.
#[doc(hidden)]
#[must_use = "current CPU-area access is valid only while this token remains in scope"]
#[derive(Debug)]
pub struct CurrentCpuArea<'scope> {
    area_base: usize,
    _scope: PhantomData<&'scope mut &'scope ()>,
    _not_send_or_sync: PhantomData<*mut ()>,
}

impl CurrentCpuArea<'_> {
    /// Calculates a typed symbol address in the selected installed CPU area.
    ///
    /// # Errors
    ///
    /// Returns [`CpuLocalError::AddressOverflow`] when adding `offset` exceeds
    /// the address space.
    ///
    /// # Safety
    ///
    /// `offset` must identify a live, properly aligned `T` in every initialized
    /// CPU area. The returned pointer may only be dereferenced while the outer
    /// owner transaction retains the synchronization required by `T`.
    #[doc(hidden)]
    pub unsafe fn symbol_ptr<T>(&self, offset: usize) -> Result<NonNull<T>, CpuLocalError> {
        let address = self
            .area_base
            .checked_add(offset)
            .ok_or(CpuLocalError::AddressOverflow)?;
        NonNull::new(address as *mut T).ok_or(CpuLocalError::InvalidAreaBase { base: address })
    }
}

impl ExclusiveCpu<'_> {
    /// Returns the initialized area covered by this stronger capability.
    pub const fn area(&self) -> CpuAreaRef {
        self.area
    }
}

/// Runs `operation` with a validated, non-escaping CPU pin.
///
/// The higher-ranked callback prevents retaining the token:
///
/// ```compile_fail
/// let retained = unsafe { cpu_local::with_cpu_pin(|pin| pin) }.unwrap();
/// # let _ = retained;
/// ```
///
/// It also cannot be sent to another execution context:
///
/// ```compile_fail
/// unsafe {
///     cpu_local::with_cpu_pin(|pin| {
///         std::thread::scope(|scope| scope.spawn(|| drop(pin)));
///     })
///     .unwrap();
/// }
/// ```
///
/// # Errors
///
/// Returns [`CpuLocalError::AreaNotInstalled`] before this CPU has installed
/// its runtime area, or an identity error if the live register and area header
/// disagree.
///
/// # Safety
///
/// The caller must prevent migration for the complete callback. Offline boot
/// code may call this while the CPU cannot be scheduled; runtime code must
/// hold an appropriate preemption or IRQ guard.
pub unsafe fn with_cpu_pin<R>(
    operation: impl for<'scope> FnOnce(&CpuPin<'scope>) -> R,
) -> Result<R, CpuLocalError> {
    let area = register::current_area()?;
    let pin = CpuPin {
        area,
        _scope: PhantomData,
        _not_send_or_sync: PhantomData,
    };
    // Validate the image's selected current-context source before exposing
    // typed access. Each image mode has exactly one authoritative source.
    register::current_context(&pin)?;
    Ok(operation(&pin))
}

/// Runs `operation` with exclusive access to mutable state on the pinned CPU.
///
/// # Safety
///
/// The caller must prevent migration, local IRQ/re-entry, and conflicting
/// remote access for the complete callback. `pin` must be covered by the same
/// guard that establishes those conditions.
pub unsafe fn with_exclusive_cpu<R>(
    pin: &CpuPin<'_>,
    operation: impl for<'exclusive> FnOnce(&ExclusiveCpu<'exclusive>) -> R,
) -> R {
    let exclusive = ExclusiveCpu {
        area: pin.area,
        _scope: PhantomData,
        _not_send_or_sync: PhantomData,
    };
    operation(&exclusive)
}

/// Runs `operation` with a non-escaping selection of the current CPU area.
///
/// Unlike [`with_cpu_pin`], this boundary does not validate current
/// execution-context publication or reconstruct the complete area identity.
/// It is intended for low-level owner code that cannot construct a pin before
/// accessing CPU-owned state.
///
/// # Errors
///
/// Returns [`CpuLocalError::AreaNotInstalled`] before the current CPU has an
/// installed runtime area, or an address error for an invalid base.
///
/// # Safety
///
/// The caller must prevent migration and context switches for the complete
/// callback. The installed area must remain mapped until shutdown. Values
/// mutably selected through this token additionally require local IRQ/re-entry
/// and every conflicting remote access to be excluded. Offline CPU bootstrap
/// satisfies these conditions before interrupt publication.
#[doc(hidden)]
pub unsafe fn with_current_cpu_area<R>(
    operation: impl for<'scope> FnOnce(&CurrentCpuArea<'scope>) -> R,
) -> Result<R, CpuLocalError> {
    let area_base = unsafe { register::current_cpu_area_base()? };
    let area = CurrentCpuArea {
        area_base,
        _scope: PhantomData,
        _not_send_or_sync: PhantomData,
    };
    Ok(operation(&area))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_cpu_area_rejects_a_symbol_address_overflow() {
        let area = CurrentCpuArea {
            area_base: usize::MAX,
            _scope: PhantomData,
            _not_send_or_sync: PhantomData,
        };

        assert_eq!(
            // SAFETY: no pointer is dereferenced; the test exercises rejection
            // before an address can be constructed.
            unsafe { area.symbol_ptr::<u8>(1) },
            Err(CpuLocalError::AddressOverflow),
        );
    }
}
