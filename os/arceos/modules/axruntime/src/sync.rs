//! ArceOS runtime providers for `ax-sync` capabilities.

#[cfg(not(feature = "host-test"))]
struct RuntimeCriticalSectionOps;

#[cfg(not(feature = "host-test"))]
#[ax_crate_interface::impl_interface]
impl ax_sync::CriticalSectionOps for RuntimeCriticalSectionOps {
    fn preempt_guard_enter() -> ax_sync::PreemptGuardToken {
        #[cfg(feature = "multitask")]
        {
            return ax_sync::PreemptGuardToken::from_entered(crate::guard::enter_lock_preempt());
        }
        #[cfg(not(feature = "multitask"))]
        ax_sync::PreemptGuardToken::from_entered(false)
    }

    fn preempt_guard_exit(token: ax_sync::PreemptGuardToken) {
        #[cfg(feature = "multitask")]
        if !token.is_none() {
            crate::guard::exit_preempt();
        }
        #[cfg(not(feature = "multitask"))]
        assert!(
            token.is_none(),
            "uniprocessor runtime received a preemption token"
        );
    }

    fn preempt_guard_exit_irq_return(token: ax_sync::PreemptGuardToken) {
        #[cfg(feature = "multitask")]
        if !token.is_none() {
            crate::guard::exit_preempt_from_irq_return();
        }
        #[cfg(not(feature = "multitask"))]
        assert!(
            token.is_none(),
            "uniprocessor runtime received an IRQ-return token"
        );
    }

    fn hardirq_enter() {
        #[cfg(feature = "multitask")]
        crate::irq_time::enter();
    }

    fn hardirq_exit() {
        #[cfg(feature = "multitask")]
        crate::irq_time::exit();
    }

    fn irq_save_and_disable() -> usize {
        let was_enabled = ax_hal::asm::irqs_enabled();
        ax_hal::asm::disable_irqs();
        usize::from(was_enabled)
    }

    fn irq_restore(state: usize) {
        if state != 0 {
            ax_hal::asm::enable_irqs();
        } else {
            ax_hal::asm::disable_irqs();
        }
    }
}

#[cfg(all(feature = "lockdep", not(feature = "host-test")))]
struct RuntimeLockdepOps;

#[cfg(all(feature = "lockdep", not(feature = "host-test")))]
#[ax_crate_interface::impl_interface]
impl ax_sync::LockdepOps for RuntimeLockdepOps {
    fn irq_save_and_disable() -> usize {
        let was_enabled = ax_hal::asm::irqs_enabled();
        ax_hal::asm::disable_irqs();
        usize::from(was_enabled)
    }

    fn irq_restore(state: usize) {
        if state != 0 {
            ax_hal::asm::enable_irqs();
        } else {
            ax_hal::asm::disable_irqs();
        }
    }

    fn collect_current_task_held_locks(snapshot: &mut ax_sync::HeldLockSnapshot) {
        ax_task::collect_current_task_held_locks(snapshot);
    }

    fn push_current_task_held_lock(held: ax_sync::HeldLock) {
        ax_task::push_current_task_held_lock(held);
    }

    fn pop_current_task_held_lock(lock_addr: usize) {
        ax_task::pop_current_task_held_lock(lock_addr);
    }

    fn console_write_str(s: &str) {
        ax_hal::console::write_bytes(s.as_bytes());
    }

    fn fatal() -> ! {
        ax_hal::power::system_off()
    }
}
