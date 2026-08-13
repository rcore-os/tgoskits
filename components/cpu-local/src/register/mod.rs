//! Architecture register primitives and shared validation.

#[cfg(feature = "task-test-hooks")]
use core::sync::atomic::AtomicUsize;
use core::{num::NonZeroUsize, pin::Pin, ptr::NonNull, sync::atomic::Ordering};

#[cfg(feature = "task-test-hooks")]
static PREEMPT_GUARD_OWNER_RESOLUTIONS: AtomicUsize = AtomicUsize::new(0);

/// Architecture-selected owner of one live ordinary preemption guard.
///
/// Fixed-anchor architectures encode their CPU-local preemption word with a
/// private sentinel. Load/store architectures encode the pinned current-thread
/// header so the matching exit can reuse the exact owner selected at entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct PreemptGuardOwner(NonZeroUsize);

impl PreemptGuardOwner {
    #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
    const FIXED_CPU: Self = Self(NonZeroUsize::MIN);

    #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
    fn from_current(current: NonNull<CurrentThreadHeader>) -> Self {
        // CurrentThreadHeader is aligned and therefore always non-null here.
        Self(unsafe { NonZeroUsize::new_unchecked(current.as_ptr() as usize) })
    }

    /// Reconstructs a runtime-transported owner token.
    ///
    /// # Safety
    ///
    /// `raw` must have been returned by [`Self::into_raw`] for a live guard on
    /// this execution context and must be consumed by exactly one matching
    /// preemption-guard exit.
    pub const unsafe fn from_raw(raw: usize) -> Option<Self> {
        match NonZeroUsize::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    /// Returns the opaque representation used by the runtime boundary.
    pub const fn into_raw(self) -> usize {
        self.0.get()
    }

    #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
    fn current(self) -> &'static CurrentThreadHeader {
        // SAFETY: construction accepts only the current pinned header. The
        // live preemption depth prevents that task from completing or moving
        // between construction and the matching exit operation.
        unsafe { &*(self.0.get() as *const CurrentThreadHeader) }
    }
}

use crate::{
    CpuAreaRef, CpuLocalError, CpuPin, CurrentThreadHeader, PreemptExit, ThreadSwitchError,
};

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
    pub(super) current_source_aliases_kernel_tls: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CurrentThreadSource {
    Architecture,
    CpuRuntimeAnchor,
}

impl ArchitectureCurrentModel {
    const fn current_thread_source(self, tls_enabled: bool) -> CurrentThreadSource {
        if tls_enabled && self.current_source_aliases_kernel_tls {
            CurrentThreadSource::CpuRuntimeAnchor
        } else {
            CurrentThreadSource::Architecture
        }
    }

    const fn current_thread_requires_irq_exclusion(self, tls_enabled: bool) -> bool {
        matches!(
            self.current_thread_source(tls_enabled),
            CurrentThreadSource::CpuRuntimeAnchor
        )
    }
}

/// Reports whether an unpinned current-task read must exclude local IRQs.
///
/// Architectures whose current register is reused as the kernel TLS base read
/// scheduler current from the CPU runtime anchor. Local IRQ exclusion is then
/// required to keep the selected CPU stable through the complete operation.
#[doc(hidden)]
pub const fn scheduler_current_requires_irq_exclusion() -> bool {
    imp::CURRENT_MODEL.current_thread_requires_irq_exclusion(cfg!(feature = "tls"))
}

/// Reads ordinary preemption nesting from the architecture-selected owner.
#[doc(hidden)]
#[inline(always)]
pub fn scheduler_preempt_guard_depth() -> Result<u32, CpuLocalError> {
    #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
    {
        Ok(unsafe { imp::preempt_guard_depth() })
    }
    #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
    {
        with_scheduler_preempt_state(CurrentThreadHeader::preempt_guard_depth)
    }
}

/// Reads ordinary preemption nesting from a live guard owner.
#[doc(hidden)]
#[inline(always)]
pub fn scheduler_owned_preempt_guard_depth(owner: PreemptGuardOwner) -> u32 {
    #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
    {
        assert_eq!(owner, PreemptGuardOwner::FIXED_CPU);
        unsafe { imp::preempt_guard_depth() }
    }
    #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
    {
        owner.current().preempt_guard_depth()
    }
}

/// Publishes scheduler work into the architecture-selected preemption word.
#[doc(hidden)]
#[inline(always)]
pub fn scheduler_set_preempt_need_resched() -> Result<(), CpuLocalError> {
    #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
    unsafe {
        imp::set_preempt_need_resched();
    }
    #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
    with_scheduler_preempt_state(CurrentThreadHeader::set_preempt_need_resched)?;
    Ok(())
}

/// Clears scheduler work after the current CPU safe point drains its queues.
#[doc(hidden)]
#[inline(always)]
pub fn scheduler_clear_preempt_need_resched() -> Result<(), CpuLocalError> {
    #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
    unsafe {
        imp::clear_preempt_need_resched();
    }
    #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
    with_scheduler_preempt_state(CurrentThreadHeader::clear_preempt_need_resched)?;
    Ok(())
}

/// Enters one ordinary preemption guard and returns its architecture owner.
#[doc(hidden)]
#[inline(always)]
pub fn scheduler_enter_preempt_guard() -> Result<PreemptGuardOwner, CpuLocalError> {
    #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
    unsafe {
        imp::enter_preempt_guard();
        Ok(PreemptGuardOwner::FIXED_CPU)
    }
    #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
    {
        let owner = resolve_preempt_guard_owner()?;
        owner.current().enter_preempt_guard();
        Ok(owner)
    }
}

/// Consumes a nested guard or retains the final depth for baton conversion.
#[doc(hidden)]
#[inline(always)]
pub fn scheduler_prepare_preempt_guard_exit(owner: PreemptGuardOwner) -> PreemptExit {
    #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
    {
        assert_eq!(owner, PreemptGuardOwner::FIXED_CPU);
        unsafe { imp::prepare_preempt_guard_exit() }
    }
    #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
    {
        owner.current().prepare_preempt_guard_exit()
    }
}

/// Resolves the owner of an already-live ordinary preemption guard.
///
/// # Safety
///
/// The current execution context must retain at least one ordinary preemption
/// depth until the returned owner is consumed by the matching exit path.
#[doc(hidden)]
#[inline(always)]
pub unsafe fn scheduler_current_preempt_guard_owner() -> Result<PreemptGuardOwner, CpuLocalError> {
    #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
    {
        Ok(PreemptGuardOwner::FIXED_CPU)
    }
    #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
    {
        resolve_preempt_guard_owner()
    }
}

#[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
#[inline(always)]
fn resolve_preempt_guard_owner() -> Result<PreemptGuardOwner, CpuLocalError> {
    record_preempt_guard_owner_resolution();
    // SAFETY: synchronous execution keeps this task allocation alive. Entry
    // immediately publishes the guard depth before the token can escape; the
    // current-owner variant requires an already-live depth from its caller.
    unsafe { scheduler_current_thread() }.map(PreemptGuardOwner::from_current)
}

#[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
#[inline(always)]
fn record_preempt_guard_owner_resolution() {
    #[cfg(feature = "task-test-hooks")]
    PREEMPT_GUARD_OWNER_RESOLUTIONS.fetch_add(1, Ordering::Relaxed);
}

/// Resets the target-side count of preemption-guard owner resolutions.
#[cfg(feature = "task-test-hooks")]
#[doc(hidden)]
pub fn reset_preempt_guard_owner_resolution_count() {
    PREEMPT_GUARD_OWNER_RESOLUTIONS.store(0, Ordering::Relaxed);
}

/// Takes the target-side count of preemption-guard owner resolutions.
#[cfg(feature = "task-test-hooks")]
#[doc(hidden)]
pub fn take_preempt_guard_owner_resolution_count() -> usize {
    PREEMPT_GUARD_OWNER_RESOLUTIONS.swap(0, Ordering::Relaxed)
}

/// Converts the exact final ordinary guard into scheduler-owned state.
#[doc(hidden)]
#[inline(always)]
pub fn scheduler_consume_final_preempt_guard(owner: PreemptGuardOwner) -> bool {
    #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
    {
        assert_eq!(owner, PreemptGuardOwner::FIXED_CPU);
        unsafe { imp::consume_final_preempt_guard() }
    }
    #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
    {
        owner.current().consume_final_preempt_guard()
    }
}

#[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
#[inline(always)]
fn with_scheduler_preempt_state<R>(
    operation: impl for<'current> FnOnce(&'current CurrentThreadHeader) -> R,
) -> Result<R, CpuLocalError> {
    // SAFETY: the scheduler owns the current task allocation. A nested switch
    // resumes this same execution on its stable pinned header before the
    // operation continues, while the non-escaping callback prevents a borrow
    // from outliving the register observation.
    let current = unsafe { scheduler_current_thread()? };
    Ok(operation(unsafe { current.as_ref() }))
}

#[cfg(feature = "host-test")]
pub(crate) mod host_test {
    /// Number of modeled architecture register reads since the last reset.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct RegisterReadCounts {
        /// Reads of the architecture CPU-area base.
        pub cpu_base: usize,
        /// Reads of the architecture current-thread pointer.
        pub current_thread: usize,
        /// Full reconstructions and identity checks of an initialized area.
        pub initialized_area_validations: usize,
    }

    /// Resets the current host thread's modeled register read counters.
    pub fn reset_register_read_counts() {
        super::imp::reset_register_read_counts();
    }

    /// Returns the current host thread's modeled register read counters.
    pub fn register_read_counts() -> RegisterReadCounts {
        super::imp::register_read_counts()
    }

    pub(crate) fn record_initialized_area_validation() {
        super::imp::record_initialized_area_validation();
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

/// Reads the architecture CPU-area base for a scheduler-owned access.
///
/// # Safety
///
/// The caller must prevent migration and context switches while using the
/// selected CPU. The installed area must retain its shutdown lifetime.
#[inline(always)]
pub(crate) unsafe fn scheduler_current_cpu_base() -> Result<usize, CpuLocalError> {
    let area_base = unsafe { imp::read_cpu_base()? };
    if area_base == 0 {
        return Err(CpuLocalError::AreaNotInstalled);
    }
    if !area_base.is_multiple_of(core::mem::align_of::<crate::CpuAreaPrefix>()) {
        return Err(CpuLocalError::InvalidAreaBase { base: area_base });
    }
    Ok(area_base)
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
#[inline(always)]
pub unsafe fn scheduler_current_thread() -> Result<NonNull<CurrentThreadHeader>, CpuLocalError> {
    match imp::CURRENT_MODEL.current_thread_source(cfg!(feature = "tls")) {
        CurrentThreadSource::Architecture => {
            // The architecture current source does not require a sampled CPU
            // area. Reading one first would race migration because this
            // function is itself used to construct the preemption guard.
            let register = unsafe { imp::read_current_thread(0) };
            scheduler_header_from_raw(register, None)
        }
        CurrentThreadSource::CpuRuntimeAnchor => loop {
            // Architectures whose current source is also the kernel TLS base
            // keep current in the CPU runtime anchor. Retry if migration
            // changes the area before the guard can be constructed.
            let area = current_area()?;
            let register = unsafe { imp::read_current_thread(area.base()) };
            if unsafe { imp::read_cpu_base()? } != area.base() {
                continue;
            }
            return scheduler_header_from_raw(register, Some(area));
        },
    }
}

/// Runs `f` with the task-owned header selected by the architecture `current`
/// source.
///
/// Unlike a current-CPU observation, this does not pin the caller. A preemption
/// may suspend and migrate the task, but execution can resume in this function
/// only through the same pinned task context. The scheduler therefore retains
/// the header for the complete call, matching Linux's stable `current` task
/// identity across migration.
#[doc(hidden)]
#[inline(always)]
pub fn with_scheduler_current_thread<R>(
    f: impl for<'current> FnOnce(&'current CurrentThreadHeader) -> R,
) -> Result<R, CpuLocalError> {
    // SAFETY: synchronous execution cannot outlive its current scheduler
    // context. Preemption may move that context between CPUs, but the context
    // allocation remains pinned and live until this stack resumes and returns.
    let current = unsafe { scheduler_current_thread()? };
    // SAFETY: the argument above establishes the header lifetime for this call,
    // and the higher-ranked closure cannot return a borrow of the header.
    Ok(f(unsafe { current.as_ref() }))
}

#[inline(always)]
fn scheduler_header_from_raw(
    raw: usize,
    expected_area: Option<CpuAreaRef>,
) -> Result<NonNull<CurrentThreadHeader>, CpuLocalError> {
    if raw == 0 || !raw.is_multiple_of(core::mem::align_of::<CurrentThreadHeader>()) {
        return Err(CpuLocalError::CurrentThreadMismatch);
    }
    let pointer = NonNull::new(raw as *mut CurrentThreadHeader)
        .ok_or(CpuLocalError::CurrentThreadMismatch)?;
    if let Some(expected_area) = expected_area {
        // SAFETY: the runtime anchor may only publish a pinned scheduler
        // header. Alignment and non-nullness were checked above; comparing the
        // bound area catches a stale anchor before guard state is accessed.
        if unsafe { pointer.as_ref() }.cpu_area() != Some(expected_area) {
            return Err(CpuLocalError::CurrentThreadMismatch);
        }
    }
    Ok(pointer)
}

/// Reads the logical CPU identity before the scheduler can construct its guard.
///
/// # Safety
///
/// The caller must keep the scheduler-owned current task alive and must not
/// use this observation after a context switch.
#[doc(hidden)]
#[inline(always)]
pub unsafe fn scheduler_current_cpu_index() -> Result<crate::CpuIndex, CpuLocalError> {
    let current = unsafe { scheduler_current_thread()? };
    unsafe { current.as_ref() }
        .cpu_index()
        .ok_or(CpuLocalError::CurrentThreadMismatch)
}

#[cfg(all(test, feature = "host-test"))]
mod tests {
    use core::mem::MaybeUninit;

    use super::*;
    use crate::{CpuAreaPrefix, CpuIndex, CurrentContext};

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
            current_source_aliases_kernel_tls: false,
        };
        assert_eq!(
            independent.current_thread_source(false),
            CurrentThreadSource::Architecture,
        );
        assert_eq!(
            independent.current_thread_source(true),
            CurrentThreadSource::Architecture,
            "an independent current register must not follow the kernel TLS feature",
        );
    }

    #[test]
    fn aliased_current_register_follows_kernel_tls_feature() {
        let aliased = ArchitectureCurrentModel {
            current_source_aliases_kernel_tls: true,
        };
        assert_eq!(
            aliased.current_thread_source(false),
            CurrentThreadSource::Architecture,
        );
        assert_eq!(
            aliased.current_thread_source(true),
            CurrentThreadSource::CpuRuntimeAnchor,
        );
        assert!(aliased.current_thread_requires_irq_exclusion(true));
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

    #[test]
    fn scheduler_current_thread_rejects_an_uninstalled_host_area() {
        let rejected = std::thread::spawn(move || {
            // SAFETY: the fresh host thread has no installed CPU area, so no
            // scheduler-owned pointer can be returned.
            matches!(
                unsafe { scheduler_current_thread() },
                Err(CpuLocalError::CurrentThreadMismatch)
            )
        })
        .join()
        .expect("host current-thread probe panicked");

        assert!(rejected);
    }

    #[test]
    fn scheduler_current_thread_rejects_a_misaligned_publication() {
        std::thread::spawn(|| {
            let area = modeled_area(0);
            // SAFETY: this fresh host thread exclusively owns the modeled CPU
            // area and publishes the malformed value only for this probe.
            unsafe {
                install_cpu_area(area).expect("modeled CPU install must succeed");
                commit_current_thread(area, 1);
            }

            assert_eq!(
                // SAFETY: the test deliberately checks that validation rejects
                // the malformed value before any dereference can occur.
                unsafe { scheduler_current_thread() },
                Err(CpuLocalError::CurrentThreadMismatch),
            );
        })
        .join()
        .expect("modeled CPU test thread must not panic");
    }

    #[test]
    fn installed_cpu_area_starts_with_linux_boot_preemption_disabled() {
        std::thread::spawn(|| {
            let area = modeled_area(0);
            // SAFETY: this fresh host thread exclusively owns the offline CPU
            // fixture and cannot receive a scheduler interrupt.
            unsafe { install_cpu_area(area) }.expect("modeled CPU install must succeed");

            assert_eq!(
                scheduler_preempt_guard_depth(),
                Ok(1),
                "boot current must retain PREEMPT_DISABLED until rq/current publication",
            );
        })
        .join()
        .expect("modeled CPU test thread must not panic");
    }

    #[test]
    fn generic_preempt_state_follows_current_thread_publication() {
        std::thread::spawn(|| {
            let area = modeled_area(0);
            let first = Box::pin(CurrentThreadHeader::new(
                CurrentContext::from_raw(1).expect("test context must be non-zero"),
            ));
            let second = Box::pin(CurrentThreadHeader::new(
                CurrentContext::from_raw(2).expect("test context must be non-zero"),
            ));

            // SAFETY: this fresh host thread models one offline CPU and owns
            // the leaked CPU fixture for the complete test.
            unsafe { install_cpu_area(area) }.expect("modeled CPU install must succeed");
            // SAFETY: the modeled CPU is serialized and receives no interrupts.
            unsafe {
                crate::with_cpu_pin(|pin| {
                    install_bootstrap_thread(pin, first.as_ref())
                        .expect("first task publication must succeed");
                    let first_owner =
                        scheduler_enter_preempt_guard().expect("first guard enter must succeed");
                    assert_eq!(scheduler_preempt_guard_depth(), Ok(1));

                    let (prepared, mut previous) =
                        crate::prepare_thread_switch(pin, first.as_ref(), second.as_ref())
                            .expect("switch to second task must prepare");
                    prepared.commit();
                    previous
                        .finish(first.as_ref())
                        .expect("first task binding must withdraw");

                    assert_eq!(
                        scheduler_preempt_guard_depth(),
                        Ok(0),
                        "an incoming task must not inherit the previous task's guard depth",
                    );
                    let second_owner =
                        scheduler_enter_preempt_guard().expect("second guard enter must succeed");
                    let _ = scheduler_prepare_preempt_guard_exit(second_owner);
                    assert_eq!(scheduler_preempt_guard_depth(), Ok(0));

                    let (prepared, mut previous) =
                        crate::prepare_thread_switch(pin, second.as_ref(), first.as_ref())
                            .expect("switch back to first task must prepare");
                    prepared.commit();
                    previous
                        .finish(second.as_ref())
                        .expect("second task binding must withdraw");

                    assert_eq!(
                        scheduler_preempt_guard_depth(),
                        Ok(1),
                        "the suspended task must retain its guard depth",
                    );
                    let _ = scheduler_prepare_preempt_guard_exit(first_owner);
                    assert_eq!(scheduler_preempt_guard_depth(), Ok(0));
                })
            }
            .expect("modeled CPU pin must succeed");
        })
        .join()
        .expect("modeled CPU test thread must not panic");
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
