//! Tests of the x86 interrupt-state acquisition contract.

#[cfg(test)]
mod tests {
    use ax_sync::{RawSpinLock, interface::CONTEXT_PREEMPT_IRQSAVE};

    use crate::host_lock_provider::{
        acquire_count, last_acquire_context, last_acquire_state, last_release_state, release_count,
    };

    #[test]
    fn irqsave_guard_excludes_reentry_and_restores_its_context() {
        let storage = RawSpinLock::new(0u32);
        let acquires_before = acquire_count();
        let releases_before = release_count();

        let mut guard = storage.lock_irqsave();
        // The device-state wrapper must take the IRQ-save acquisition context,
        // not a bare spin acquisition. The counters are thread-local, so this
        // stays exact even when tests run in parallel.
        assert_eq!(
            last_acquire_context(),
            CONTEXT_PREEMPT_IRQSAVE,
            "x86 device state must be taken with local IRQs saved",
        );
        assert_eq!(acquire_count(), acquires_before + 1);
        let acquire_state = last_acquire_state();
        assert!(acquire_state.is_some());

        // A held guard excludes a second acquisition of the same storage.
        assert!(
            storage.try_lock_irqsave().is_none(),
            "a held guard must exclude a second acquisition",
        );

        assert_eq!(*guard, 0);
        *guard = 7;
        drop(guard);

        // The guard hands its acquisition's opaque context back to the provider
        // so the saved local-IRQ state is restored on release.
        assert_eq!(release_count(), releases_before + 1);
        assert_eq!(last_release_state(), acquire_state);

        // The protected value survives the guarded section and can be retaken.
        assert_eq!(*storage.lock_irqsave(), 7);
    }
}
