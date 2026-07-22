//! Typed CPU-local facade over the value-only static platform ABI.

use core::{pin::Pin, ptr::NonNull};

#[cfg(feature = "smp")]
pub use ax_plat::percpu::init_secondary;
pub use ax_plat::percpu::{
    init_primary, this_cpu_id, this_cpu_id_pinned, this_cpu_is_bsp, this_cpu_is_bsp_pinned,
};
pub use cpu_local::{
    ContextIdentity, CpuBindingEpoch, CpuBindingV1, CpuLocalError, CpuPin, CurrentThreadError,
    CurrentThreadHeader, PreparedCurrentThreadPublish, ThreadIdentity,
};
use cpu_local::{CpuLocalStatus, RegisterModeV1, image_register_mode};

/// Returns the direct current CPU-area base under an explicit pin.
pub fn cpu_base(_pin: &CpuPin) -> Result<NonNull<u8>, CpuLocalError> {
    let binding = platform_binding()?;
    NonNull::new(binding.area_base as *mut u8).ok_or(CpuLocalError::InvalidBinding)
}

/// Returns the validated value-only binding for the pinned current CPU.
pub fn current_cpu_binding(_pin: &CpuPin) -> Result<CpuBindingV1, CpuLocalError> {
    platform_binding()
}

/// Returns the pinned current execution-context header.
pub fn current_thread(_pin: &CpuPin) -> Result<NonNull<CurrentThreadHeader>, CpuLocalError> {
    let binding = platform_binding()?;
    let raw = cpu_local::platform::current_thread();
    let pointer = NonNull::new(raw as *mut CurrentThreadHeader)
        .filter(|pointer| {
            pointer
                .as_ptr()
                .align_offset(core::mem::align_of::<CurrentThreadHeader>())
                == 0
        })
        .ok_or(CpuLocalError::CurrentThreadMismatch)?;
    // SAFETY: CpuLocalPlatformV1 promises that a nonzero value names the pinned
    // header published by this CPU. The caller's CpuPin covers validation.
    let current_binding = unsafe { pointer.as_ref() }
        .cpu_binding()
        .ok_or(CpuLocalError::CurrentThreadMismatch)?;
    let cpu_index = binding.cpu_index().ok_or(CpuLocalError::InvalidBinding)?;
    if current_binding.area_base() != binding.area_base || current_binding.cpu_index() != cpu_index
    {
        return Err(CpuLocalError::CurrentThreadMismatch);
    }
    Ok(pointer)
}

/// Reads the published current-thread header without constructing a CPU pin.
///
/// This is the minimal compatibility entry used while the scheduler itself is
/// establishing or releasing the preemption guard that would normally provide
/// a [`CpuPin`]. It does not introduce another per-CPU source of truth: the
/// returned pointer is still read from the CPU runtime anchor.
///
/// # Safety
///
/// The caller must only dereference the result while the pointed-to task keeps
/// its scheduler-owned current reference alive. Code outside scheduler guard
/// bootstrap should use [`current_thread`] instead.
pub unsafe fn current_thread_raw() -> *const CurrentThreadHeader {
    cpu_local::platform::current_thread() as *const CurrentThreadHeader
}

/// Validates current-thread publication before the irreversible switch tail.
///
/// # Safety
///
/// Only the IRQ-disabled scheduler path may call this. The header must remain
/// pinned and CPU-bound through the subsequent commit and raw context switch.
pub unsafe fn prepare_current_thread_publish<'pin>(
    pin: &'pin CpuPin,
    header: Pin<&'pin CurrentThreadHeader>,
) -> Result<PreparedCurrentThreadPublish<'pin>, CurrentThreadError> {
    let binding = platform_binding().map_err(|_| CurrentThreadError::InvalidCpuBinding)?;
    unsafe { cpu_local::prepare_current_thread_publish_for_binding(binding, pin, header) }
}

/// Performs the infallible Release-store publication immediately before the
/// naked context switch.
///
/// # Safety
///
/// The scheduler serialization and CPU pin used during preparation must still
/// be active. No fallible Rust code may run after this call and before
/// `TaskContext::switch_to_raw`.
#[inline(always)]
pub unsafe fn commit_current_thread_publish(prepared: PreparedCurrentThreadPublish<'_>) {
    unsafe { cpu_local::commit_current_thread_publish(prepared) }
}

/// Installs the scheduler bootstrap header in LinuxCurrent mode.
///
/// # Safety
///
/// The CPU must remain offline with IRQs/traps excluded. `header` must already
/// be bound to this CPU and remain pinned until normal scheduler replacement.
#[cfg(not(feature = "tls"))]
pub unsafe fn install_bootstrap_current_thread(
    pin: &CpuPin,
    header: Pin<&CurrentThreadHeader>,
) -> Result<(), CurrentThreadError> {
    let pointer = header.as_non_null().as_ptr() as usize;
    let prepared = unsafe { prepare_current_thread_publish(pin, header) }?;
    unsafe { cpu_local::platform::set_tp(pointer) }
        .map_err(|_| CurrentThreadError::InvalidCpuBinding)?;
    unsafe { commit_current_thread_publish(prepared) };
    Ok(())
}

/// Reads the current task-owned kernel TLS base.
#[cfg(feature = "tls")]
pub fn kernel_tls() -> crate::context::KernelTlsBase {
    crate::context::KernelTlsBase::new(cpu_local::platform::get_tp())
}

/// Installs the bootstrap task's kernel TLS before scheduling starts.
///
/// # Safety
///
/// The CPU must still be offline or otherwise unable to schedule, and
/// `kernel_tls` must remain valid while the bootstrap context can execute.
#[cfg(feature = "tls")]
pub unsafe fn install_bootstrap_kernel_tls(
    kernel_tls: crate::context::KernelTlsBase,
) -> Result<(), CpuLocalError> {
    match unsafe { cpu_local::platform::set_tp(kernel_tls.as_usize()) } {
        Ok(()) => Ok(()),
        Err(CpuLocalStatus::NotInitialized) => Err(CpuLocalError::NotInitialized),
        Err(_) => Err(CpuLocalError::InvalidBinding),
    }
}

fn platform_binding() -> Result<CpuBindingV1, CpuLocalError> {
    let binding = match cpu_local::platform::current_cpu_binding() {
        Ok(binding) => binding,
        Err(CpuLocalStatus::NotInitialized) => {
            return Err(CpuLocalError::NotInitialized);
        }
        Err(_) => return Err(CpuLocalError::InvalidBinding),
    };
    if binding.register_mode() != Some(image_register_mode())
        || binding.register_mode() == Some(RegisterModeV1::UnikernelTls) && !cfg!(feature = "tls")
    {
        return Err(CpuLocalError::InvalidBinding);
    }
    Ok(binding)
}

/// Allocates and binds the single modeled CPU used by host-side scheduler tests.
#[cfg(feature = "host-test")]
pub fn initialize_host_test_cpu() {
    use core::num::NonZeroU32;

    let layout = ax_percpu::host_test::initialize(NonZeroU32::new(1).unwrap())
        .expect("host CPU-local layout must initialize");
    let area =
        ax_percpu::area(ax_percpu::CpuIndex::try_from(0).expect("CPU zero must be representable"))
            .expect("host CPU zero area must exist");
    debug_assert_eq!(layout.runtime_base, area.runtime_base());

    // SAFETY: the scheduler test worker models one non-migrating CPU and the
    // process-lifetime host fixture has completed typed initialization.
    let pin = unsafe { CpuPin::new_unchecked() };
    match cpu_local::raw::current_binding(&pin) {
        Ok(binding) => assert_eq!(binding, area.binding()),
        Err(CpuLocalError::NotInitialized) => {
            // SAFETY: this is the unique offline binding point for the host
            // scheduler worker and the area remains mapped until process exit.
            unsafe { cpu_local::raw::install_binding(area.binding()) }
                .expect("host CPU-local binding must install");
        }
        Err(error) => panic!("invalid host CPU-local binding: {error}"),
    }
}

#[cfg(feature = "host-test")]
struct HostCpuLocalPlatform;

#[cfg(feature = "host-test")]
#[cpu_local::abi::impl_extern_trait(name = "cpu-local_0_1", abi = "rust")]
impl cpu_local::CpuLocalPlatformV1 for HostCpuLocalPlatform {
    fn current_cpu_binding() -> cpu_local::CpuBindingResultV1 {
        // SAFETY: host-test callers remain on their modeled CPU thread.
        let pin = unsafe { CpuPin::new_unchecked() };
        match cpu_local::raw::current_binding(&pin) {
            Ok(binding) => cpu_local::CpuBindingResultV1::ok(binding),
            Err(CpuLocalError::NotInitialized) => {
                cpu_local::CpuBindingResultV1::error(cpu_local::CpuLocalStatus::NotInitialized)
            }
            Err(_) => {
                cpu_local::CpuBindingResultV1::error(cpu_local::CpuLocalStatus::InvalidBinding)
            }
        }
    }

    fn get_tp() -> usize {
        // SAFETY: the host fixture models the same pinning contract as a CPU.
        unsafe { cpu_local::raw::get_task_pointer() }
    }

    unsafe fn set_tp(value: usize) -> cpu_local::CpuLocalStatus {
        // SAFETY: forwarded trait contract owns the modeled task-pointer slot.
        unsafe { cpu_local::raw::set_task_pointer(value) };
        cpu_local::CpuLocalStatus::Ok
    }

    fn current_thread() -> usize {
        // SAFETY: the host fixture models the same pinning contract as a CPU.
        let pin = unsafe { CpuPin::new_unchecked() };
        cpu_local::raw::current_thread(&pin).map_or(0, |header| header.as_ptr() as usize)
    }
}
