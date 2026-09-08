//! Context-switch plans, entry contracts and completion.

use alloc::sync::Arc;
use core::{marker::PhantomData, ptr::NonNull};

pub use crate::runtime::switch::dispatch::{
    schedule_current_cpu, schedule_current_cpu_from_irq_guard_exit,
    schedule_current_cpu_from_preempt_exit,
};
use crate::{
    runtime::{
        RuntimeStatus, TaskSystemHandle,
        context::{RuntimeIrqGuard, validate_task_context},
        cpu::{CurrentCpuOwnerHandles, RuntimeCpuId},
        resource::{AddressSpaceHandle, AddressSpaceMembarrierId, ExecutionContextHandle},
        switch::dispatch::complete_current_context_switch_tail,
        task_runtime,
    },
    thread::TaskError,
};

/// Completes switch tail and consumes the inherited IRQ guard on first entry.
///
/// Fresh context trampolines must invoke this before accessing thread-local
/// state, enabling interrupts, polling futures, or calling user/OS code.
/// Resumed contexts must not call it because their suspended scheduler guard
/// consumes the same baton when the architecture switch returns.
///
/// # Safety
///
/// The caller must be the first instruction sequence of a freshly switched-in
/// context. Exactly one scheduler IRQ guard must be inherited on this CPU, and
/// this function must be called exactly once for that context's first entry.
pub unsafe fn finish_initial_context_switch() -> Result<(), TaskError> {
    validate_task_context()?;
    let mut irq = RuntimeIrqGuard::enter();
    // SAFETY: this trampoline inherits the transferred scheduler baton and
    // the runtime IRQ guard retains its raw IRQ-off state through completion.
    unsafe { complete_current_context_switch_tail(&mut irq)? };
    drop(irq);
    task_runtime::finish_initial_context_switch();
    Ok(())
}
pub(crate) mod dispatch;

use crate::runtime::handle::opaque_handle;

opaque_handle!(
    /// Opaque pointer to the Arc-backed scheduler core of the current thread.
    ///
    /// This value is useful only as part of a runtime-provided
    /// [`CurrentThreadPublication`]. The scheduler may acquire a strong handle
    /// from it only while a preemption pin proves that the published thread is
    /// still current and therefore retains its owner-side strong reference.
    CurrentThreadOwnerHandle,
    "runtime::switch"
);

/// Move-only runtime transaction for one committed scheduler switch.
///
/// ax-task constructs this value only after the scheduler has committed two
/// distinct live endpoints and released its internal locks. The execution
/// contexts and logical address spaces travel through one runtime call, so a
/// provider cannot activate an `mm` and then fail before preparing the matching
/// architecture context. Consuming the transaction prevents replay.
#[derive(Debug, Eq, PartialEq)]
#[repr(C)]
pub struct RuntimeSwitchPlan {
    previous_context: ExecutionContextHandle,
    previous_address_space: AddressSpaceHandle,
    next_context: ExecutionContextHandle,
    next_address_space: AddressSpaceHandle,
    #[cfg(feature = "qperf-metrics")]
    qperf_prepare_started_ns: u64,
}

impl RuntimeSwitchPlan {
    pub(crate) fn new(
        previous_context: ExecutionContextHandle,
        previous_address_space: AddressSpaceHandle,
        previous_address_space_identity: AddressSpaceMembarrierId,
        next_context: ExecutionContextHandle,
        next_address_space: AddressSpaceHandle,
        next_address_space_identity: AddressSpaceMembarrierId,
    ) -> Option<Self> {
        debug_assert_eq!(
            previous_address_space.is_none(),
            previous_address_space_identity.is_none(),
        );
        debug_assert_eq!(
            next_address_space.is_none(),
            next_address_space_identity.is_none(),
        );
        debug_assert!(
            previous_address_space != next_address_space
                || previous_address_space_identity == next_address_space_identity,
        );
        if previous_context.is_none() || next_context.is_none() || previous_context == next_context
        {
            None
        } else {
            let same_address_space = !previous_address_space_identity.is_none()
                && previous_address_space_identity == next_address_space_identity;
            Some(Self {
                previous_context,
                previous_address_space: if same_address_space {
                    next_address_space
                } else {
                    previous_address_space
                },
                next_context,
                next_address_space,
                #[cfg(feature = "qperf-metrics")]
                qperf_prepare_started_ns: 0,
            })
        }
    }

    /// Returns the outgoing runtime context.
    pub const fn previous_context(&self) -> ExecutionContextHandle {
        self.previous_context
    }

    /// Returns the outgoing logical address space, canonicalized to the
    /// incoming live token when both endpoints select the same `mm`.
    pub const fn previous_address_space(&self) -> AddressSpaceHandle {
        self.previous_address_space
    }

    /// Returns the incoming runtime context.
    pub const fn next_context(&self) -> ExecutionContextHandle {
        self.next_context
    }

    /// Returns the incoming scheduler-selected logical address space.
    pub const fn next_address_space(&self) -> AddressSpaceHandle {
        self.next_address_space
    }

    /// Returns whether both scheduler endpoints select the same logical `mm`.
    pub const fn same_address_space(&self) -> bool {
        !self.next_address_space.is_none()
            && self.previous_address_space.into_raw() == self.next_address_space.into_raw()
    }

    #[cfg(feature = "qperf-metrics")]
    pub(crate) fn set_qperf_prepare_started_ns(&mut self, started_ns: u64) {
        self.qperf_prepare_started_ns = started_ns;
    }

    #[doc(hidden)]
    #[cfg(feature = "qperf-metrics")]
    pub const fn qperf_prepare_started_ns(&self) -> u64 {
        self.qperf_prepare_started_ns
    }
}

/// Immutable scheduler snapshot of one thread's runtime switch bindings.
///
/// Linux keeps the architecture context and `mm` selected by the rq transition
/// reachable without taking a second task lock after `pick_next_task()`.  The
/// ax-task owner rq follows the same rule: task-control code republishes this
/// value whenever the binding changes, and the switch plan consumes only the
/// rq-owned snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ThreadRuntimeBinding {
    context: ExecutionContextHandle,
    address_space: AddressSpaceHandle,
}

impl ThreadRuntimeBinding {
    pub(crate) const fn new(
        context: ExecutionContextHandle,
        address_space: AddressSpaceHandle,
    ) -> Self {
        Self {
            context,
            address_space,
        }
    }

    pub(crate) const fn context(self) -> ExecutionContextHandle {
        self.context
    }

    pub(crate) const fn address_space(self) -> AddressSpaceHandle {
        self.address_space
    }
}

/// Scheduler entry whose context constraints the runtime must validate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RuntimeScheduleOrigin {
    /// A thread is about to publish or commit a blocking state.
    Block   = 0,
    /// A thread voluntarily yields its remaining service.
    Yield   = 1,
    /// A thread permanently exits.
    Exit    = 2,
    /// A sticky preemption request is serviced from task context.
    Preempt = 3,
}

/// Typed source of one scheduler-frame baton.
///
/// The runtime uses this value to validate and atomically transform its
/// CPU-local preemption state. In particular, preemption-guard exits retain
/// their final lock depth until the scheduler frame owns the baton, closing the
/// interrupt window between enabling preemption and entering the scheduler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RuntimeSchedulerEntry {
    /// Ordinary task context with IRQs enabled and no preemption guard.
    Task         = 0,
    /// Final task-context preemption guard exit with IRQs disabled.
    ///
    /// The runtime retains the final preemption depth while it disables raw
    /// IRQs, then atomically converts that depth into the scheduler baton.
    PreemptExit  = 1,
    /// Final IRQ-return preemption guard exit with IRQs still disabled.
    IrqReturn    = 2,
    /// Final task-context IRQ publication guard exit with IRQs disabled.
    ///
    /// The runtime retains the final IRQ-guard depth after publishing local
    /// scheduler work, then atomically converts that depth into the scheduler
    /// baton. This is the local counterpart of a remote scheduler IPI.
    IrqGuardExit = 3,
    /// A repeated IRQ-return pass after the previous scheduler frame fully
    /// released its switch baton.
    ///
    /// The caller enters with hardware IRQs disabled and preemption depth zero.
    /// Before claiming the fresh scheduler baton, the runtime establishes one
    /// ordinary preemption depth, opens the Linux-style IRQ window, disables
    /// IRQs again, and atomically converts that depth into the scheduler baton.
    IrqReturnContinuation = 4,
}

/// Raw IRQ state expected by the suspended scheduler continuation.
///
/// This is continuation-local rather than CPU-local: a context resumed by an
/// IRQ-return schedule may itself have been suspended in an ordinary task
/// schedule, and vice versa.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RuntimeSchedulerReturn {
    /// Resume ordinary task context with local IRQs enabled.
    Task      = 0,
    /// Resume the architecture trap epilogue with local IRQs disabled.
    IrqReturn = 1,
}

/// Versioned generation-bearing thread identity for runtime context binding.
///
/// The explicit fields keep the scheduler's private integer encoding out of OS
/// runtime implementations while remaining a value-only trait-FFI type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct ThreadIdentityV1 {
    /// Task-system registry slot.
    pub slot: u32,
    /// Non-zero reuse generation for `slot`.
    pub generation: u32,
}

/// Immutable scheduler publication owned by one runtime execution context.
///
/// This is the Rust equivalent of Linux's architecture-selected `current`
/// pointer: the identity and its Arc-backed owner address are installed once
/// before the context can run, then remain immutable across preemption and
/// migration. The owner address is never a standalone weak or strong handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct CurrentThreadPublication {
    identity: ThreadIdentityV1,
    owner: CurrentThreadOwnerHandle,
}

/// Atomic runtime result of claiming one scheduler frame.
///
/// A successful result carries every immutable capability selected under the
/// same IRQ-off CPU pin: task system and owner CPU endpoints. Current-thread
/// identity is read only by operations that need it, while this frame keeps
/// the execution context pinned; scheduler selection itself uses `rq->curr`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct RuntimeSchedulerFrameEnterResult {
    system: TaskSystemHandle,
    cpu: CurrentCpuOwnerHandles,
}

impl RuntimeSchedulerFrameEnterResult {
    /// Creates a successful scheduler-frame capability snapshot.
    ///
    /// # Safety
    ///
    /// `system` must be non-empty. All handles must describe the CPU pinned by
    /// the scheduler baton that was claimed in the same runtime transaction.
    pub const unsafe fn success(system: TaskSystemHandle, cpu: CurrentCpuOwnerHandles) -> Self {
        Self { system, cpu }
    }

    /// Creates an unsafe-context rejection without live capabilities.
    pub const fn failure() -> Self {
        Self {
            system: TaskSystemHandle::NONE,
            cpu: CurrentCpuOwnerHandles::NONE,
        }
    }

    /// Returns the runtime entry status.
    pub const fn status(self) -> RuntimeStatus {
        if self.system.is_none() {
            RuntimeStatus::UnsafeContext
        } else {
            RuntimeStatus::Success
        }
    }

    /// Returns the pinned task-system capability.
    pub const fn system(self) -> TaskSystemHandle {
        self.system
    }

    /// Returns the pinned owner-CPU capability.
    pub const fn cpu(self) -> CurrentCpuOwnerHandles {
        self.cpu
    }
}

/// Borrowed view of the scheduler-owned current-thread reference.
///
/// Unlike [`crate::thread::ThreadHandle`], this capability does not acquire an
/// external lifetime lease. It is confined to the current execution context;
/// the architecture publication and scheduler-owned `rq->curr` reference keep
/// the pointed-to core alive until the synchronous operation returns.
pub(crate) struct CurrentThreadRef {
    identity: crate::thread::ThreadId,
    core: NonNull<crate::thread::ThreadCore>,
    _not_send: PhantomData<*mut ()>,
}

impl CurrentThreadRef {
    pub(crate) const fn id(&self) -> crate::thread::ThreadId {
        self.identity
    }

    pub(crate) fn runtime_core(&self) -> &crate::thread::ThreadCore {
        // SAFETY: construction validates the current publication while the
        // scheduler retains its owner-side reference. The borrow cannot
        // outlive this non-Send capability.
        unsafe { self.core.as_ref() }
    }
}

impl CurrentThreadPublication {
    /// Sentinel returned by an unbound bootstrap execution context.
    pub const NONE: Self = Self {
        identity: ThreadIdentityV1::NONE,
        owner: CurrentThreadOwnerHandle::NONE,
    };

    /// Returns the generation-bearing scheduler identity.
    pub const fn identity(self) -> ThreadIdentityV1 {
        self.identity
    }

    /// Returns the opaque current-owner address.
    pub const fn owner(self) -> CurrentThreadOwnerHandle {
        self.owner
    }

    pub(crate) fn from_core(
        identity: crate::thread::ThreadId,
        core: &Arc<crate::thread::ThreadCore>,
    ) -> Self {
        let owner = Arc::as_ptr(core).expose_provenance();
        // SAFETY: `core` supplies the live Arc allocation. Consumers may use
        // this address only through the checked current-publication accessors
        // while the matching runtime context remains the executing task.
        let owner = unsafe { CurrentThreadOwnerHandle::from_raw(owner) };
        Self {
            identity: ThreadIdentityV1::new(identity.slot(), identity.generation()),
            owner,
        }
    }

    /// Borrows the scheduler-owned current reference without creating an
    /// external handle or changing any Arc count.
    ///
    /// # Safety
    ///
    /// The runtime must have copied this publication from the architecture-
    /// selected current context. The caller must use the returned capability
    /// only in the synchronous operation of that context and must not exit the
    /// thread while it remains live.
    pub(crate) unsafe fn borrow_current(
        self,
    ) -> Result<CurrentThreadRef, crate::thread::TaskError> {
        if !self.identity.is_bound() {
            return Err(crate::thread::TaskError::NoRunnableThread);
        }
        let core = NonNull::new(core::ptr::with_exposed_provenance_mut::<
            crate::thread::ThreadCore,
        >(self.owner.into_raw()))
        .ok_or(crate::thread::TaskError::InvalidRuntimeHandle)?;
        let identity =
            crate::thread::ThreadId::from_parts(self.identity.slot, self.identity.generation);
        let current = CurrentThreadRef {
            identity,
            core,
            _not_send: PhantomData,
        };
        if current.runtime_core().id() != identity {
            return Err(crate::thread::TaskError::InvalidRuntimeHandle);
        }
        Ok(current)
    }

    /// Acquires an ordinary external scheduler handle from the current
    /// context's owner publication.
    ///
    /// # Safety
    ///
    /// The runtime must have copied the publication from the architecture-
    /// selected current task context. The scheduler must retain that thread's
    /// owner-side `Arc` while the caller can execute or resume this operation.
    pub(crate) unsafe fn acquire_handle(
        self,
    ) -> Result<crate::thread::ThreadHandle, crate::thread::TaskError> {
        let core = unsafe {
            // SAFETY: this method has the same current-context ownership
            // contract as `acquire_scheduler_core`.
            self.acquire_scheduler_core()?
        };
        Ok(crate::thread::ThreadHandle::from_core(core))
    }

    /// Acquires a scheduler-internal strong reference without publishing an
    /// external management lifetime lease.
    ///
    /// # Safety
    ///
    /// The runtime must have copied the publication from the architecture-
    /// selected current task context. The scheduler must retain that thread's
    /// owner-side `Arc` while the caller can execute or resume this operation.
    pub(crate) unsafe fn acquire_scheduler_core(
        self,
    ) -> Result<Arc<crate::thread::ThreadCore>, crate::thread::TaskError> {
        if !self.identity.is_bound() {
            return Err(crate::thread::TaskError::NoRunnableThread);
        }
        if self.owner.is_none() {
            return Err(crate::thread::TaskError::InvalidRuntimeHandle);
        }
        let core =
            core::ptr::with_exposed_provenance::<crate::thread::ThreadCore>(self.owner.into_raw());
        // SAFETY: the current-task publication contract proves that an owner-
        // side strong reference remains live across preemption and migration.
        unsafe { Arc::increment_strong_count(core) };
        // SAFETY: the increment above created exactly one strong reference for
        // this reconstruction.
        let core = unsafe { Arc::from_raw(core) };
        let expected =
            crate::thread::ThreadId::from_parts(self.identity.slot, self.identity.generation);
        if core.id() != expected {
            return Err(crate::thread::TaskError::InvalidRuntimeHandle);
        }
        Ok(core)
    }
}

impl ThreadIdentityV1 {
    /// Sentinel returned before a runtime context is bound to a scheduler thread.
    pub const NONE: Self = Self {
        slot: 0,
        generation: 0,
    };

    /// Creates a runtime identity from its explicit generation-bearing parts.
    pub const fn new(slot: u32, generation: u32) -> Self {
        Self { slot, generation }
    }

    /// Returns whether this value names a published scheduler generation.
    pub const fn is_bound(self) -> bool {
        self.generation != 0
    }
}

/// Immutable association between one runtime context and scheduler ownership.
///
/// Contexts are created before the scheduler allocates a generation-bearing
/// thread ID. The scheduler submits this value exactly once after ID allocation
/// and before the thread can become runnable. The publication keeps only a
/// pointer-sized owner address; it does not transfer an Arc or external reaper
/// lease across the trait-FFI boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct ContextThreadBinding {
    /// Live runtime-owned execution context to bind.
    pub context: ExecutionContextHandle,
    /// Immutable current-thread publication for this execution context.
    pub publication: CurrentThreadPublication,
}

/// Allocation-free scheduler switch diagnostic record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct SchedSwitchRecord {
    /// Logical CPU performing the switch.
    pub cpu: RuntimeCpuId,
    /// Previous generation-based thread identifier encoded as a scalar.
    pub previous_thread: u64,
    /// Next generation-based thread identifier encoded as a scalar.
    pub next_thread: u64,
    /// Monotonic switch timestamp.
    pub timestamp_ns: u64,
    /// Policy-specific reason code defined by ax-task.
    pub reason: u32,
}

pub use crate::sched::system::{
    ScheduleDecision, SchedulerOutcome, SwitchInCompletion, YieldOutcome,
};

#[cfg(test)]
mod switch_plan_tests {
    use super::*;

    #[test]
    fn runtime_switch_plan_keeps_context_and_logical_mm_in_one_transaction() {
        // SAFETY: opaque values are never dereferenced by this value-only
        // contract test.
        let previous_context = unsafe { ExecutionContextHandle::from_raw(0x1000) };
        // SAFETY: see above.
        let next_context = unsafe { ExecutionContextHandle::from_raw(0x2000) };
        // SAFETY: see above.
        let previous_mm = unsafe { AddressSpaceHandle::from_raw(0x3000) };
        // SAFETY: see above.
        let next_mm = unsafe { AddressSpaceHandle::from_raw(0x4000) };
        // SAFETY: opaque values are compared only and never dereferenced.
        let previous_mm_identity = unsafe { AddressSpaceMembarrierId::from_raw(0x5000) };
        // SAFETY: see above.
        let next_mm_identity = unsafe { AddressSpaceMembarrierId::from_raw(0x6000) };
        let plan = RuntimeSwitchPlan::new(
            previous_context,
            previous_mm,
            previous_mm_identity,
            next_context,
            next_mm,
            next_mm_identity,
        )
        .expect("two distinct live contexts must form one runtime switch plan");

        assert_eq!(plan.previous_context(), previous_context);
        assert_eq!(plan.previous_address_space(), previous_mm);
        assert_eq!(plan.next_context(), next_context);
        assert_eq!(plan.next_address_space(), next_mm);
    }
}
