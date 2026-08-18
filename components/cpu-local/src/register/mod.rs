//! Architecture register primitives and shared validation.

use core::{pin::Pin, ptr::NonNull, sync::atomic::Ordering};

use crate::{ContextSwitchError, CpuAreaRef, CpuLocalError, CpuPin, ExecutionContextHeader};

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

#[derive(Clone, Copy, Debug)]
pub(super) struct ArchitectureCurrentModel {
    pub(super) linux_current: CurrentContextSource,
    pub(super) unikernel_tls: CurrentContextSource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CurrentContextSource {
    #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
    ArchitectureRegister,
    #[cfg(not(all(target_arch = "aarch64", not(feature = "host-test"))))]
    RuntimeAnchor,
}

impl ArchitectureCurrentModel {
    const fn current_context_source(self, tls_enabled: bool) -> CurrentContextSource {
        if tls_enabled {
            self.unikernel_tls
        } else {
            self.linux_current
        }
    }
}

/// Installs the final area of an offline CPU.
///
/// # Safety
///
/// The area must remain mapped until shutdown. The CPU must be offline with
/// traps disabled, and no previous area may be installed on this physical CPU.
#[doc(hidden)]
pub unsafe fn install_cpu_area(area: CpuAreaRef) -> Result<(), CpuLocalError> {
    imp::validate_environment()?;
    let boot_context = area.prefix().boot_context().header();
    let boot_pointer = boot_context as *const ExecutionContextHeader as usize;
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

/// Reads the architecture CPU-area base without validating current context.
///
/// # Safety
///
/// The caller must prevent migration and context switches while using the
/// selected CPU. The installed area must remain mapped until shutdown.
#[inline(always)]
pub(crate) unsafe fn current_cpu_area_base() -> Result<usize, CpuLocalError> {
    let area_base = unsafe { imp::read_cpu_base()? };
    if area_base == 0 {
        return Err(CpuLocalError::AreaNotInstalled);
    }
    if !area_base.is_multiple_of(core::mem::align_of::<crate::CpuAreaPrefix>()) {
        return Err(CpuLocalError::InvalidAreaBase { base: area_base });
    }
    Ok(area_base)
}

/// Commits the current-context source before the architecture switch tail.
///
/// # Safety
///
/// The caller must own the final IRQ-disabled context-switch boundary. `value`
/// must identify the prepared pinned header and remain alive while current.
pub(crate) unsafe fn commit_current_context(_area: CpuAreaRef, _value: usize) {
    match imp::CURRENT_MODEL.current_context_source(cfg!(feature = "tls")) {
        #[cfg(not(all(target_arch = "aarch64", not(feature = "host-test"))))]
        CurrentContextSource::RuntimeAnchor => _area
            .runtime_anchor()
            .current_context_slot()
            .store(_value, Ordering::Release),
        #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
        CurrentContextSource::ArchitectureRegister => {
            core::sync::atomic::compiler_fence(Ordering::Release);
            #[cfg(feature = "host-test")]
            unsafe {
                imp::write_current_context(_value)
            };
        }
    }
}

/// Returns the pinned header selected by this image's sole current source.
pub fn current_context(pin: &CpuPin<'_>) -> Result<NonNull<ExecutionContextHeader>, CpuLocalError> {
    let area = pin.area();
    let raw = match imp::CURRENT_MODEL.current_context_source(cfg!(feature = "tls")) {
        #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
        CurrentContextSource::ArchitectureRegister => unsafe {
            imp::read_current_context(area.base())
        },
        #[cfg(not(all(target_arch = "aarch64", not(feature = "host-test"))))]
        CurrentContextSource::RuntimeAnchor => area.runtime_anchor().current_context_raw(),
    };
    let pointer = validated_context_pointer(raw)?;
    // SAFETY: context publication only accepts pinned headers that remain
    // alive while current, and the caller retains the required CPU pin.
    let context_area = unsafe { pointer.as_ref() }
        .cpu_area()
        .ok_or(CpuLocalError::CurrentContextMismatch)?;
    if context_area != area {
        return Err(CpuLocalError::CurrentContextMismatch);
    }
    Ok(pointer)
}

/// Reads the current header before a caller can construct its migration guard.
///
/// # Safety
///
/// The caller must keep the owning execution context alive and must not
/// dereference the result after a context switch.
#[doc(hidden)]
pub unsafe fn current_context_unpinned() -> Result<NonNull<ExecutionContextHeader>, CpuLocalError> {
    match imp::CURRENT_MODEL.current_context_source(cfg!(feature = "tls")) {
        #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
        CurrentContextSource::ArchitectureRegister => {
            // The architecture current source does not require a sampled CPU
            // area. Reading one first would race migration because this
            // function is itself used to construct the preemption guard.
            let register = unsafe { imp::read_current_context(0) };
            validated_context_pointer(register)
        }
        #[cfg(not(all(target_arch = "aarch64", not(feature = "host-test"))))]
        CurrentContextSource::RuntimeAnchor => loop {
            // Architectures whose current source is also the kernel TLS base
            // keep current in the CPU runtime anchor. Retry if migration
            // changes the area before the guard can be constructed.
            let area = current_area()?;
            let register = unsafe { imp::read_current_context(area.base()) };
            if unsafe { imp::read_cpu_base()? } != area.base() {
                continue;
            }
            return validated_context_pointer(register);
        },
    }
}

/// Reports whether `context` is the CPU area's permanent boot context.
///
/// Runtime layers use this distinction before they publish their first owned
/// execution context. The check compares identities only; the boot context
/// does not carry a runtime kind, task cookie, or consumer pointer.
#[doc(hidden)]
pub fn is_permanent_boot_context(
    context: NonNull<ExecutionContextHeader>,
) -> Result<bool, CpuLocalError> {
    let area = current_area()?;
    Ok(context == NonNull::from(area.prefix().boot_context().header()))
}

fn validated_context_pointer(raw: usize) -> Result<NonNull<ExecutionContextHeader>, CpuLocalError> {
    if raw == 0 || !raw.is_multiple_of(core::mem::align_of::<ExecutionContextHeader>()) {
        return Err(CpuLocalError::CurrentContextMismatch);
    }
    NonNull::new(raw as *mut ExecutionContextHeader).ok_or(CpuLocalError::CurrentContextMismatch)
}

#[cfg(feature = "host-test")]
pub(crate) mod host_test {
    /// Number of modeled architecture-register operations since the last reset.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct RegisterReadCounts {
        /// Reads of the architecture CPU-area base.
        pub cpu_base: usize,
        /// Reads of the selected architecture current-context source.
        pub current_context: usize,
        /// Complete reconstructions and identity checks of an initialized area.
        pub initialized_area_validations: usize,
    }

    /// Resets the current host thread's modeled register-operation counters.
    pub fn reset_register_read_counts() {
        super::imp::reset_register_read_counts();
    }

    /// Returns the current host thread's modeled register-operation counters.
    pub fn register_read_counts() -> RegisterReadCounts {
        super::imp::register_read_counts()
    }

    pub(crate) fn record_initialized_area_validation() {
        super::imp::record_initialized_area_validation();
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
    fn independent_current_register_ignores_kernel_tls_feature() {
        let independent = ArchitectureCurrentModel {
            linux_current: CurrentContextSource::ArchitectureRegister,
            unikernel_tls: CurrentContextSource::ArchitectureRegister,
        };
        assert_eq!(
            independent.current_context_source(false),
            CurrentContextSource::ArchitectureRegister,
        );
        assert_eq!(
            independent.current_context_source(true),
            CurrentContextSource::ArchitectureRegister,
        );
    }

    #[test]
    fn aliased_current_register_follows_kernel_tls_feature() {
        let aliased = ArchitectureCurrentModel {
            linux_current: CurrentContextSource::ArchitectureRegister,
            unikernel_tls: CurrentContextSource::RuntimeAnchor,
        };
        assert_eq!(
            aliased.current_context_source(false),
            CurrentContextSource::ArchitectureRegister,
        );
        assert_eq!(
            aliased.current_context_source(true),
            CurrentContextSource::RuntimeAnchor,
        );
    }

    #[test]
    fn current_context_unpinned_survives_migration_during_bootstrap_read() {
        let first = modeled_area(0);
        let second = modeled_area(1);
        let first_boot = first.prefix().boot_context().header();

        // SAFETY: this host thread serially owns both leaked CPU fixtures.
        unsafe { imp::install_cpu_base(first.base(), first_boot as *const _ as usize) };
        imp::migrate_on_next_current_read(second.base());

        assert_eq!(
            // SAFETY: both boot headers have process-lifetime storage.
            unsafe { current_context_unpinned() },
            if cfg!(feature = "tls") {
                Ok(NonNull::from(second.prefix().boot_context().header()))
            } else {
                Ok(NonNull::from(first_boot))
            },
        );
    }

    #[test]
    fn current_context_unpinned_rejects_an_uninstalled_host_area() {
        let rejected = std::thread::spawn(move || {
            // SAFETY: the fresh host thread has no installed CPU area, so no
            // execution-context pointer can be returned.
            unsafe { current_context_unpinned() }.is_err()
        })
        .join()
        .expect("host current-context probe panicked");

        assert!(rejected);
    }

    #[test]
    fn permanent_boot_context_is_classified_by_area_identity() {
        let area = modeled_area(0);
        let boot = area.prefix().boot_context().header();
        let runtime_context = Box::pin(ExecutionContextHeader::new());

        // SAFETY: this host thread serially owns the leaked CPU fixture.
        unsafe { imp::install_cpu_base(area.base(), boot as *const _ as usize) };

        assert_eq!(is_permanent_boot_context(NonNull::from(boot)), Ok(true));
        assert_eq!(
            is_permanent_boot_context(runtime_context.as_ref().as_non_null()),
            Ok(false)
        );
    }

    #[test]
    #[cfg(not(feature = "tls"))]
    fn architecture_current_is_authoritative_when_anchor_is_stale() {
        let area = modeled_area(0);
        let boot = area.prefix().boot_context().header();
        let next = Box::pin(ExecutionContextHeader::new());

        // SAFETY: this host thread serially owns the modeled CPU and both
        // process-lifetime context headers.
        unsafe { imp::install_cpu_base(area.base(), boot as *const _ as usize) };
        unsafe {
            crate::with_cpu_pin(|pin| {
                let next_epoch = next.as_ref().bind_cpu(area).unwrap();
                imp::set_architecture_current(next.as_ref().as_non_null().as_ptr() as usize);

                assert_eq!(current_context(pin), Ok(next.as_ref().as_non_null()));

                imp::set_architecture_current(0);
                next.as_ref().unbind_cpu(next_epoch).unwrap();
            })
        }
        .unwrap();
    }
}

/// Binds and publishes the first execution context on an offline CPU.
///
/// # Safety
///
/// The CPU must remain offline and trap-free. `header` must stay pinned and
/// alive until its owner replaces it through a prepared switch.
#[doc(hidden)]
pub unsafe fn install_bootstrap_context(
    pin: &CpuPin<'_>,
    header: Pin<&ExecutionContextHeader>,
) -> Result<(), ContextSwitchError> {
    let epoch = unsafe { header.bind_cpu(pin.area()) }?;
    let pointer = header.as_non_null().as_ptr() as usize;
    match imp::CURRENT_MODEL.current_context_source(cfg!(feature = "tls")) {
        #[cfg(not(all(target_arch = "aarch64", not(feature = "host-test"))))]
        CurrentContextSource::RuntimeAnchor => unsafe {
            commit_current_context(pin.area(), pointer)
        },
        #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
        CurrentContextSource::ArchitectureRegister => unsafe {
            imp::write_current_context(pointer)
        },
    }
    if current_context(pin) != Ok(header.as_non_null()) {
        // The register is already committed, so continuing would make all
        // later Rust execution unsound. Rollback is intentionally impossible.
        let _ = epoch;
        fatal_register_invariant();
    }
    Ok(())
}

/// Reads execution-context-owned kernel TLS under an explicit CPU pin.
#[cfg(feature = "tls")]
pub fn kernel_tls(_pin: &CpuPin<'_>) -> usize {
    unsafe { imp::read_kernel_tls() }
}

/// Installs execution-context-owned kernel TLS at an offline bootstrap boundary.
///
/// # Safety
///
/// The caller must own the offline CPU or IRQ-disabled final context switch, and
/// `value` must remain a valid TLS base for the installed execution context.
#[cfg(feature = "tls")]
#[doc(hidden)]
pub unsafe fn install_kernel_tls(_pin: &CpuPin<'_>, value: usize) {
    unsafe { imp::write_kernel_tls(value) };
}

#[cold]
#[inline(never)]
pub(crate) fn fatal_register_invariant() -> ! {
    panic!("CPU-local register commit did not retain the validated state")
}

#[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
#[inline(always)]
pub(crate) unsafe fn enter_x86_preemption() {
    unsafe { imp::enter_preemption() };
}
