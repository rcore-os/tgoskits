use alloc::boxed::Box;
use core::{
    cell::UnsafeCell,
    mem::offset_of,
    pin::Pin,
    ptr::{self, NonNull},
    sync::atomic::{AtomicU64, Ordering},
};

use ax_hal::percpu::{CpuPin, CurrentContext, CurrentThreadHeader, PreviousThreadBinding};
use ax_task::{
    TaskError,
    runtime::{
        AddressSpaceHandle, ContextThreadBinding, ExecutionContextHandle, KernelContextRequest,
        RuntimeHandleResult, RuntimeStatus, StackHandle, UserContextRequest,
    },
};

use super::{
    resources::{RuntimeStack, runtime_tls_pointer},
    runtime_status_error, with_current_cpu_pin,
};

/// Opaque runtime token for one user page-table root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskAddressSpace(pub(super) AddressSpaceHandle);

impl TaskAddressSpace {
    /// Creates a token from a non-zero physical page-table root address.
    pub fn from_page_table_root(root: usize) -> Result<Self, TaskError> {
        if root == 0 {
            Err(TaskError::InvalidRuntimeHandle)
        } else {
            // SAFETY: the non-zero root is the runtime's address-space token;
            // the OS that creates this wrapper owns the corresponding page
            // tables for every scheduler record that retains the token.
            Ok(Self(unsafe { AddressSpaceHandle::from_raw(root) }))
        }
    }
}

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

/// Replaces the current user context's page-table root and installs it now.
///
/// This operation is valid only for the running thread during an `exec`-style
/// address-space replacement.
pub fn switch_current_page_table(root: usize) -> Result<(), TaskError> {
    if root == 0 {
        return Err(TaskError::InvalidRuntimeHandle);
    }
    #[cfg(feature = "uspace")]
    {
        let _irq = ax_kernel_guard::IrqSave::new();
        let root = ax_memory_addr::PhysAddr::from(root);
        // SAFETY: the exec caller transfers a live process page-table root;
        // the scheduler retains only its opaque identity while the process MM
        // remains the allocation owner.
        let address_space = unsafe { AddressSpaceHandle::from_raw(root.as_usize()) };
        // Keep the scheduler endpoint, architecture context, and hardware root
        // coherent across exec. A later switch must restore this new root
        // instead of the address space that existed when the context was built.
        let _old_address_space = ax_task::replace_current_address_space(address_space)?;
        set_current_context_page_table_root(root)?;
        let status = install_runtime_address_space(address_space);
        if status == RuntimeStatus::Success {
            Ok(())
        } else {
            Err(runtime_status_error(status))
        }
    }
    #[cfg(not(feature = "uspace"))]
    {
        let _ = root;
        Err(TaskError::RuntimeFailure(RuntimeStatus::Unsupported as u32))
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
    inner: Box<UnsafeCell<ax_hal::context::TaskContext>>,
    thread_identity: AtomicU64,
    stack: StackHandle,
    switch_tail: UnsafeCell<Option<RuntimeSwitchTail>>,
}

const _: () = assert!(offset_of!(RuntimeContext, header) == 0);

impl RuntimeContext {
    fn allocate(inner: ax_hal::context::TaskContext, stack: StackHandle) -> *mut RuntimeContext {
        let inner = Box::new(UnsafeCell::new(inner));
        let identity = CurrentContext::from_raw(inner.get().expose_provenance())
            .expect("an architecture context allocation must have a non-zero identity");
        Box::into_raw(Box::new(Self {
            header: CurrentThreadHeader::new(identity),
            inner,
            thread_identity: AtomicU64::new(0),
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

    unsafe fn finish_switch_tail(&self) -> RuntimeStatus {
        // SAFETY: the current incoming continuation owns this slot with local
        // IRQs disabled and completes the one-shot previous-binding token.
        let slot = unsafe { &mut *self.switch_tail.get() };
        let Some(tail) = slot.as_mut() else {
            return RuntimeStatus::InvalidHandle;
        };
        // SAFETY: the outgoing header stays pinned and unreclaimable through
        // the scheduler `on_cpu` handoff; this tail owns its exact epoch.
        let previous = unsafe { Pin::new_unchecked(tail.previous.as_ref()) };
        match unsafe { tail.binding.finish(previous) } {
            Ok(()) => {
                // The exact binding epoch is now withdrawn. Only this success
                // consumes the staged transaction and permits scheduler tail.
                let _ = slot.take();
                RuntimeStatus::Success
            }
            Err(_) => RuntimeStatus::InvalidHandle,
        }
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

pub(super) fn finish_runtime_context_switch_tail() -> RuntimeStatus {
    // SAFETY: TaskSystem invokes this with the scheduler baton and local IRQs
    // disabled immediately after entering the incoming context.
    unsafe {
        with_current_cpu_pin(|cpu_pin| {
            let Ok(current) = current_runtime_context(cpu_pin) else {
                return RuntimeStatus::InvalidHandle;
            };
            // SAFETY: the incoming context exclusively owns its staged one-shot tail.
            current.finish_switch_tail()
        })
    }
}

pub(super) fn create_runtime_context(request: KernelContextRequest) -> RuntimeHandleResult {
    create_runtime_context_parts(
        request.stack,
        request.entry,
        request.tls,
        request.address_space,
    )
}

pub(super) fn create_user_runtime_context(request: UserContextRequest) -> RuntimeHandleResult {
    if request.address_space.is_none() {
        return RuntimeHandleResult::failure(RuntimeStatus::InvalidHandle);
    }
    create_runtime_context_parts(
        request.stack,
        request.entry,
        request.tls,
        request.address_space,
    )
}

fn create_runtime_context_parts(
    stack_handle: StackHandle,
    entry: ax_task::runtime::KernelEntry,
    tls_handle: ax_task::runtime::TlsHandle,
    address_space: AddressSpaceHandle,
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
    #[cfg(not(feature = "uspace"))]
    if !address_space.is_none() {
        return RuntimeHandleResult::failure(RuntimeStatus::Unsupported);
    }
    #[cfg(feature = "uspace")]
    context.set_page_table_root(ax_memory_addr::PhysAddr::from(resolve_address_space_root(
        address_space,
    )));
    RuntimeHandleResult::success(
        RuntimeContext::allocate(context, stack_handle).expose_provenance(),
    )
}

pub(super) fn create_bootstrap_context() -> ExecutionContextHandle {
    let context = ax_hal::context::TaskContext::new();
    let context = RuntimeContext::allocate(context, StackHandle::NONE);
    // SAFETY: Box::into_raw yields a non-null uniquely owned RuntimeContext
    // that stays live until destroy_runtime_context consumes the handle.
    unsafe { ExecutionContextHandle::from_raw(context.expose_provenance()) }
}

#[cfg(feature = "uspace")]
fn resolve_address_space_root(address_space: AddressSpaceHandle) -> usize {
    if !address_space.is_none() {
        return address_space.into_raw();
    }
    if cfg!(any(target_arch = "x86_64", target_arch = "riscv64")) {
        // SAFETY: callers retain the scheduler baton or an IRQ guard, and every
        // CPU publishes this immutable root before coming online.
        unsafe { with_current_cpu_pin(super::bootstrap::kernel_address_space_root) }
    } else {
        // AArch64 and LoongArch have distinct kernel roots; zero disables the
        // lower/user translation root without disturbing kernel mappings.
        0
    }
}

#[cfg(feature = "uspace")]
fn set_current_context_page_table_root(root: ax_memory_addr::PhysAddr) -> Result<(), TaskError> {
    // SAFETY: the exec path holds local IRQ exclusion, so the current context
    // cannot migrate or overlap a scheduler handoff while its saved root is
    // replaced.
    unsafe {
        with_current_cpu_pin(|cpu_pin| {
            let context = current_runtime_context(cpu_pin).map_err(runtime_status_error)?;
            // SAFETY: the published current context exclusively owns its
            // architecture state until the next scheduler switch.
            (&mut *context.inner.get()).set_page_table_root(root);
            Ok(())
        })
    }
}

pub(super) fn install_runtime_address_space(address_space: AddressSpaceHandle) -> RuntimeStatus {
    #[cfg(feature = "uspace")]
    {
        let root = ax_memory_addr::PhysAddr::from(resolve_address_space_root(address_space));
        if ax_hal::asm::read_user_page_table() != root {
            // SAFETY: both scheduler switch and exec replacement invoke this
            // with local IRQs disabled after committing the selected address
            // space to the current scheduler endpoint.
            unsafe { ax_hal::asm::write_user_page_table(root) };
            ax_hal::asm::flush_tlb(None);
        }
        RuntimeStatus::Success
    }
    #[cfg(not(feature = "uspace"))]
    {
        if address_space.is_none() {
            RuntimeStatus::Success
        } else {
            RuntimeStatus::Unsupported
        }
    }
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
    if binding.identity.generation == 0 {
        return RuntimeStatus::InvalidArgument;
    }
    let thread_identity =
        ((binding.identity.generation as u64) << 32) | binding.identity.slot as u64;
    let Ok(context) = runtime_context(binding.context) else {
        return RuntimeStatus::InvalidHandle;
    };
    if context
        .thread_identity
        .compare_exchange(0, thread_identity, Ordering::AcqRel, Ordering::Acquire)
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

#[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
pub(super) fn install_initial_fp_state(context: usize, fp_state: ax_hal::cpu::FpState) {
    let context = ptr::with_exposed_provenance_mut::<RuntimeContext>(context);
    // SAFETY: the context allocation was just created and has not been
    // published, so this construction path exclusively owns its FP snapshot.
    unsafe { (*(*context).inner.get()).fp_state = fp_state };
}

pub(super) unsafe fn switch_runtime_context(
    previous: ExecutionContextHandle,
    next: ExecutionContextHandle,
) {
    assert!(!previous.is_none(), "previous task context is missing");
    assert!(!next.is_none(), "next task context is missing");
    assert_ne!(
        previous, next,
        "raw context switch requires distinct contexts"
    );
    crate::guard::assert_scheduler_switch_baton();
    let previous_raw = previous.into_raw();
    let next_raw = next.into_raw();
    let previous = ptr::with_exposed_provenance_mut::<RuntimeContext>(previous_raw);
    let next = ptr::with_exposed_provenance_mut::<RuntimeContext>(next_raw);
    // SAFETY: the active scheduler baton keeps local IRQs disabled for
    // preparation, publication, and the naked switch tail.
    unsafe {
        with_current_cpu_pin(|pin| {
            let published_previous = current_runtime_context(pin)
                .unwrap_or_else(|status| panic!("current runtime context is invalid: {status:?}"));
            assert!(
                ptr::eq(published_previous, previous),
                "scheduler previous context differs from the pinned current header"
            );
            // SAFETY: both handles stay live and are uniquely owned by the
            // committed scheduler switch plan.
            let previous_context = &*previous;
            let next_context = &*next;
            let previous_arch_context = &mut *previous_context.inner.get();
            let next_arch_context = &*next_context.inner.get();
            assert_eq!(
                previous_arch_context.current_header(),
                Some(previous_context.header().as_non_null()),
                "outgoing architecture context retained a different current header"
            );
            assert_eq!(
                next_arch_context.current_header(),
                Some(next_context.header().as_non_null()),
                "incoming architecture context retained a different current header"
            );
            assert!(
                previous_context.header.cpu_area() == Some(pin.area()),
                "scheduler previous context is not bound to this CPU"
            );
            assert!(
                next_context.header.cpu_area().is_none(),
                "scheduler next context is already CPU-bound"
            );
            assert!(
                !next_context.has_switch_tail(),
                "scheduler next context retained an unfinished switch tail"
            );

            // All fallible CPU binding validation and FP/address-space
            // preparation precede the irreversible baton transfer.
            let (prepared, previous_binding) = ax_hal::percpu::prepare_thread_switch(
                pin,
                previous_context.header(),
                next_context.header(),
            )
            .unwrap_or_else(|error| panic!("failed to prepare runtime context switch: {error}"));
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
    fn failed_switch_tail_preserves_the_binding_transaction_for_retry() {
        std::thread::spawn(|| {
            let storage = Box::leak(Box::new(MaybeUninit::<CpuAreaPrefix>::uninit()));
            let base = storage.as_mut_ptr() as usize;
            storage.write(CpuAreaPrefix::initialize(CpuIndex::try_from(0).unwrap(), base).unwrap());
            // SAFETY: the leaked prefix is initialized and remains mapped for
            // this modeled CPU's complete process lifetime.
            let area = unsafe { CpuAreaRef::from_initialized_base(base) }.unwrap();
            // SAFETY: this fresh host thread owns its CPU-local register model.
            unsafe { cpu_local::install_cpu_area(area) }.unwrap();

            let previous =
                RuntimeContext::allocate(ax_hal::context::TaskContext::new(), StackHandle::NONE);
            let next =
                RuntimeContext::allocate(ax_hal::context::TaskContext::new(), StackHandle::NONE);

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
                        // Inject a retryable validation failure without
                        // mutating the live previous binding.
                        previous: next.header().as_non_null(),
                        binding,
                    })
                    .unwrap();
                    assert_eq!(next.finish_switch_tail(), RuntimeStatus::InvalidHandle);
                    assert!(
                        next.has_switch_tail(),
                        "a failed runtime tail must retain its binding token"
                    );

                    (*next.switch_tail.get())
                        .as_mut()
                        .expect("failed tail must remain staged")
                        .previous = previous.header().as_non_null();
                    assert_eq!(next.finish_switch_tail(), RuntimeStatus::Success);
                    assert!(!next.has_switch_tail());
                    assert_eq!(previous.header.cpu_area(), None);
                })
            }
            .unwrap();
        })
        .join()
        .expect("modeled CPU must complete the retry");
    }
}
