use alloc::boxed::Box;
use core::{
    cell::UnsafeCell,
    mem::offset_of,
    pin::Pin,
    ptr::{self, NonNull},
};

use ax_hal::percpu::{
    CpuPin, CurrentContext, CurrentThreadHeader, PreparedThreadSwitch, PreviousThreadBinding,
    RuntimeThreadCookie,
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
    previous: NonNull<CurrentThreadHeader>,
    binding: PreviousThreadBinding,
}

/// Runtime-owned architecture context and its pinned scheduler identity.
///
/// The header stays at offset zero so a current-thread publication can be
/// checked against its context handle without a second registry or per-CPU
/// pointer. `switch_tail` is written only while this context is off-CPU and is
/// consumed exactly once after it becomes current with local IRQs disabled.
#[repr(C)]
struct RuntimeContext {
    header: CurrentThreadHeader,
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
        let identity = CurrentContext::from_raw(inner.get().expose_provenance())
            .expect("an architecture context allocation must have a non-zero identity");
        let header = match preemption {
            InitialPreemptionState::Enabled => CurrentThreadHeader::new(identity),
            InitialPreemptionState::BootstrapDisabled => {
                CurrentThreadHeader::new_bootstrap(identity)
            }
        };
        Box::into_raw(Box::new(Self {
            header,
            publication: UnsafeCell::new(CurrentThreadPublication::NONE),
            inner,
            stack,
            switch_tail: UnsafeCell::new(None),
        }))
    }

    fn header(&self) -> Pin<&CurrentThreadHeader> {
        // SAFETY: every RuntimeContext is constructed in a Box and is never
        // moved before destruction after its header is no longer published.
        unsafe { Pin::new_unchecked(&self.header) }
    }

    fn context_identity(&self) -> CurrentContext {
        self.header
            .current_context()
            .expect("runtime task header must retain its context identity")
    }

    fn architecture_context_identity(&self) -> usize {
        self.inner.get().expose_provenance()
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
            .as_mut()
            .expect("incoming runtime context is missing its switch tail");
        // SAFETY: the outgoing header stays pinned and unreclaimable through
        // the scheduler `on_cpu` handoff; this tail owns its exact epoch.
        let previous = unsafe { Pin::new_unchecked(tail.previous.as_ref()) };
        unsafe { tail.binding.finish(previous) }
            .expect("runtime switch tail did not own the exact previous CPU binding");
        let _ = slot.take();
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
    if context.context_identity().as_usize() != context.architecture_context_identity() {
        return Err(RuntimeStatus::InvalidHandle);
    }
    Ok(context)
}

fn current_runtime_context(cpu_pin: &CpuPin) -> Result<&'static RuntimeContext, RuntimeStatus> {
    let current = ax_hal::percpu::current_thread(cpu_pin)
        .map_err(|_| RuntimeStatus::InvalidHandle)?
        .as_ptr()
        .expose_provenance();
    let header = ptr::with_exposed_provenance::<CurrentThreadHeader>(current);
    // SAFETY: the CPU runtime slot may publish only a pinned header whose live
    // CPU binding matches this prefix. The supplied pin covers the load and
    // every validation below.
    let header = unsafe { &*header };
    // RuntimeContext is `repr(C)` and the pinned header is its offset-zero
    // owner identity. The independently allocated architecture context keeps
    // ContextIdentity free of self-referential outer pointers.
    let context = unsafe { &*ptr::from_ref(header).cast::<RuntimeContext>() };
    if !ptr::eq(context.header().get_ref(), header)
        || context.context_identity().as_usize() != context.architecture_context_identity()
    {
        return Err(RuntimeStatus::InvalidHandle);
    }
    if header.current_context().is_none() {
        return Err(RuntimeStatus::NotInitialized);
    }
    if header.cpu_area() != Some(cpu_pin.area()) {
        return Err(RuntimeStatus::InvalidHandle);
    }
    Ok(context)
}

pub(super) fn bind_bootstrap_runtime_context(
    cpu_pin: &CpuPin,
    handle: ExecutionContextHandle,
    kernel_tls: usize,
) -> Result<(), TaskError> {
    let boot_thread =
        ax_hal::percpu::current_thread(cpu_pin).map_err(|_| TaskError::InvalidConfiguration)?;
    // The permanent boot header has no runtime execution-context identity.
    if unsafe { boot_thread.as_ref() }.current_context().is_some() {
        return Err(TaskError::InvalidConfiguration);
    }
    let context = runtime_context(handle).map_err(runtime_status_error)?;
    // SAFETY: the CPU is still offline and trap-free, while the scheduler
    // record keeps this pinned header alive until its switch tail withdraws it.
    unsafe { ax_hal::percpu::install_bootstrap_thread(cpu_pin, context.header()) }
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

pub(super) fn finish_runtime_context_switch_tail() {
    // SAFETY: TaskSystem invokes this with the scheduler baton and local IRQs
    // disabled immediately after entering the incoming context.
    unsafe {
        with_current_cpu_pin(|cpu_pin| {
            let current = current_runtime_context(cpu_pin)
                .expect("incoming scheduler context is not runtime-owned");
            // SAFETY: the incoming context exclusively owns its staged one-shot tail.
            current.finish_switch_tail();
        })
    }
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
    if context.header.runtime_thread_cookie().is_some() {
        return RuntimeStatus::InvalidArgument;
    }
    // Context binding runs exactly once before scheduler publication, so this
    // is the sole write to the pinned current-thread publication.
    unsafe { *context.publication.get() = binding.publication };
    let publication = context.publication.get().expose_provenance();
    let Some(publication) = RuntimeThreadCookie::new(publication) else {
        return RuntimeStatus::InvalidHandle;
    };
    if context
        .header
        .bind_runtime_thread_cookie(publication)
        .is_err()
    {
        return RuntimeStatus::InvalidArgument;
    }
    // Scheduler construction invokes this exactly once before the context can
    // enter a run queue. The bootstrap placeholder is likewise not consumed by
    // assembly until its first switch-out.
    unsafe { &mut *context.inner.get() }.set_current_header(context.header().as_non_null());
    RuntimeStatus::Success
}

/// Reads the immutable scheduler publication owned by the current task context.
#[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
pub(super) fn scheduler_current_thread_publication() -> CurrentThreadPublication {
    ax_hal::percpu::with_scheduler_current_thread(|header| {
        let Some(publication) = header.runtime_thread_cookie() else {
            // Only the permanent pre-scheduler bootstrap header has no runtime
            // cookie. A runtime context receives its immutable cookie before it
            // can enter any runqueue.
            return CurrentThreadPublication::NONE;
        };
        let context = header as *const CurrentThreadHeader as *const RuntimeContext;
        // SAFETY: `RuntimeContext::header` is at offset zero. The current-task
        // callback retains this pinned context even if preemption migrates it.
        let expected = unsafe { (*context).publication.get() };
        assert_eq!(
            publication.get(),
            expected.expose_provenance(),
            "current-thread cookie must identify its owning runtime publication"
        );
        // SAFETY: binding initialized this immutable field before the context
        // entered any run queue, and the current task retains its owner.
        unsafe { *expected }
    })
    .unwrap_or_else(|error| panic!("scheduler current-thread register is invalid: {error}"))
}

fn prepare_runtime_thread_switch<'switch>(
    pin: &'switch CpuPin<'_>,
    previous: &'static RuntimeContext,
    next: &'static RuntimeContext,
) -> (PreparedThreadSwitch<'switch>, PreviousThreadBinding) {
    // `prepare_thread_switch` is the single production authority for current
    // publication, previous binding and next-unbound validation. Repeating
    // those checks here would reread the architecture current-thread register
    // and split one switch transaction across two facts.
    // SAFETY: the scheduler baton pins this CPU, and both runtime contexts stay
    // live through the raw switch and incoming tail.
    unsafe { ax_hal::percpu::prepare_thread_switch(pin, previous.header(), next.header()) }
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
                previous_arch_context.current_header(),
                Some(previous_context.header().as_non_null()),
                "outgoing architecture context retained a different current header"
            );
            debug_assert_eq!(
                next_arch_context.current_header(),
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
                    cpu_local::install_bootstrap_thread(pin, previous.header()).unwrap();
                    let (prepared, binding) =
                        cpu_local::prepare_thread_switch(pin, previous.header(), next.header())
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
    fn switch_prepare_reads_the_current_thread_register_once() {
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
                    cpu_local::install_bootstrap_thread(pin, previous.header()).unwrap();
                    cpu_local::host_test::reset_register_read_counts();

                    let (prepared, _binding) = prepare_runtime_thread_switch(pin, previous, next);
                    let reads = cpu_local::host_test::register_read_counts();
                    assert_eq!(
                        reads.current_thread, 1,
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
}
