//! CPU-local register publication and architecture dispatch.

use core::{
    fmt,
    marker::PhantomData,
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{
    CPU_AREA_RUNTIME_ANCHOR_OFFSET, CPU_LOCAL_ABI_VERSION, CpuAreaHeader, CpuAreaInitV2,
    CpuBindingV1, CpuPin, CpuRuntimeAnchor, CurrentThreadError, CurrentThreadHeader,
    image_register_mode,
};

#[cfg(all(not(feature = "host-test"), target_arch = "aarch64"))]
mod aarch64;
#[cfg(all(not(feature = "host-test"), target_arch = "arm"))]
mod arm;
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
#[cfg(all(not(feature = "host-test"), target_arch = "arm"))]
use arm as imp;
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

/// Installs the current CPU's final value-only register binding.
///
/// # Safety
///
/// `binding` must describe a fully initialized v2 CPU area that remains mapped
/// until shutdown. Local IRQs and traps must be disabled, the CPU must still be
/// offline, and no previous binding may exist on this physical CPU.
#[inline(always)]
pub unsafe fn install_binding(binding: CpuBindingV1) -> Result<(), CpuLocalError> {
    validate_binding(binding)?;
    let header = unsafe { &*(binding.area_base as *const CpuAreaHeader) };
    let init = CpuAreaInitV2::from_binding(binding).ok_or(CpuLocalError::InvalidBinding)?;
    header
        .validate_init(init)
        .map_err(|_| CpuLocalError::HeaderMismatch)?;
    imp::validate_arch_binding(binding)?;
    // SAFETY: validated mapped storage and the offline/trap-free publication
    // window are forwarded to the architecture implementation.
    unsafe { imp::install_current(binding) };
    if unsafe { imp::read_current_area_base() } != binding.area_base {
        fatal_register_invariant();
    }
    Ok(())
}

#[cold]
#[inline(never)]
fn fatal_register_invariant() -> ! {
    panic!("CPU-local register commit did not retain the validated binding")
}

/// Reads the unverified architecture-owned current-area address.
///
/// # Safety
///
/// `pin` must cover the read. In LinuxCurrent mode RISC-V obtains this value
/// through the pinned current header; a corrupt kernel TP is therefore a fatal
/// architecture invariant rather than an untrusted userspace value.
#[doc(hidden)]
#[inline(always)]
pub unsafe fn current_area_base_raw(_pin: &CpuPin) -> usize {
    unsafe { imp::read_current_area_base() }
}

/// Returns the current CPU-area base without dynamic validation.
///
/// # Safety
///
/// A valid CPU binding must be installed and migration must remain impossible
/// for the complete operation consuming the returned pointer.
#[inline(always)]
pub unsafe fn current_area_base_unchecked() -> NonNull<u8> {
    let area_base = unsafe { imp::read_current_area_base() };
    unsafe { NonNull::new_unchecked(area_base as *mut u8) }
}

/// Reads the mode-owned architecture task pointer without validation.
///
/// LinuxCurrent returns the current-thread header; UnikernelTls returns the
/// kernel TLS base. This raw primitive is reserved for the platform provider.
///
/// # Safety
///
/// A CPU binding must be installed and the caller must be pinned.
#[doc(hidden)]
pub unsafe fn get_task_pointer_raw() -> usize {
    unsafe { imp::get_task_pointer() }
}

/// Writes the mode-owned architecture task pointer.
///
/// # Safety
///
/// Only an offline bootstrap boundary or IRQ-disabled final switch tail may
/// call this with a value valid for the selected image mode.
#[doc(hidden)]
pub unsafe fn set_task_pointer_raw(value: usize) {
    unsafe { imp::set_task_pointer(value) }
}

/// Returns and validates the current CPU's frozen binding.
pub fn current_cpu_binding(pin: &CpuPin) -> Result<CpuBindingV1, CpuLocalError> {
    let area_base = unsafe { current_area_base_raw(pin) };
    if area_base == 0 {
        return Err(CpuLocalError::NotInitialized);
    }
    // SAFETY: an installed CPU register is the architecture trust root and its
    // area remains mapped until shutdown. Callers keep the CPU pinned.
    let binding = unsafe { (*(area_base as *const CpuAreaHeader)).binding() };
    validate_binding(binding)?;
    if binding.area_base != area_base {
        return Err(CpuLocalError::HeaderMismatch);
    }
    Ok(binding)
}

/// Returns the pinned current-thread header under a CPU pin.
pub fn current_thread(pin: &CpuPin) -> Result<NonNull<CurrentThreadHeader>, CpuLocalError> {
    let binding = current_cpu_binding(pin)?;
    let slot = unsafe { runtime_anchor(binding.area_base) }.current_thread_raw();
    let register = unsafe { imp::read_current_thread(binding.area_base) };
    if slot == 0 || slot != register || slot % core::mem::align_of::<CurrentThreadHeader>() != 0 {
        return Err(CpuLocalError::CurrentThreadMismatch);
    }
    let pointer = NonNull::new(slot as *mut CurrentThreadHeader)
        .ok_or(CpuLocalError::CurrentThreadMismatch)?;
    // SAFETY: the CPU slot may only publish pinned headers. The caller's pin
    // prevents the current slot from being replaced during validation.
    let thread_binding = unsafe { pointer.as_ref() }
        .cpu_binding()
        .ok_or(CpuLocalError::CurrentThreadMismatch)?;
    let cpu_index = binding.cpu_index().ok_or(CpuLocalError::InvalidBinding)?;
    if thread_binding.area_base() != binding.area_base || thread_binding.cpu_index() != cpu_index {
        return Err(CpuLocalError::CurrentThreadMismatch);
    }
    Ok(pointer)
}

/// Validated, CPU-pinned current-thread publication ready for one Release store.
#[must_use = "a prepared current-thread publication must be committed or discarded before switching"]
pub struct PreparedCurrentThreadPublish<'switch> {
    current_slot: &'static AtomicUsize,
    current_thread: usize,
    _lifetime: PhantomData<(&'switch CpuPin, &'switch CurrentThreadHeader)>,
    _not_send_or_sync: PhantomData<*mut ()>,
}

/// Validates the next current header before the irreversible switch tail.
///
/// # Safety
///
/// Only the IRQ-disabled scheduler path may prepare a publication. `header`
/// must remain pinned while current and the caller must retain `pin` through
/// the matching commit and raw context switch.
pub unsafe fn prepare_current_thread_publish<'switch>(
    pin: &'switch CpuPin,
    header: Pin<&'switch CurrentThreadHeader>,
) -> Result<PreparedCurrentThreadPublish<'switch>, CurrentThreadError> {
    let binding = current_cpu_binding(pin).map_err(|_| CurrentThreadError::InvalidCpuBinding)?;
    unsafe { prepare_current_thread_publish_for_binding(binding, pin, header) }
}

/// Validates a current header using a binding obtained from CpuLocalPlatformV1.
///
/// # Safety
///
/// `binding` must be the current CPU binding returned under an active pin. The
/// remaining scheduler serialization and lifetime requirements are identical
/// to [`prepare_current_thread_publish`].
pub unsafe fn prepare_current_thread_publish_for_binding<'switch>(
    binding: CpuBindingV1,
    _pin: &'switch CpuPin,
    header: Pin<&'switch CurrentThreadHeader>,
) -> Result<PreparedCurrentThreadPublish<'switch>, CurrentThreadError> {
    validate_binding(binding).map_err(|_| CurrentThreadError::InvalidCpuBinding)?;
    let expected_cpu = binding
        .cpu_index()
        .ok_or(CurrentThreadError::InvalidCpuBinding)?;
    let thread_binding = header
        .cpu_binding()
        .ok_or(CurrentThreadError::CpuBindingMismatch)?;
    if thread_binding.area_base() != binding.area_base || thread_binding.cpu_index() != expected_cpu
    {
        return Err(CurrentThreadError::CpuBindingMismatch);
    }
    Ok(PreparedCurrentThreadPublish {
        current_slot: unsafe { runtime_anchor(binding.area_base) }.current_thread_slot(),
        current_thread: header.as_non_null().as_ptr() as usize,
        _lifetime: PhantomData,
        _not_send_or_sync: PhantomData,
    })
}

/// Commits a prepared current-thread publication with one Release store.
///
/// This operation is infallible so the caller can place it immediately before
/// `TaskContext::switch_to_raw` without a post-publication result branch.
///
/// # Safety
///
/// The preparing scheduler serialization and CPU pin must still be active.
/// After this store, the caller must immediately enter the final raw context
/// switch; no fallible helper or ownership-sensitive Rust code may run.
#[inline(always)]
pub unsafe fn commit_current_thread_publish(prepared: PreparedCurrentThreadPublish<'_>) {
    prepared
        .current_slot
        .store(prepared.current_thread, Ordering::Release);
}

/// Returns the fixed runtime anchor for a known mapped CPU area.
///
/// # Safety
///
/// `area_base` must name a live initialized v2 CPU area, and the returned
/// reference must not outlive that shutdown-lifetime mapping.
pub unsafe fn runtime_anchor(area_base: usize) -> &'static CpuRuntimeAnchor {
    unsafe { &*((area_base + CPU_AREA_RUNTIME_ANCHOR_OFFSET) as *const CpuRuntimeAnchor) }
}

fn validate_binding(binding: CpuBindingV1) -> Result<(), CpuLocalError> {
    if binding.abi_version != CPU_LOCAL_ABI_VERSION
        || binding.register_mode() != Some(image_register_mode())
        || binding.host_level().is_none()
        || binding.cpu_index().is_none()
        || binding.area_base == 0
        || binding.boot_thread == 0
        || binding.cookie == 0
        || binding.generation == 0
    {
        return Err(CpuLocalError::InvalidBinding);
    }
    Ok(())
}

/// CPU-local register or publication failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuLocalError {
    /// No CPU binding is installed.
    NotInitialized,
    /// A scalar binding is malformed or selects another final-image mode.
    InvalidBinding,
    /// Frozen area header differs from the supplied binding.
    HeaderMismatch,
    /// The binding's host level differs from the live architecture level.
    HostLevelMismatch,
    /// CPU slot and architecture current-thread register disagree.
    CurrentThreadMismatch,
}

impl fmt::Display for CpuLocalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotInitialized => "CPU-local state is not initialized",
            Self::InvalidBinding => "CPU-local binding is invalid",
            Self::HeaderMismatch => "CPU-area header differs from its binding",
            Self::HostLevelMismatch => "CPU-local binding selects a different host level",
            Self::CurrentThreadMismatch => "current-thread register and CPU slot mismatch",
        })
    }
}

impl core::error::Error for CpuLocalError {}
