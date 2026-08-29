use alloc::boxed::Box;
use core::{
    cell::UnsafeCell,
    mem::offset_of,
    pin::Pin,
    ptr::{self, NonNull},
};

use ax_hal::percpu::{
    CpuPin, ExecutionContextHeader, PreparedContextSwitch, PreviousContextBinding,
};
use ax_task::{
    TaskError,
    runtime::{
        ContextSwitch, ContextThreadBinding, CurrentThreadPublication, ExecutionContextHandle,
        KernelContextRequest, RuntimeHandleResult, RuntimeStatus, StackHandle, UserContextRequest,
    },
};

use super::{
    resources::{RuntimeStack, runtime_tls_pointer},
    runtime_status_error, with_current_cpu_pin,
};

/// Reports whether a kernel page fault hit the current runtime stack guard.
pub fn diagnose_current_stack_guard_page_fault(fault: ax_memory_addr::VirtAddr) -> bool {
    #[cfg(feature = "stack-guard-page")]
    {
        // SAFETY: trap execution cannot migrate before returning through its
        // architecture epilogue.
        unsafe {
            with_current_cpu_pin(|cpu_pin| {
                let Ok(context) = current_runtime_context(cpu_pin) else {
                    return false;
                };
                let stack = context.stack.into_raw();
                if stack == 0 {
                    return false;
                }
                // SAFETY: the scheduler owns the stack until this context can
                // no longer run, and the current header keeps it on-CPU.
                let stack = &*ptr::with_exposed_provenance::<RuntimeStack>(stack);
                let super::resources::StackBacking::GuardedPages { guard_size, .. } =
                    &stack.backing
                else {
                    return false;
                };
                let guard_end = stack.base.saturating_add(*guard_size);
                if !(stack.base..guard_end).contains(&fault.as_usize()) {
                    return false;
                }
                error!(
                    "task stack guard page hit: fault_addr={:#x}, stack=[{:#x}..{:#x}), \
                     guard=[{:#x}..{:#x})",
                    fault.as_usize(),
                    guard_end,
                    stack.usable_top,
                    stack.base,
                    guard_end,
                );
                true
            })
        }
    }
    #[cfg(not(feature = "stack-guard-page"))]
    {
        let _ = fault;
        false
    }
}

struct RuntimeSwitchTail {
    previous: NonNull<ExecutionContextHeader>,
    binding: PreviousContextBinding,
}

/// Runtime-owned architecture context and its pinned scheduler identity.
///
/// The header stays at offset zero so a current-thread publication can be
/// checked against its context handle without a second registry or per-CPU
/// pointer. `switch_tail` is written only while this context is off-CPU and is
/// consumed exactly once after it becomes current with local IRQs disabled.
#[repr(C)]
struct RuntimeContext {
    header: ExecutionContextHeader,
    publication: UnsafeCell<CurrentThreadPublication>,
    inner: Box<UnsafeCell<ax_hal::context::TaskContext>>,
    stack: StackHandle,
    switch_tail: UnsafeCell<Option<RuntimeSwitchTail>>,
}

#[derive(Clone, Copy)]
enum InitialPreemptionState {
    Enabled,
    BootstrapDisabled,
}

const _: () = assert!(offset_of!(RuntimeContext, header) == 0);

impl RuntimeContext {
    fn allocate(
        inner: ax_hal::context::TaskContext,
        stack: StackHandle,
        preemption: InitialPreemptionState,
    ) -> *mut RuntimeContext {
        let inner = Box::new(UnsafeCell::new(inner));
        let header = match preemption {
            InitialPreemptionState::Enabled => ExecutionContextHeader::new(),
            InitialPreemptionState::BootstrapDisabled => ExecutionContextHeader::new_bootstrap(),
        };
        Box::into_raw(Box::new(Self {
            header,
            publication: UnsafeCell::new(CurrentThreadPublication::NONE),
            inner,
            stack,
            switch_tail: UnsafeCell::new(None),
        }))
    }

    fn header(&self) -> Pin<&ExecutionContextHeader> {
        // SAFETY: every RuntimeContext is constructed in a Box and is never
        // moved before destruction after its header is no longer published.
        unsafe { Pin::new_unchecked(&self.header) }
    }

    fn has_switch_tail(&self) -> bool {
        // SAFETY: only the incoming scheduler continuation reads this slot;
        // the context is current and local IRQs serialize scheduler entry.
        unsafe { (*self.switch_tail.get()).is_some() }
    }

    unsafe fn stage_switch_tail(&self, tail: RuntimeSwitchTail) -> Result<(), RuntimeStatus> {
        // SAFETY: the scheduler selected this context while it is off-CPU and
        // holds the only right to prepare its next incoming continuation.
        let slot = unsafe { &mut *self.switch_tail.get() };
        if slot.is_some() {
            return Err(RuntimeStatus::Busy);
        }
        *slot = Some(tail);
        Ok(())
    }

    unsafe fn finish_switch_tail(&self) {
        // SAFETY: the current incoming continuation owns this slot with local
        // IRQs disabled and completes the one-shot previous-binding token.
        let slot = unsafe { &mut *self.switch_tail.get() };
        let tail = slot
            .take()
            .expect("incoming runtime context is missing its switch tail");
        // SAFETY: the outgoing header stays pinned and unreclaimable through
        // the scheduler `on_cpu` handoff; this tail owns its exact epoch.
        let previous = unsafe { Pin::new_unchecked(tail.previous.as_ref()) };
        unsafe { tail.binding.finish(previous) }
            .expect("runtime switch tail did not own the exact previous CPU binding");
    }
}

fn runtime_context(
    handle: ExecutionContextHandle,
) -> Result<&'static RuntimeContext, RuntimeStatus> {
    if handle.is_none() {
        return Err(RuntimeStatus::InvalidHandle);
    }
    let context = ptr::with_exposed_provenance::<RuntimeContext>(handle.into_raw());
    // SAFETY: TaskRuntime receives only live handles created by this provider;
    // the scheduler retains context ownership through every runtime call.
    let context = unsafe { &*context };
    if !ptr::eq(
        ptr::addr_of!(context.header),
        context as *const RuntimeContext as *const ExecutionContextHeader,
    ) {
        return Err(RuntimeStatus::InvalidHandle);
    }
    Ok(context)
}

fn current_runtime_context(cpu_pin: &CpuPin) -> Result<&'static RuntimeContext, RuntimeStatus> {
    let current = ax_hal::percpu::current_context(cpu_pin)
        .map_err(|_| RuntimeStatus::InvalidHandle)?
        .as_ptr()
        .expose_provenance();
    let header = ptr::with_exposed_provenance::<ExecutionContextHeader>(current);
    // SAFETY: `current_context` has already validated that the published,
    // pinned header is live and bound to the supplied CPU pin.
    let header = unsafe { &*header };
    // RuntimeContext is `repr(C)` and the pinned header is its offset-zero
    // owner identity. The independently allocated architecture context keeps
    // ContextIdentity free of self-referential outer pointers.
    let context = unsafe { &*ptr::from_ref(header).cast::<RuntimeContext>() };
    if !ptr::eq(context.header().get_ref(), header) {
        return Err(RuntimeStatus::InvalidHandle);
    }
    Ok(context)
}

pub(super) fn bind_bootstrap_runtime_context(
    cpu_pin: &CpuPin,
    handle: ExecutionContextHandle,
    kernel_tls: usize,
) -> Result<(), TaskError> {
    let boot_context =
        ax_hal::percpu::current_context(cpu_pin).map_err(|_| TaskError::InvalidConfiguration)?;
    if !ax_hal::percpu::is_permanent_boot_context(boot_context)
        .map_err(|_| TaskError::InvalidConfiguration)?
    {
        return Err(TaskError::InvalidConfiguration);
    }
    let context = runtime_context(handle).map_err(runtime_status_error)?;
    // SAFETY: the CPU is still offline and trap-free, while the scheduler
    // record keeps this pinned header alive until its switch tail withdraws it.
    unsafe { ax_hal::percpu::install_bootstrap_context(cpu_pin, context.header()) }
        .map_err(|_| TaskError::InvalidConfiguration)?;
    #[cfg(feature = "tls")]
    // SAFETY: the same offline bootstrap boundary owns the task TLS register.
    unsafe {
        ax_hal::percpu::install_bootstrap_kernel_tls(
            cpu_pin,
            ax_hal::context::KernelTlsBase::new(kernel_tls),
        );
    }
    #[cfg(not(feature = "tls"))]
    assert_eq!(
        kernel_tls, 0,
        "TLS-disabled bootstrap must retain a zero TLS identity"
    );
    Ok(())
}

pub(super) fn finish_runtime_context_switch_tail() -> bool {
    // SAFETY: TaskSystem invokes this with the scheduler baton and local IRQs
    // disabled immediately after entering the incoming context.
    unsafe {
        with_current_cpu_pin(|cpu_pin| {
            let current = current_runtime_context(cpu_pin)
                .expect("incoming scheduler context is not runtime-owned");
            // SAFETY: the incoming context exclusively owns its staged one-shot tail.
            current.finish_switch_tail();
        })
    };
    super::address_space::take_context_switch_reclaim_ready()
}

pub(super) fn create_runtime_context(request: KernelContextRequest) -> RuntimeHandleResult {
    create_runtime_context_parts(request.stack, request.entry, request.tls)
}

pub(super) fn create_user_runtime_context(request: UserContextRequest) -> RuntimeHandleResult {
    #[cfg(not(feature = "uspace"))]
    {
        let _ = request;
        RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
    }
    #[cfg(feature = "uspace")]
    {
        create_runtime_context_parts(request.stack, request.entry, request.tls)
    }
}

fn create_runtime_context_parts(
    stack_handle: StackHandle,
    entry: ax_task::runtime::KernelEntry,
    tls_handle: ax_task::runtime::TlsHandle,
) -> RuntimeHandleResult {
    if stack_handle.is_none() {
        return RuntimeHandleResult::failure(RuntimeStatus::InvalidHandle);
    }
    // SAFETY: the scheduler keeps the stack handle live until context destroy.
    let stack = unsafe { &*ptr::with_exposed_provenance::<RuntimeStack>(stack_handle.into_raw()) };
    let tls_pointer = runtime_tls_pointer(tls_handle);
    let mut context = ax_hal::context::TaskContext::new();
    context.init(
        entry as usize,
        ax_memory_addr::VirtAddr::from(stack.usable_top),
        ax_hal::context::KernelTlsBase::new(tls_pointer),
    );
    RuntimeHandleResult::success(
        RuntimeContext::allocate(context, stack_handle, InitialPreemptionState::Enabled)
            .expose_provenance(),
    )
}

pub(super) fn create_bootstrap_context() -> ExecutionContextHandle {
    let context = ax_hal::context::TaskContext::new();
    let context = RuntimeContext::allocate(
        context,
        StackHandle::NONE,
        InitialPreemptionState::BootstrapDisabled,
    );
    // SAFETY: Box::into_raw yields a non-null uniquely owned RuntimeContext
    // that stays live until destroy_runtime_context consumes the handle.
    unsafe { ExecutionContextHandle::from_raw(context.expose_provenance()) }
}

pub(super) fn destroy_runtime_context(handle: ExecutionContextHandle) -> RuntimeStatus {
    if handle.is_none() {
        return RuntimeStatus::InvalidHandle;
    }
    let context = ptr::with_exposed_provenance_mut::<RuntimeContext>(handle.into_raw());
    // SAFETY: the scheduler keeps the runtime handle live while asking whether
    // its physical CPU handoff has completed.
    let context_ref = unsafe { &*context };
    if context_ref.header.cpu_area().is_some() || context_ref.has_switch_tail() {
        return RuntimeStatus::Busy;
    }
    // SAFETY: the scheduler proves this context cannot run again and consumes
    // its runtime handle exactly once.
    drop(unsafe { Box::from_raw(context) });
    RuntimeStatus::Success
}

pub(super) fn bind_runtime_context_thread(binding: ContextThreadBinding) -> RuntimeStatus {
    if !binding.publication.identity().is_bound() || binding.publication.owner().is_none() {
        return RuntimeStatus::InvalidArgument;
    }
    let Ok(context) = runtime_context(binding.context) else {
        return RuntimeStatus::InvalidHandle;
    };
    // Binding is immutable and occurs exactly once before scheduler publication.
    if unsafe { *context.publication.get() } != CurrentThreadPublication::NONE {
        return RuntimeStatus::InvalidArgument;
    }
    // Context binding runs exactly once before scheduler publication, so this
    // is the sole write to the pinned current-thread publication.
    unsafe { *context.publication.get() = binding.publication };
    // Scheduler construction invokes this exactly once before the context can
    // enter a run queue. The bootstrap placeholder is likewise not consumed by
    // assembly until its first switch-out.
    unsafe { &mut *context.inner.get() }.set_context_header(context.header().as_non_null());
    RuntimeStatus::Success
}

/// Reads the immutable scheduler publication owned by the current task context.
pub(super) fn scheduler_current_thread_publication() -> CurrentThreadPublication {
    // SAFETY: the architecture current source identifies this executing
    // context. Preemption may suspend and migrate it during the read, but the
    // same pinned context resumes and its publication is immutable.
    let Ok(header) = (unsafe { ax_hal::percpu::current_context_unpinned() }) else {
        return CurrentThreadPublication::NONE;
    };
    let Ok(false) = ax_hal::percpu::is_permanent_boot_context(header) else {
        return CurrentThreadPublication::NONE;
    };
    let context = header.as_ptr().cast::<RuntimeContext>();
    // SAFETY: RuntimeContext embeds the published header at offset zero and
    // remains alive while this execution context can run or resume.
    unsafe { *(*context).publication.get() }
}

/// Restores the current x86 userspace FPU image after the final no-work snapshot.
pub(crate) fn prepare_current_user_fp_return() -> Result<(), TaskError> {
    #[cfg(all(target_arch = "x86_64", feature = "fp-simd"))]
    {
        // SAFETY: the user-return boundary keeps local IRQs disabled, so the
        // current header, runtime context, and CPU-local FPU owner stay pinned
        // through the restore and owner publication.
        unsafe {
            with_current_cpu_pin(|cpu_pin| {
                let context = current_runtime_context(cpu_pin).map_err(runtime_status_error)?;
                // SAFETY: this is the currently executing architecture context;
                // no scheduler or remote path can mutate it under IRQ exclusion.
                let architecture_context = &*context.inner.get();
                architecture_context.prepare_user_return_fp();
                Ok(())
            })
        }
    }
    #[cfg(not(all(target_arch = "x86_64", feature = "fp-simd")))]
    {
        Ok(())
    }
}

#[cfg(all(target_arch = "x86_64", feature = "fp-simd", feature = "uspace"))]
pub(super) fn validate_current_user_fp_clone_context() -> Result<(), TaskError> {
    if !ax_hal::asm::irqs_enabled() || ax_hal::irq::in_irq_context() {
        return Err(TaskError::UnsafeContext);
    }
    ax_hal::asm::disable_irqs();
    // SAFETY: local IRQ exclusion pins the current header while validating
    // that this call originates from a runtime-owned user task context.
    let result = unsafe {
        with_current_cpu_pin(|cpu_pin| {
            current_runtime_context(cpu_pin)
                .map(|_| ())
                .map_err(runtime_status_error)
        })
    };
    ax_hal::asm::enable_irqs();
    result
}

#[cfg(all(target_arch = "x86_64", feature = "fp-simd", feature = "uspace"))]
pub(super) fn inherit_current_user_fp_state(child_context: usize) {
    assert!(
        ax_hal::asm::irqs_enabled() && !ax_hal::irq::in_irq_context(),
        "x86 FPU inheritance requires ordinary task context",
    );
    assert_ne!(
        child_context, 0,
        "x86 FPU inheritance requires a child context"
    );
    let child = ptr::with_exposed_provenance_mut::<RuntimeContext>(child_context);
    ax_hal::asm::disable_irqs();
    // SAFETY: the child allocation is exclusively owned by resource creation
    // and remains unpublished. IRQ exclusion pins the current parent context
    // and its CPU-local FPU owner through the direct XSAVE into the child.
    unsafe {
        with_current_cpu_pin(|cpu_pin| {
            let parent = current_runtime_context(cpu_pin)
                .unwrap_or_else(|status| panic!("invalid FPU clone parent context: {status:?}"));
            assert!(!core::ptr::eq(parent, child));
            let parent_architecture_context = &*parent.inner.get();
            let child_architecture_context = &mut *(*child).inner.get();
            parent_architecture_context.clone_user_fp_state_into(child_architecture_context);
        })
    };
    ax_hal::asm::enable_irqs();
}

#[cfg(all(target_arch = "x86_64", feature = "fp-simd", feature = "uspace"))]
pub(super) fn capture_current_user_fp_state() -> Result<ax_hal::cpu::UserXstate, TaskError> {
    if !ax_hal::asm::irqs_enabled() || ax_hal::irq::in_irq_context() {
        return Err(TaskError::UnsafeContext);
    }
    ax_hal::asm::disable_irqs();
    // SAFETY: local IRQ exclusion pins the runtime context and CPU-local FPU
    // owner while the current hardware image is copied into a task-owned value.
    let result = unsafe {
        with_current_cpu_pin(|cpu_pin| {
            let context = current_runtime_context(cpu_pin).map_err(runtime_status_error)?;
            // SAFETY: this is the current architecture context and the IRQ-off
            // CPU pin excludes scheduler and remote context mutation.
            let architecture_context = &*context.inner.get();
            Ok(architecture_context.capture_user_fp_state())
        })
    };
    ax_hal::asm::enable_irqs();
    result
}

#[cfg(all(target_arch = "x86_64", feature = "fp-simd", feature = "uspace"))]
pub(super) fn replace_current_user_fp_state(
    state: ax_hal::cpu::UserXstate,
) -> Result<(), TaskError> {
    if !ax_hal::asm::irqs_enabled() || ax_hal::irq::in_irq_context() {
        return Err(TaskError::UnsafeContext);
    }
    ax_hal::asm::disable_irqs();
    // SAFETY: local IRQ exclusion pins the runtime context and CPU-local FPU
    // owner through the task-memory replacement, hardware restore, and owner
    // publication transaction.
    let result = unsafe {
        with_current_cpu_pin(|cpu_pin| {
            let context = current_runtime_context(cpu_pin).map_err(runtime_status_error)?;
            // SAFETY: this is the current architecture context and IRQ
            // exclusion prevents concurrent scheduler mutation.
            let architecture_context = &mut *context.inner.get();
            architecture_context.replace_user_fp_state(state);
            Ok(())
        })
    };
    ax_hal::asm::enable_irqs();
    result
}

pub(super) fn reset_current_user_fp_state() -> Result<(), TaskError> {
    #[cfg(all(target_arch = "x86_64", feature = "fp-simd", feature = "uspace"))]
    {
        if !ax_hal::asm::irqs_enabled() || ax_hal::irq::in_irq_context() {
            return Err(TaskError::UnsafeContext);
        }
        ax_hal::asm::disable_irqs();
        // SAFETY: local IRQ exclusion pins the current runtime context and its
        // CPU-local FPU owner through the reset and owner publication.
        let result = unsafe {
            with_current_cpu_pin(|cpu_pin| {
                let context = current_runtime_context(cpu_pin).map_err(runtime_status_error)?;
                // SAFETY: this is the currently executing architecture context;
                // IRQ exclusion prevents scheduler or interrupt re-entry.
                let architecture_context = &mut *context.inner.get();
                architecture_context.reset_user_fp_state();
                Ok(())
            })
        };
        ax_hal::asm::enable_irqs();
        result
    }
    #[cfg(not(all(target_arch = "x86_64", feature = "fp-simd", feature = "uspace")))]
    {
        Ok(())
    }
}

fn prepare_runtime_thread_switch<'switch>(
    pin: &'switch CpuPin<'_>,
    previous: &'static RuntimeContext,
    next: &'static RuntimeContext,
) -> (PreparedContextSwitch<'switch>, PreviousContextBinding) {
    // `prepare_context_switch` is the single production authority for current
    // publication, previous binding and next-unbound validation. Repeating
    // those checks here would reread the architecture current-thread register
    // and split one switch transaction across two facts.
    // SAFETY: the scheduler baton pins this CPU, and both runtime contexts stay
    // live through the raw switch and incoming tail.
    unsafe { ax_hal::percpu::prepare_context_switch(pin, previous.header(), next.header()) }
        .unwrap_or_else(|error| panic!("failed to prepare runtime context switch: {error}"))
}

#[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
pub(super) fn install_initial_fp_state(context: usize, fp_state: ax_hal::cpu::FpState) {
    let context = ptr::with_exposed_provenance_mut::<RuntimeContext>(context);
    // SAFETY: the context allocation was just created and has not been
    // published, so this construction path exclusively owns its FP snapshot.
    unsafe { (*(*context).inner.get()).fp_state = fp_state };
}

pub(super) unsafe fn switch_runtime_context(switch: ContextSwitch) {
    crate::guard::assert_scheduler_switch_baton();
    let previous_raw = switch.previous().into_raw();
    let next_raw = switch.next().into_raw();
    let previous = ptr::with_exposed_provenance_mut::<RuntimeContext>(previous_raw);
    let next = ptr::with_exposed_provenance_mut::<RuntimeContext>(next_raw);
    // SAFETY: the active scheduler baton keeps local IRQs disabled for
    // preparation, publication, and the naked switch tail.
    unsafe {
        with_current_cpu_pin(|pin| {
            // SAFETY: both handles stay live and are uniquely owned by the
            // committed scheduler switch plan.
            let previous_context = &*previous;
            let next_context = &*next;
            let previous_arch_context = &mut *previous_context.inner.get();
            let next_arch_context = &mut *next_context.inner.get();
            debug_assert_eq!(
                previous_arch_context.context_header(),
                Some(previous_context.header().as_non_null()),
                "outgoing architecture context retained a different current header"
            );
            debug_assert_eq!(
                next_arch_context.context_header(),
                Some(next_context.header().as_non_null()),
                "incoming architecture context retained a different current header"
            );
            // All fallible CPU binding validation and FP preparation precede
            // the irreversible baton transfer. Address-space activation is an
            // independent ax-runtime transaction and is never repeated here.
            let (prepared, previous_binding) =
                prepare_runtime_thread_switch(pin, previous_context, next_context);
            previous_arch_context.prepare_switch_to(next_arch_context);
            let tail = RuntimeSwitchTail {
                previous: previous_context.header().as_non_null(),
                binding: previous_binding,
            };
            next_context
                .stage_switch_tail(tail)
                .unwrap_or_else(|status| panic!("failed to stage runtime switch tail: {status:?}"));
            crate::guard::transfer_scheduler_switch_baton();
            // SAFETY: switch_to_prepared consumes the sole publication token
            // and enters naked assembly without another fallible or
            // ownership-sensitive Rust operation.
            previous_arch_context.switch_to_prepared(next_arch_context, prepared);
        })
    };
}

#[cfg(test)]
mod tests {
    use core::mem::MaybeUninit;

    use cpu_local::{CpuAreaPrefix, CpuAreaRef, CpuIndex};

    use super::*;

    #[test]
    fn switch_tail_consumes_the_exact_previous_binding_once() {
        std::thread::spawn(|| {
            let storage = Box::leak(Box::new(MaybeUninit::<CpuAreaPrefix>::uninit()));
            let base = storage.as_mut_ptr() as usize;
            storage.write(CpuAreaPrefix::initialize(CpuIndex::try_from(0).unwrap(), base).unwrap());
            // SAFETY: the leaked prefix is initialized and remains mapped for
            // this modeled CPU's complete process lifetime.
            let area = unsafe { CpuAreaRef::from_initialized_base(base) }.unwrap();
            // SAFETY: this fresh host thread owns its CPU-local register model.
            unsafe { cpu_local::install_cpu_area(area) }.unwrap();

            let previous = RuntimeContext::allocate(
                ax_hal::context::TaskContext::new(),
                StackHandle::NONE,
                InitialPreemptionState::Enabled,
            );
            let next = RuntimeContext::allocate(
                ax_hal::context::TaskContext::new(),
                StackHandle::NONE,
                InitialPreemptionState::Enabled,
            );

            // SAFETY: both leaked runtime contexts remain pinned for the
            // modeled switch, and this host thread cannot migrate.
            unsafe {
                cpu_local::with_cpu_pin(|pin| {
                    let previous = &*previous;
                    let next = &*next;
                    cpu_local::install_bootstrap_context(pin, previous.header()).unwrap();
                    let (prepared, binding) =
                        cpu_local::prepare_context_switch(pin, previous.header(), next.header())
                            .unwrap();
                    prepared.commit();

                    next.stage_switch_tail(RuntimeSwitchTail {
                        previous: previous.header().as_non_null(),
                        binding,
                    })
                    .unwrap();
                    next.finish_switch_tail();
                    assert!(!next.has_switch_tail());
                    assert_eq!(previous.header.cpu_area(), None);
                })
            }
            .unwrap();
        })
        .join()
        .expect("modeled CPU must complete the switch tail");
    }

    #[test]
    fn switch_prepare_reads_the_current_context_register_once() {
        std::thread::spawn(|| {
            let storage = Box::leak(Box::new(MaybeUninit::<CpuAreaPrefix>::uninit()));
            let base = storage.as_mut_ptr() as usize;
            storage.write(CpuAreaPrefix::initialize(CpuIndex::try_from(0).unwrap(), base).unwrap());
            // SAFETY: the leaked prefix is initialized and remains mapped for
            // this modeled CPU's complete process lifetime.
            let area = unsafe { CpuAreaRef::from_initialized_base(base) }.unwrap();
            // SAFETY: this fresh host thread owns its CPU-local register model.
            unsafe { cpu_local::install_cpu_area(area) }.unwrap();

            let previous = RuntimeContext::allocate(
                ax_hal::context::TaskContext::new(),
                StackHandle::NONE,
                InitialPreemptionState::Enabled,
            );
            let next = RuntimeContext::allocate(
                ax_hal::context::TaskContext::new(),
                StackHandle::NONE,
                InitialPreemptionState::Enabled,
            );

            // SAFETY: both leaked contexts remain pinned while the modeled CPU
            // validates and then rolls back this uncommitted switch.
            unsafe {
                cpu_local::with_cpu_pin(|pin| {
                    let previous = &*previous;
                    let next = &*next;
                    cpu_local::install_bootstrap_context(pin, previous.header()).unwrap();
                    cpu_local::host_test::reset_register_read_counts();

                    let (prepared, _binding) = prepare_runtime_thread_switch(pin, previous, next);
                    let reads = cpu_local::host_test::register_read_counts();
                    assert_eq!(
                        reads.current_context, 1,
                        "switch preparation must validate current publication exactly once"
                    );
                    drop(prepared);
                })
            }
            .unwrap();
        })
        .join()
        .expect("modeled CPU must complete switch preparation");
    }

    #[test]
    fn current_runtime_context_reuses_pinned_binding_validation() {
        std::thread::spawn(|| {
            let storage = Box::leak(Box::new(MaybeUninit::<CpuAreaPrefix>::uninit()));
            let base = storage.as_mut_ptr() as usize;
            storage.write(CpuAreaPrefix::initialize(CpuIndex::try_from(0).unwrap(), base).unwrap());
            // SAFETY: the leaked prefix is initialized and remains mapped for
            // this modeled CPU's complete process lifetime.
            let area = unsafe { CpuAreaRef::from_initialized_base(base) }.unwrap();
            // SAFETY: this fresh host thread owns its CPU-local register model.
            unsafe { cpu_local::install_cpu_area(area) }.unwrap();

            let current = RuntimeContext::allocate(
                ax_hal::context::TaskContext::new(),
                StackHandle::NONE,
                InitialPreemptionState::Enabled,
            );

            // SAFETY: the leaked runtime context remains pinned while this
            // host thread validates the modeled current publication.
            unsafe {
                cpu_local::with_cpu_pin(|pin| {
                    let expected = &*current;
                    cpu_local::install_bootstrap_context(pin, expected.header()).unwrap();
                    cpu_local::host_test::reset_register_read_counts();

                    let observed = current_runtime_context(pin).unwrap();
                    assert!(ptr::eq(observed, expected));
                    let reads = cpu_local::host_test::register_read_counts();
                    assert_eq!(reads.current_context, 1);
                    assert_eq!(
                        reads.binding_observations, 1,
                        "the pinned current lookup must not repeat its binding validation"
                    );
                })
            }
            .unwrap();
        })
        .join()
        .expect("modeled CPU must complete current lookup");
    }
}
