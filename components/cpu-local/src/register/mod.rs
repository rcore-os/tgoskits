//! Architecture register primitives and shared validation.

use core::{pin::Pin, ptr::NonNull, sync::atomic::Ordering};

use crate::{CpuAreaRef, CpuLocalError, CpuPin, CurrentThreadHeader, ThreadSwitchError};

#[cfg(all(not(feature = "host-test"), target_arch = "aarch64"))]
mod aarch64;
#[cfg(feature = "host-test")]
mod host;
#[cfg(all(not(feature = "host-test"), target_arch = "loongarch64"))]
mod loongarch64;
#[cfg(all(
    not(feature = "host-test"),
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
mod riscv;
#[cfg(all(not(feature = "host-test"), target_arch = "x86_64"))]
mod x86_64;

#[cfg(all(not(feature = "host-test"), target_arch = "aarch64"))]
use aarch64 as imp;
#[cfg(feature = "host-test")]
use host as imp;
#[cfg(all(not(feature = "host-test"), target_arch = "loongarch64"))]
use loongarch64 as imp;
#[cfg(all(
    not(feature = "host-test"),
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
use riscv as imp;
#[cfg(all(not(feature = "host-test"), target_arch = "x86_64"))]
use x86_64 as imp;

#[cfg(all(
    not(feature = "host-test"),
    not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv32",
        target_arch = "riscv64",
        target_arch = "loongarch64"
    ))
))]
compile_error!("cpu-local supports x86_64, AArch64, RISC-V, and LoongArch64 only");

/// Installs the final area of an offline CPU.
///
/// # Safety
///
/// The area must remain mapped until shutdown. The CPU must be offline with
/// traps disabled, and no previous area may be installed on this physical CPU.
#[doc(hidden)]
pub unsafe fn install_cpu_area(area: CpuAreaRef) -> Result<(), CpuLocalError> {
    imp::validate_environment()?;
    let boot_thread = area.prefix().boot_thread().header();
    let boot_pointer = boot_thread as *const CurrentThreadHeader as usize;
    // SAFETY: the caller owns the offline register installation boundary.
    unsafe { imp::install_cpu_base(area.base(), boot_pointer) };
    if unsafe { imp::read_cpu_base()? } != area.base() {
        fatal_register_invariant();
    }
    Ok(())
}

pub(crate) fn current_area() -> Result<CpuAreaRef, CpuLocalError> {
    let area_base = unsafe { imp::read_cpu_base()? };
    if area_base == 0 {
        return Err(CpuLocalError::AreaNotInstalled);
    }
    // SAFETY: only install_cpu_area writes the architecture-owned base, and
    // its contract requires a shutdown-lifetime initialized area.
    unsafe { CpuAreaRef::from_initialized_base(area_base) }
}

/// Publishes the scheduler anchor before the architecture switch tail.
///
/// # Safety
///
/// The caller must own the final IRQ-disabled context-switch boundary. `value`
/// must identify the prepared pinned header and remain alive while current.
pub(crate) unsafe fn commit_current_thread(area: CpuAreaRef, value: usize) {
    area.runtime_anchor()
        .current_thread_slot()
        .store(value, Ordering::Release);
}

/// Returns the pinned current-thread header after checking both sources.
pub fn current_thread(pin: &CpuPin<'_>) -> Result<NonNull<CurrentThreadHeader>, CpuLocalError> {
    let area = pin.area();
    let slot = area.runtime_anchor().current_thread_raw();
    let register = unsafe { imp::read_current_thread(area.base()) };
    if slot == 0
        || slot != register
        || !slot.is_multiple_of(core::mem::align_of::<CurrentThreadHeader>())
    {
        return Err(CpuLocalError::CurrentThreadMismatch);
    }
    let pointer = NonNull::new(slot as *mut CurrentThreadHeader)
        .ok_or(CpuLocalError::CurrentThreadMismatch)?;
    // SAFETY: scheduler publication only accepts pinned headers that remain
    // alive while current, and the caller holds the required CPU pin.
    let thread_area = unsafe { pointer.as_ref() }
        .cpu_area()
        .ok_or(CpuLocalError::CurrentThreadMismatch)?;
    if thread_area != area {
        return Err(CpuLocalError::CurrentThreadMismatch);
    }
    Ok(pointer)
}

/// Reads the current header before the scheduler can construct its guard.
///
/// # Safety
///
/// The caller must keep the scheduler-owned current task alive and must not
/// dereference the result after a context switch.
#[doc(hidden)]
pub unsafe fn scheduler_current_thread() -> Result<NonNull<CurrentThreadHeader>, CpuLocalError> {
    #[cfg(not(feature = "tls"))]
    {
        // LinuxCurrent images keep the task pointer in one architecture-owned
        // source. Reading a CPU area first would race migration because this
        // function is itself used to construct the preemption guard.
        let register = unsafe { imp::read_current_thread(0) };
        NonNull::new(register as *mut CurrentThreadHeader)
            .ok_or(CpuLocalError::CurrentThreadMismatch)
    }

    #[cfg(feature = "tls")]
    loop {
        // UnikernelTls images keep current in the CPU area's runtime anchor.
        // Retry if migration changes the base between sampling the area and
        // loading its slot; the caller cannot be pinned before this lookup.
        let area = current_area()?;
        let register = unsafe { imp::read_current_thread(area.base()) };
        if unsafe { imp::read_cpu_base()? } != area.base() {
            continue;
        }
        return NonNull::new(register as *mut CurrentThreadHeader)
            .ok_or(CpuLocalError::CurrentThreadMismatch);
    }
}

#[cfg(all(test, feature = "host-test"))]
mod tests {
    use core::mem::MaybeUninit;

    use super::*;
    use crate::{CpuAreaPrefix, CpuIndex};

    fn modeled_area(cpu_index: usize) -> CpuAreaRef {
        let storage = Box::leak(Box::new(MaybeUninit::<CpuAreaPrefix>::uninit()));
        let base = storage.as_mut_ptr() as usize;
        storage.write(
            CpuAreaPrefix::initialize(CpuIndex::try_from(cpu_index).unwrap(), base).unwrap(),
        );
        // SAFETY: the initialized fixture is leaked for the process lifetime.
        unsafe { CpuAreaRef::from_initialized_base(base) }.unwrap()
    }

    #[test]
    fn scheduler_current_thread_survives_migration_during_bootstrap_read() {
        let first = modeled_area(0);
        let second = modeled_area(1);
        let first_boot = first.prefix().boot_thread().header();
        let second_boot = second.prefix().boot_thread().header();

        // SAFETY: this host thread serially owns both leaked CPU fixtures.
        unsafe { imp::install_cpu_base(first.base(), first_boot as *const _ as usize) };
        imp::migrate_on_next_current_read(second.base());

        assert_eq!(
            // SAFETY: both boot headers have process-lifetime storage.
            unsafe { scheduler_current_thread() },
            Ok(NonNull::from(second_boot)),
        );
    }
}

/// Binds and publishes the first scheduler task on an offline CPU.
///
/// # Safety
///
/// The CPU must remain offline and trap-free. `header` must stay pinned and
/// alive until the scheduler replaces it through a prepared switch.
#[doc(hidden)]
pub unsafe fn install_bootstrap_thread(
    pin: &CpuPin<'_>,
    header: Pin<&CurrentThreadHeader>,
) -> Result<(), ThreadSwitchError> {
    let epoch = unsafe { header.bind_cpu(pin.area()) }?;
    let pointer = header.as_non_null().as_ptr() as usize;
    unsafe { commit_current_thread(pin.area(), pointer) };
    // Bootstrap has no raw switch tail. Install the architecture-owned current
    // register directly while this CPU remains offline and trap-free.
    unsafe { imp::write_current_thread(pointer) };
    if current_thread(pin) != Ok(header.as_non_null()) {
        // The register is already committed, so continuing would make all
        // later Rust execution unsound. Rollback is intentionally impossible.
        let _ = epoch;
        fatal_register_invariant();
    }
    Ok(())
}

/// Reads task-owned kernel TLS under an explicit CPU pin.
#[cfg(feature = "tls")]
pub fn kernel_tls(_pin: &CpuPin<'_>) -> usize {
    unsafe { imp::read_kernel_tls() }
}

/// Installs task-owned kernel TLS at an offline bootstrap boundary.
///
/// # Safety
///
/// The caller must own the offline CPU or IRQ-disabled final task switch, and
/// `value` must remain a valid TLS base for the installed execution context.
#[cfg(feature = "tls")]
#[doc(hidden)]
pub unsafe fn install_kernel_tls(_pin: &CpuPin<'_>, value: usize) {
    unsafe { imp::write_kernel_tls(value) };
}

#[cold]
#[inline(never)]
fn fatal_register_invariant() -> ! {
    panic!("CPU-local register commit did not retain the validated state")
}
