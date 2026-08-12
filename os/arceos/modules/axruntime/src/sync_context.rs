//! Execution-context capability selected by the runtime, not by `ax-sync`.

pub(crate) fn preempt_enter() -> usize {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::preempt_enter();

    #[cfg(all(
        not(all(feature = "host-test", not(target_os = "none"))),
        feature = "multitask"
    ))]
    return usize::from(crate::guard::enter_lock_preempt());

    #[cfg(all(
        not(all(feature = "host-test", not(target_os = "none"))),
        not(feature = "multitask")
    ))]
    0
}

pub(crate) unsafe fn preempt_exit(state: usize) {
    if state == 0 {
        return;
    }

    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    unsafe {
        crate::host::preempt_exit(state);
    }

    #[cfg(all(
        not(all(feature = "host-test", not(target_os = "none"))),
        feature = "multitask"
    ))]
    crate::guard::exit_preempt();

    #[cfg(all(
        not(all(feature = "host-test", not(target_os = "none"))),
        not(feature = "multitask")
    ))]
    unreachable!("a uniprocessor runtime cannot own a preemption token");
}

pub(crate) unsafe fn preempt_exit_irq_return(state: usize) {
    if state == 0 {
        return;
    }

    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    unsafe {
        crate::host::preempt_exit(state);
    }

    #[cfg(all(
        not(all(feature = "host-test", not(target_os = "none"))),
        feature = "multitask"
    ))]
    crate::guard::exit_preempt_from_irq_return();

    #[cfg(all(
        not(all(feature = "host-test", not(target_os = "none"))),
        not(feature = "multitask")
    ))]
    unreachable!("a uniprocessor runtime cannot own an IRQ-return preemption token");
}

pub(crate) fn irq_save_and_disable() -> usize {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::irq_save_and_disable();

    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    {
        let was_enabled = ax_hal::asm::irqs_enabled();
        ax_hal::asm::disable_irqs();
        usize::from(was_enabled)
    }
}

pub(crate) unsafe fn irq_restore(state: usize) {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    unsafe {
        crate::host::irq_restore(state);
    }

    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    if state != 0 {
        ax_hal::asm::enable_irqs();
    } else {
        ax_hal::asm::disable_irqs();
    }
}

pub(crate) fn hardirq_enter() {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    crate::host::hardirq_enter();

    #[cfg(all(
        not(all(feature = "host-test", not(target_os = "none"))),
        feature = "multitask"
    ))]
    crate::irq_time::enter();
}

pub(crate) fn hardirq_exit() {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    crate::host::hardirq_exit();

    #[cfg(all(
        not(all(feature = "host-test", not(target_os = "none"))),
        feature = "multitask"
    ))]
    crate::irq_time::exit();
}
