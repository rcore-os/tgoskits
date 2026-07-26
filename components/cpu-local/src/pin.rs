use core::marker::PhantomData;

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
    // Validate the second architecture-owned source before exposing any
    // typed access. This catches a restored CPU base paired with a stale task
    // register (notably after a vCPU exit) at the pin boundary.
    register::current_thread(&pin)?;
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
