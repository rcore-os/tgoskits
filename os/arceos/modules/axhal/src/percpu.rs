//! Typed facade for CPU-local area and scheduler publication capabilities.

use core::{pin::Pin, ptr::NonNull};

#[cfg(feature = "smp")]
pub use ax_plat::percpu::init_secondary;
pub use ax_plat::percpu::{
    init_primary, this_cpu_id, this_cpu_id_pinned, this_cpu_is_bsp, this_cpu_is_bsp_pinned,
};
pub use cpu_local::{
    CpuAreaRef, CpuLocalError, CpuPin, CurrentContext, CurrentThreadHeader, ExclusiveCpu,
    PreparedThreadSwitch, PreviousThreadBinding, ThreadSwitchError, with_cpu_pin,
    with_exclusive_cpu,
};

/// Returns the direct current CPU-area base under an explicit pin.
pub fn cpu_base(pin: &CpuPin<'_>) -> NonNull<u8> {
    // CpuPin construction already validated the non-null initialized area.
    unsafe { NonNull::new_unchecked(pin.area().base() as *mut u8) }
}

/// Returns the validated current CPU area.
pub const fn current_cpu_area(pin: &CpuPin<'_>) -> CpuAreaRef {
    pin.area()
}

/// Returns the pinned current execution-context header.
pub fn current_thread(pin: &CpuPin<'_>) -> Result<NonNull<CurrentThreadHeader>, CpuLocalError> {
    cpu_local::current_thread(pin)
}

/// Reads current-thread identity before constructing a scheduler guard.
///
/// # Safety
///
/// The caller must keep the scheduler-owned current task alive and must not
/// dereference the result after a context switch.
pub unsafe fn current_thread_raw() -> *const CurrentThreadHeader {
    unsafe { cpu_local::scheduler_current_thread() }
        .map_or(core::ptr::null(), |pointer| pointer.as_ptr().cast_const())
}

/// Prepares a complete current-thread switch transaction.
///
/// # Safety
///
/// The caller must own the IRQ-disabled scheduler path and keep both task
/// allocations pinned through the raw switch and incoming tail.
pub unsafe fn prepare_thread_switch<'switch>(
    pin: &'switch CpuPin<'_>,
    previous: Pin<&CurrentThreadHeader>,
    next: Pin<&CurrentThreadHeader>,
) -> Result<(PreparedThreadSwitch<'switch>, PreviousThreadBinding), ThreadSwitchError> {
    unsafe { cpu_local::prepare_thread_switch(pin, previous, next) }
}

/// Installs the scheduler bootstrap task on an offline CPU.
///
/// # Safety
///
/// The CPU must be offline and trap-free, and `header` must remain pinned.
pub unsafe fn install_bootstrap_thread(
    pin: &CpuPin<'_>,
    header: Pin<&CurrentThreadHeader>,
) -> Result<(), ThreadSwitchError> {
    unsafe { cpu_local::install_bootstrap_thread(pin, header) }
}

/// Reads the current task-owned kernel TLS base.
#[cfg(feature = "tls")]
pub fn kernel_tls(pin: &CpuPin<'_>) -> crate::context::KernelTlsBase {
    crate::context::KernelTlsBase::new(cpu_local::kernel_tls(pin))
}

/// Installs bootstrap task TLS before scheduling starts.
///
/// # Safety
///
/// The CPU must remain offline, and `kernel_tls` must remain valid while the
/// bootstrap context executes.
#[cfg(feature = "tls")]
pub unsafe fn install_bootstrap_kernel_tls(
    pin: &CpuPin<'_>,
    kernel_tls: crate::context::KernelTlsBase,
) {
    unsafe { cpu_local::install_kernel_tls(pin, kernel_tls.as_usize()) };
}

/// Allocates and installs CPU zero for host-side scheduler tests.
#[cfg(feature = "host-test")]
pub fn initialize_host_test_cpu() {
    use core::num::NonZeroU32;

    let layout = ax_percpu::host_test::initialize(NonZeroU32::new(1).unwrap())
        .expect("host per-CPU layout must initialize");
    let cpu_index = ax_percpu::CpuIndex::try_from(0).expect("CPU zero must be representable");
    let area = layout
        .area(cpu_index)
        .expect("host CPU zero area must exist");
    let cpu_area = area.cpu_area().expect("host CPU zero prefix must be valid");

    // SAFETY: the scheduler test worker models one offline, non-migrating CPU
    // and the process-lifetime fixture is fully initialized.
    match unsafe { cpu_local::install_cpu_area(cpu_area) } {
        Ok(()) => {}
        Err(CpuLocalError::AreaNotInstalled) => unreachable!(),
        Err(error) => {
            // Repeated host initialization is accepted when this thread
            // already has the same area installed.
            let current = unsafe { cpu_local::with_cpu_pin(|pin| pin.area()) };
            assert_eq!(
                current,
                Ok(cpu_area),
                "invalid host CPU-local state: {error}"
            );
        }
    }
}
