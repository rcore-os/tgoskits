use core::{
    mem::{offset_of, size_of},
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
};

use crate::{CpuAreaPrefix, CpuAreaRef, CpuIndex, ThreadSwitchError};

/// Stable opaque identity of one runtime-owned execution context.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct CurrentContext(usize);

impl CurrentContext {
    /// Converts a non-null opaque execution-context handle.
    pub const fn from_raw(raw: usize) -> Option<Self> {
        if raw == 0 { None } else { Some(Self(raw)) }
    }

    /// Returns the opaque scalar representation.
    pub const fn as_usize(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CpuBindingEpoch(usize);

#[derive(Clone, Copy, Debug)]
pub(crate) struct CurrentCpuBinding {
    pub(crate) area: CpuAreaRef,
    pub(crate) epoch: CpuBindingEpoch,
}

const CPU_PHASE_MASK: usize = 0b11;
const CPU_UNBOUND: usize = 0b00;
const CPU_BINDING: usize = 0b01;
const CPU_BOUND: usize = 0b10;
const CPU_UNBINDING: usize = 0b11;
pub(crate) const PREEMPT_NO_RESCHED: u32 = 1 << 31;
const PREEMPT_DEPTH_MASK: u32 = !PREEMPT_NO_RESCHED;

const fn current_thread_reserved_size() -> usize {
    64 - 5 * size_of::<usize>() - size_of::<u32>()
}

/// Outcome of preparing one current-thread preemption-guard exit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CurrentPreemptExit {
    /// A nested depth was consumed without exposing a preemptible context.
    NestedConsumed,
    /// The final depth was consumed because no scheduler work is pending.
    FinalConsumed,
    /// The final depth remains published for scheduler-baton conversion.
    FinalPending,
}

/// Pinned scheduler/architecture header for one execution context.
///
/// CPU binding uses a four-phase publication word:
/// `Unbound -> Binding -> Bound -> Unbinding -> next Unbound`. The epoch is
/// retained solely to reject a stale incoming switch tail.
#[repr(C, align(64))]
pub struct CurrentThreadHeader {
    context: usize,
    cpu_area: AtomicUsize,
    binding_epoch: AtomicUsize,
    architecture_state: [AtomicUsize; 2],
    preempt_state: AtomicU32,
    reserved: [u8; current_thread_reserved_size()],
}

impl CurrentThreadHeader {
    /// Creates an unbound header before placing it in stable pinned storage.
    pub const fn new(context: CurrentContext) -> Self {
        Self {
            context: context.0,
            cpu_area: AtomicUsize::new(0),
            binding_epoch: AtomicUsize::new(CPU_UNBOUND),
            architecture_state: [const { AtomicUsize::new(0) }; 2],
            preempt_state: AtomicU32::new(PREEMPT_NO_RESCHED),
            reserved: [0; current_thread_reserved_size()],
        }
    }

    pub(crate) const fn boot(area_base: usize) -> Self {
        Self {
            context: 0,
            cpu_area: AtomicUsize::new(area_base),
            binding_epoch: AtomicUsize::new(CPU_BOUND),
            architecture_state: [const { AtomicUsize::new(0) }; 2],
            preempt_state: AtomicU32::new(PREEMPT_NO_RESCHED),
            reserved: [0; current_thread_reserved_size()],
        }
    }

    /// Returns the immutable runtime context identity, if this is a task.
    pub const fn current_context(&self) -> Option<CurrentContext> {
        CurrentContext::from_raw(self.context)
    }

    /// Returns the ordinary preemption-guard depth of this execution context.
    ///
    /// The current-thread register makes this state migration-stable without a
    /// CPU-area lookup. The scheduler baton remains CPU-local and is not stored
    /// in this task-owned header.
    #[doc(hidden)]
    #[inline(always)]
    pub fn preempt_guard_depth(&self) -> u32 {
        self.preempt_state() & PREEMPT_DEPTH_MASK
    }

    #[inline(always)]
    fn preempt_state(&self) -> u32 {
        #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
        // SAFETY: the header remains pinned, and the x86 backend performs one
        // local memory instruction against this exact field.
        unsafe {
            crate::register::current_preempt_state(self)
        }
        #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
        self.preempt_state.load(Ordering::Relaxed)
    }

    /// Returns whether this context must schedule at its next safe point.
    #[doc(hidden)]
    #[inline(always)]
    pub fn preempt_need_resched(&self) -> bool {
        self.preempt_state() & PREEMPT_NO_RESCHED == 0
    }

    /// Publishes scheduler work into the current preemption fast-path word.
    #[doc(hidden)]
    #[inline(always)]
    pub fn set_preempt_need_resched(&self) {
        #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
        // SAFETY: a single local x86 RMW instruction is indivisible with
        // respect to a nested interrupt on the only CPU running this context.
        unsafe {
            crate::register::set_current_preempt_need_resched(self)
        }
        #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
        self.preempt_state
            .fetch_and(PREEMPT_DEPTH_MASK, Ordering::Relaxed);
    }

    /// Clears scheduler work after the safe point has drained the CPU queues.
    #[doc(hidden)]
    #[inline(always)]
    pub fn clear_preempt_need_resched(&self) {
        #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
        // SAFETY: the scheduler owns this current context with local IRQs
        // disabled, so one local x86 RMW instruction cannot race a switch.
        unsafe {
            crate::register::clear_current_preempt_need_resched(self)
        }
        #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
        self.preempt_state
            .fetch_or(PREEMPT_NO_RESCHED, Ordering::Relaxed);
    }

    /// Enters one ordinary preemption guard on the current execution context.
    #[doc(hidden)]
    #[inline(always)]
    pub fn enter_preempt_guard(&self) {
        #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
        let previous = {
            let previous = self.preempt_guard_depth();
            // SAFETY: only the CPU executing this current context mutates the
            // field. One x86 RMW instruction is indivisible with respect to a
            // nested local interrupt, matching Linux raw per-CPU semantics.
            unsafe { crate::register::enter_current_preempt_guard(self) };
            previous
        };
        #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
        let previous = self.preempt_state.fetch_add(1, Ordering::Relaxed);
        assert_ne!(
            previous & PREEMPT_DEPTH_MASK,
            PREEMPT_DEPTH_MASK,
            "current-thread preemption guard nesting overflow"
        );
    }

    /// Consumes a nested guard or retains the final depth for baton conversion.
    #[doc(hidden)]
    pub fn prepare_preempt_guard_exit(&self) -> CurrentPreemptExit {
        #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
        loop {
            let state = self.preempt_state();
            let depth = state & PREEMPT_DEPTH_MASK;
            assert!(depth > 0, "unbalanced current-thread preemption guard exit");
            if depth == 1 {
                if state & PREEMPT_NO_RESCHED == 0 {
                    return CurrentPreemptExit::FinalPending;
                }
                // SAFETY: cmpxchg is one local instruction. If an interrupt
                // publishes scheduler work before it executes, the comparison
                // fails and the loop observes the pending state.
                if unsafe { crate::register::try_consume_final_current_preempt_guard(self) } {
                    return CurrentPreemptExit::FinalConsumed;
                }
                continue;
            }
            // SAFETY: preemption is already disabled, so this context cannot
            // run concurrently elsewhere. A nested local IRQ restores its own
            // increment before this single decrement executes.
            unsafe { crate::register::exit_nested_current_preempt_guard(self) };
            return CurrentPreemptExit::NestedConsumed;
        }

        #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
        loop {
            let state = self.preempt_state.load(Ordering::Relaxed);
            let depth = state & PREEMPT_DEPTH_MASK;
            assert!(depth > 0, "unbalanced current-thread preemption guard exit");
            if depth == 1 {
                if state & PREEMPT_NO_RESCHED == 0 {
                    return CurrentPreemptExit::FinalPending;
                }
                if self
                    .preempt_state
                    .compare_exchange_weak(
                        state,
                        PREEMPT_NO_RESCHED,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    return CurrentPreemptExit::FinalConsumed;
                }
                continue;
            }
            if self
                .preempt_state
                .compare_exchange_weak(state, state - 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return CurrentPreemptExit::NestedConsumed;
            }
        }
    }

    /// Converts the exact final ordinary guard into scheduler-owned state.
    ///
    /// Callers must keep local IRQs disabled until the CPU-local scheduler
    /// baton is published, so no interrupt can observe an unowned preemptible
    /// window between the two state transitions.
    #[doc(hidden)]
    #[inline(always)]
    pub fn consume_final_preempt_guard(&self) -> bool {
        #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
        {
            if self.preempt_guard_depth() != 1 {
                return false;
            }
            // SAFETY: the caller's local IRQ exclusion makes the final store
            // indivisible with scheduler-baton publication on this CPU.
            unsafe { crate::register::consume_final_current_preempt_guard(self) };
            true
        }
        #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
        {
            self.preempt_state
                .compare_exchange(1, 0, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        }
    }

    /// Returns the stable CPU area while this header is fully bound.
    pub fn cpu_area(&self) -> Option<CpuAreaRef> {
        self.cpu_binding().map(|binding| binding.area)
    }

    /// Returns the raw bound area base used by architecture trap entry.
    #[inline(always)]
    pub fn cpu_area_base(&self) -> Option<usize> {
        self.raw_cpu_binding().map(|(area_base, _)| area_base)
    }

    /// Returns the immutable logical identity of the bound CPU area.
    #[inline(always)]
    pub fn cpu_index(&self) -> Option<CpuIndex> {
        let area_base = self.cpu_area_base()?;
        // SAFETY: bind_cpu accepts only a validated shutdown-lifetime
        // CpuAreaRef. The binding epoch above keeps the selected base coherent,
        // and the prefix identity is immutable after initialization.
        Some(
            unsafe { &*(area_base as *const CpuAreaPrefix) }
                .header()
                .cpu_index(),
        )
    }

    pub(crate) unsafe fn bind_cpu(
        self: Pin<&Self>,
        area: CpuAreaRef,
    ) -> Result<CpuBindingEpoch, ThreadSwitchError> {
        let this = self.get_ref();
        let unbound = this.binding_epoch.load(Ordering::Acquire);
        if unbound & CPU_PHASE_MASK != CPU_UNBOUND {
            return Err(ThreadSwitchError::NextThreadAlreadyBound);
        }
        this.binding_epoch
            .compare_exchange(
                unbound,
                unbound | CPU_BINDING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| ThreadSwitchError::NextThreadAlreadyBound)?;
        this.cpu_area.store(area.base(), Ordering::Relaxed);
        let bound = (unbound & !CPU_PHASE_MASK) | CPU_BOUND;
        this.binding_epoch.store(bound, Ordering::Release);
        Ok(CpuBindingEpoch(bound))
    }

    pub(crate) unsafe fn unbind_cpu(
        self: Pin<&Self>,
        expected: CpuBindingEpoch,
    ) -> Result<(), ThreadSwitchError> {
        if expected.0 & CPU_PHASE_MASK != CPU_BOUND {
            return Err(ThreadSwitchError::StalePreviousBinding);
        }
        let this = self.get_ref();
        let unbinding = (expected.0 & !CPU_PHASE_MASK) | CPU_UNBINDING;
        this.binding_epoch
            .compare_exchange(expected.0, unbinding, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| ThreadSwitchError::StalePreviousBinding)?;
        this.cpu_area.store(0, Ordering::Relaxed);
        let next_unbound = (expected.0 & !CPU_PHASE_MASK).wrapping_add(4);
        this.binding_epoch.store(next_unbound, Ordering::Release);
        Ok(())
    }

    pub(crate) fn cpu_binding(&self) -> Option<CurrentCpuBinding> {
        let (area_base, epoch) = self.raw_cpu_binding()?;
        // SAFETY: only bind_cpu can publish this field, and it accepts an
        // already validated shutdown-lifetime CpuAreaRef.
        let area = unsafe { CpuAreaRef::from_initialized_base(area_base) }.ok()?;
        Some(CurrentCpuBinding { area, epoch })
    }

    #[inline(always)]
    pub(crate) fn raw_cpu_binding(&self) -> Option<(usize, CpuBindingEpoch)> {
        loop {
            let before = self.binding_epoch.load(Ordering::Acquire);
            if before & CPU_PHASE_MASK != CPU_BOUND {
                return None;
            }
            let area_base = self.cpu_area.load(Ordering::Relaxed);
            let after = self.binding_epoch.load(Ordering::Acquire);
            if before == after {
                return Some((area_base, CpuBindingEpoch(after)));
            }
            core::hint::spin_loop();
        }
    }

    /// Returns the stable pointer installed in the current-thread register.
    pub fn as_non_null(self: Pin<&Self>) -> NonNull<Self> {
        NonNull::from(self.get_ref())
    }
}

/// Byte offset of the current header's bound CPU-area base.
pub const CURRENT_THREAD_CPU_BASE_OFFSET: usize = offset_of!(CurrentThreadHeader, cpu_area);
/// Byte offset of architecture-owned task trap state.
pub const CURRENT_THREAD_ARCH_STATE_OFFSET: usize =
    offset_of!(CurrentThreadHeader, architecture_state);
/// Byte offset of current-context preemption depth and reschedule state.
#[doc(hidden)]
pub const CURRENT_THREAD_PREEMPT_STATE_OFFSET: usize =
    offset_of!(CurrentThreadHeader, preempt_state);
/// Reserved bytes available to architecture-owned task trap state.
pub const CURRENT_THREAD_ARCH_STATE_SIZE: usize = 2 * size_of::<usize>();

const _: () = {
    assert!(size_of::<CurrentThreadHeader>() == 64);
    assert!(core::mem::align_of::<CurrentThreadHeader>() == 64);
};
