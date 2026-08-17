use core::{
    marker::PhantomData,
    mem::ManuallyDrop,
    ptr::NonNull,
    sync::atomic::{AtomicU32, Ordering},
};

use crate::{CpuLocalError, CpuPin};

const PREEMPT_NO_PENDING: u32 = 1 << 31;
const PREEMPT_DEPTH_MASK: u32 = !PREEMPT_NO_PENDING;

/// Snapshot of one architecture-selected preemption state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreemptionSnapshot {
    depth: u32,
    pending: bool,
}

impl PreemptionSnapshot {
    /// Returns the number of active preemption exclusions.
    pub const fn depth(self) -> u32 {
        self.depth
    }

    /// Returns whether work is pending at the next preemptible boundary.
    pub const fn is_pending(self) -> bool {
        self.pending
    }
}

/// Linear proof of one entered preemption exclusion.
#[must_use = "every entered preemption token must be finished exactly once"]
pub struct PreemptionToken {
    owner: NonNull<PreemptionState>,
    _not_send_or_sync: PhantomData<*mut ()>,
}

/// Linear proof that the final preemption depth is reserved for a safe point.
#[must_use = "pending preemption must be released by the external safe-point owner"]
pub struct PendingPreemption {
    owner: NonNull<PreemptionState>,
    _not_send_or_sync: PhantomData<*mut ()>,
}

/// Result of finishing one preemption exclusion.
#[must_use]
pub enum PreemptionExit {
    /// A nested depth was consumed; preemption remains excluded.
    Nested,
    /// The final depth was consumed and execution is preemptible.
    Enabled,
    /// The final depth remains reserved until an external safe point claims it.
    Pending(PendingPreemption),
}

/// Architecture-neutral preemption word.
///
/// The high bit uses inverted pending polarity, allowing a newly initialized
/// context to start enabled without a runtime initialization write. The low
/// bits contain the exclusion depth. All updates are local to the selected CPU
/// or pinned context; external safe-point serialization provides ordering for
/// protected state, so this word does not publish cross-CPU data.
#[repr(transparent)]
pub(crate) struct PreemptionState(AtomicU32);

impl PreemptionState {
    pub(crate) const fn new() -> Self {
        Self(AtomicU32::new(PREEMPT_NO_PENDING))
    }

    pub(crate) const fn bootstrap_disabled() -> Self {
        Self(AtomicU32::new(PREEMPT_NO_PENDING | 1))
    }

    fn snapshot(&self) -> PreemptionSnapshot {
        let state = self.0.load(Ordering::Relaxed);
        PreemptionSnapshot {
            depth: state & PREEMPT_DEPTH_MASK,
            pending: state & PREEMPT_NO_PENDING == 0,
        }
    }

    fn set_pending(&self) {
        self.0.fetch_and(PREEMPT_DEPTH_MASK, Ordering::Relaxed);
    }

    fn clear_pending(&self) {
        self.0.fetch_or(PREEMPT_NO_PENDING, Ordering::Relaxed);
    }

    #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
    fn enter(&self) {
        let previous = self.0.fetch_add(1, Ordering::Relaxed);
        assert_ne!(
            previous & PREEMPT_DEPTH_MASK,
            PREEMPT_DEPTH_MASK,
            "preemption nesting overflow"
        );
    }

    fn finish(&self) -> PreemptionExit {
        loop {
            let state = self.0.load(Ordering::Relaxed);
            let depth = state & PREEMPT_DEPTH_MASK;
            assert!(depth > 0, "unbalanced preemption exit");

            if depth == 1 && state & PREEMPT_NO_PENDING == 0 {
                return PreemptionExit::Pending(PendingPreemption::new(self));
            }

            let next = state - 1;
            if self
                .0
                .compare_exchange_weak(state, next, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return if depth == 1 {
                    PreemptionExit::Enabled
                } else {
                    PreemptionExit::Nested
                };
            }
        }
    }

    fn release_pending(&self) {
        assert_eq!(
            self.0
                .compare_exchange(1, 0, Ordering::Relaxed, Ordering::Relaxed),
            Ok(1),
            "pending preemption no longer owns the final depth"
        );
    }

    fn release_bootstrap(&self) {
        assert_eq!(
            self.0.compare_exchange(
                PREEMPT_NO_PENDING | 1,
                PREEMPT_NO_PENDING,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ),
            Ok(PREEMPT_NO_PENDING | 1),
            "bootstrap preemption depth must be released exactly once"
        );
    }

    #[cfg(any(all(target_arch = "x86_64", not(feature = "host-test")), test))]
    fn release_initial_switch(&self) -> bool {
        loop {
            let state = self.0.load(Ordering::Relaxed);
            if state == PREEMPT_NO_PENDING {
                return false;
            }
            if state != (PREEMPT_NO_PENDING | 1) && state != 1 {
                panic!("initial context switch found invalid preemption state {state:#x}");
            }
            // A nested outgoing guard may mirror its owner's pending bit after
            // the switch guard was entered. The incoming context has its own
            // upper-layer publication, so consume the inherited depth and
            // reset that stale mirror as one transition.
            if self
                .0
                .compare_exchange_weak(
                    state,
                    PREEMPT_NO_PENDING,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                return true;
            }
        }
    }
}

impl PreemptionToken {
    fn new(owner: &PreemptionState) -> Self {
        Self {
            owner: NonNull::from(owner),
            _not_send_or_sync: PhantomData,
        }
    }

    /// Converts this token into an opaque value for an ABI that transports
    /// guard state as one machine word.
    #[doc(hidden)]
    pub fn into_raw(self) -> usize {
        let token = ManuallyDrop::new(self);
        token.owner.as_ptr() as usize
    }

    /// Reconstructs a token produced by [`Self::into_raw`].
    ///
    /// # Safety
    ///
    /// `raw` must come from one still-live token, refer to a live CPU-local
    /// preemption owner, and be reconstructed and finished exactly once.
    #[doc(hidden)]
    pub unsafe fn from_raw(raw: usize) -> Option<Self> {
        if !raw.is_multiple_of(core::mem::align_of::<PreemptionState>()) {
            return None;
        }
        NonNull::new(raw as *mut PreemptionState).map(|owner| Self {
            owner,
            _not_send_or_sync: PhantomData,
        })
    }

    fn state(&self) -> &PreemptionState {
        // SAFETY: construction and raw reconstruction require the selected
        // owner to remain live through this token's single finish operation.
        unsafe { self.owner.as_ref() }
    }

    #[cfg(any(all(target_arch = "x86_64", not(feature = "host-test")), test))]
    fn handoff_after_context_switch(self, resumed_owner: &PreemptionState) -> Self {
        if self.owner == NonNull::from(resumed_owner) {
            self
        } else {
            // The old CPU's incoming context consumed the depth represented by
            // this proof. Consume the proof itself and adopt the equivalent
            // depth left by the outgoing context on the resumed CPU.
            Self::new(resumed_owner)
        }
    }
}

impl PendingPreemption {
    fn new(owner: &PreemptionState) -> Self {
        Self {
            owner: NonNull::from(owner),
            _not_send_or_sync: PhantomData,
        }
    }

    /// Atomically consumes the final reserved preemption depth.
    pub fn release(self) {
        // SAFETY: the linear token retains its exact owner until this consume.
        unsafe { self.owner.as_ref() }.release_pending();
    }
}

/// Enters preemption exclusion on the owner selected by this architecture.
#[inline(always)]
pub fn enter_preemption() -> PreemptionToken {
    #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
    {
        // Increment through GS before resolving the token owner. Once the
        // increment is visible this execution cannot migrate away from it.
        unsafe { crate::register::enter_x86_preemption() };
        let owner = crate::register::current_area()
            .unwrap_or_else(|_| crate::register::fatal_register_invariant())
            .runtime_anchor()
            .preemption_state();
        PreemptionToken::new(owner)
    }

    #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
    {
        let current = unsafe { crate::current_context_unpinned() }
            .unwrap_or_else(|_| crate::register::fatal_register_invariant());
        // SAFETY: the architecture current register identifies the executing
        // context itself. An interrupt may suspend and later migrate that
        // context between this read and the increment, but it cannot resume
        // this instruction stream as a different context; the pinned header
        // stays live and therefore remains the exact token owner.
        let owner = unsafe { current.as_ref() }.preemption_state();
        owner.enter();
        PreemptionToken::new(owner)
    }
}

/// Observes the current architecture-selected preemption state.
pub fn preemption_snapshot(pin: &CpuPin<'_>) -> Result<PreemptionSnapshot, CpuLocalError> {
    Ok(selected_state(pin)?.snapshot())
}

/// Marks work pending at the current preemptible boundary.
pub fn set_preemption_pending(pin: &CpuPin<'_>) -> Result<(), CpuLocalError> {
    selected_state(pin)?.set_pending();
    Ok(())
}

/// Clears the pending mark after the external owner has drained its work.
pub fn clear_preemption_pending(pin: &CpuPin<'_>) -> Result<(), CpuLocalError> {
    selected_state(pin)?.clear_pending();
    Ok(())
}

/// Finishes the exact owner captured by [`enter_preemption`].
pub fn finish_preemption(token: PreemptionToken) -> PreemptionExit {
    token.state().finish()
}

/// Transfers a CPU-owned switch exclusion to the CPU where its context resumed.
///
/// Context-owned tokens keep their original owner across migration. A CPU-owned
/// token is replaced only when a context switch resumes it on another CPU: the
/// old CPU's incoming context has already consumed the old switch depth, while
/// this CPU's outgoing context left the matching depth for the resumed guard.
///
/// # Errors
///
/// Returns an error when the pinned CPU has no valid selected preemption owner.
///
/// # Panics
///
/// Panics when a context-owned architecture observes a different owner after
/// the context switch. Such architectures migrate the owner with the context.
#[doc(hidden)]
pub fn handoff_preemption_after_context_switch(
    pin: &CpuPin<'_>,
    token: PreemptionToken,
) -> Result<PreemptionToken, CpuLocalError> {
    let owner = selected_state(pin)?;
    #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
    {
        Ok(token.handoff_after_context_switch(owner))
    }

    #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
    {
        assert_eq!(
            token.owner,
            NonNull::from(owner),
            "context-owned preemption token changed owner across a context switch"
        );
        Ok(token)
    }
}

/// Releases the single preemption depth inherited by a bootstrap context.
///
/// The caller uses this only after the context and every local safe-point
/// dependency have been published.
#[doc(hidden)]
pub fn release_bootstrap_preemption(pin: &CpuPin<'_>) -> Result<(), CpuLocalError> {
    selected_state(pin)?.release_bootstrap();
    Ok(())
}

/// Releases the preemption exclusion transferred to a context on its first
/// architecture switch.
///
/// CPU-owned preemption architectures carry the outgoing switch exclusion
/// across the raw transfer, but a new context has no suspended caller whose
/// guard can finish it. Context-owned architectures need no action because the
/// new header begins enabled.
#[doc(hidden)]
pub fn release_initial_context_preemption(pin: &CpuPin<'_>) -> Result<bool, CpuLocalError> {
    #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
    {
        Ok(selected_state(pin)?.release_initial_switch())
    }

    #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
    {
        let snapshot = selected_state(pin)?.snapshot();
        assert_eq!(
            snapshot.depth(),
            0,
            "new context-owned preemption state must start enabled"
        );
        Ok(false)
    }
}

fn selected_state(pin: &CpuPin<'_>) -> Result<&'static PreemptionState, CpuLocalError> {
    #[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
    {
        Ok(pin.area().runtime_anchor().preemption_state())
    }

    #[cfg(any(not(target_arch = "x86_64"), feature = "host-test"))]
    {
        let current = crate::current_context(pin)?;
        // SAFETY: current publication keeps the pinned context live, and the
        // caller's CpuPin prevents it from changing during this observation.
        Ok(unsafe { current.as_ref() }.preemption_state())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enter_on(state: &PreemptionState) -> PreemptionToken {
        state.enter();
        PreemptionToken::new(state)
    }

    #[test]
    fn nested_and_final_exits_are_linear() {
        let state = PreemptionState::new();
        let outer = enter_on(&state);
        let inner = enter_on(&state);

        assert!(matches!(finish_preemption(inner), PreemptionExit::Nested));
        assert_eq!(state.snapshot().depth(), 1);
        assert!(matches!(finish_preemption(outer), PreemptionExit::Enabled));
        assert_eq!(state.snapshot().depth(), 0);
    }

    #[test]
    fn pending_exit_reserves_depth_until_release() {
        let state = PreemptionState::new();
        let token = enter_on(&state);
        state.set_pending();

        let PreemptionExit::Pending(pending) = finish_preemption(token) else {
            panic!("final pending exit must retain its depth");
        };
        assert_eq!(state.snapshot().depth(), 1);
        pending.release();
        assert_eq!(state.snapshot().depth(), 0);
        assert!(state.snapshot().is_pending());
    }

    #[test]
    fn bootstrap_starts_disabled() {
        let state = PreemptionState::bootstrap_disabled();
        assert_eq!(state.snapshot().depth(), 1);
        assert!(!state.snapshot().is_pending());
    }

    #[test]
    fn initial_switch_discards_outgoing_pending_mirror() {
        let state = PreemptionState::bootstrap_disabled();
        state.set_pending();

        assert!(state.release_initial_switch());
        assert_eq!(state.snapshot().depth(), 0);
        assert!(!state.snapshot().is_pending());
    }

    #[test]
    #[should_panic(expected = "pending preemption no longer owns the final depth")]
    fn pending_depth_cannot_be_consumed_twice() {
        let state = PreemptionState(AtomicU32::new(1));
        let first = PendingPreemption::new(&state);
        let duplicate = PendingPreemption::new(&state);

        first.release();
        duplicate.release();
    }

    #[test]
    #[should_panic(expected = "bootstrap preemption depth must be released exactly once")]
    fn bootstrap_depth_cannot_be_released_twice() {
        let state = PreemptionState::bootstrap_disabled();

        state.release_bootstrap();
        state.release_bootstrap();
    }

    #[test]
    fn token_stays_bound_to_the_entry_owner() {
        let original_context = PreemptionState::new();
        let migrated_context = PreemptionState::new();
        let original_cpu = PreemptionState::new();
        let migrated_cpu = PreemptionState::new();

        let context_token = enter_on(&original_context);
        let cpu_token = enter_on(&original_cpu);
        migrated_context.enter();
        migrated_cpu.enter();

        assert!(matches!(
            finish_preemption(context_token),
            PreemptionExit::Enabled
        ));
        assert!(matches!(
            finish_preemption(cpu_token),
            PreemptionExit::Enabled
        ));
        assert_eq!(original_context.snapshot().depth(), 0);
        assert_eq!(original_cpu.snapshot().depth(), 0);
        assert_eq!(migrated_context.snapshot().depth(), 1);
        assert_eq!(migrated_cpu.snapshot().depth(), 1);
    }

    #[test]
    fn cpu_owned_switch_token_handoff_follows_the_resumed_cpu() {
        let original_cpu = PreemptionState::new();
        let resumed_cpu = PreemptionState::new();

        let suspended_switch = enter_on(&original_cpu);
        // Another incoming context consumes the switch depth left on the old
        // CPU while this context is suspended.
        assert!(original_cpu.release_initial_switch());
        // The outgoing context on the destination CPU leaves the equivalent
        // switch depth for the context that is about to resume there.
        resumed_cpu.enter();

        let resumed_switch = suspended_switch.handoff_after_context_switch(&resumed_cpu);
        assert!(matches!(
            finish_preemption(resumed_switch),
            PreemptionExit::Enabled
        ));
        assert_eq!(original_cpu.snapshot().depth(), 0);
        assert_eq!(resumed_cpu.snapshot().depth(), 0);
    }

    #[test]
    fn malformed_raw_owner_is_rejected() {
        // SAFETY: no token is reconstructed because the value is misaligned.
        assert!(unsafe { PreemptionToken::from_raw(1) }.is_none());
    }
}
