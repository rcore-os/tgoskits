//! Runtime-specific held-lock storage and fatal-reporting backends.

use core::fmt;

use super::types::{HeldLock, HeldLockSnapshot};

#[cfg(any(test, doctest, all(feature = "host-test", not(target_os = "none"))))]
mod imp {
    use std::cell::RefCell;

    use super::*;
    use crate::sync::lockdep::types::HeldLockStack;

    std::thread_local! {
        static HELD_LOCKS: RefCell<HeldLockStack> = const { RefCell::new(HeldLockStack::new()) };
    }

    fn with_current_task_held_locks<R>(f: impl FnOnce(&HeldLockStack) -> R) -> R {
        HELD_LOCKS.with(|held_locks| f(&held_locks.borrow()))
    }

    fn with_current_task_held_locks_mut<R>(f: impl FnOnce(&mut HeldLockStack) -> R) -> R {
        HELD_LOCKS.with(|held_locks| f(&mut held_locks.borrow_mut()))
    }

    pub(crate) fn collect_current_task_held_locks(snapshot: &mut HeldLockSnapshot) {
        with_current_task_held_locks(|stack| snapshot.extend(stack));
    }

    pub(crate) fn push_current_task_held_lock(held: HeldLock) {
        with_current_task_held_locks_mut(|stack| stack.push(held));
    }

    pub(crate) fn pop_current_task_held_lock(lock_addr: usize) {
        with_current_task_held_locks_mut(|stack| stack.pop_checked(lock_addr));
    }

    pub(crate) fn lockdep_fatal(message: fmt::Arguments<'_>) -> ! {
        panic!("{message}")
    }
}

#[cfg(not(any(test, doctest, all(feature = "host-test", not(target_os = "none")))))]
mod imp {
    use super::*;

    pub(crate) fn collect_current_task_held_locks(snapshot: &mut HeldLockSnapshot) {
        let _irq_guard = crate::sync::IrqSaveGuard::new();
        if let Some(curr) = crate::current_may_uninit() {
            curr.with_held_locks(|stack| snapshot.extend(stack));
        }
    }

    pub(crate) fn push_current_task_held_lock(held: HeldLock) {
        let _irq_guard = crate::sync::IrqSaveGuard::new();
        if let Some(curr) = crate::current_may_uninit() {
            curr.with_held_locks(|stack| stack.push(held));
        }
    }

    pub(crate) fn pop_current_task_held_lock(lock_addr: usize) {
        let _irq_guard = crate::sync::IrqSaveGuard::new();
        if let Some(curr) = crate::current_may_uninit() {
            curr.with_held_locks(|stack| stack.pop_checked(lock_addr));
        }
    }

    pub(crate) fn lockdep_fatal(message: fmt::Arguments<'_>) -> ! {
        let _oops_guard = axpanic::enter_oops();

        struct ConsoleWriter;

        impl fmt::Write for ConsoleWriter {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                emergency_write_str(s);
                Ok(())
            }
        }

        let mut writer = ConsoleWriter;
        let _ = fmt::Write::write_fmt(&mut writer, message);
        let _ = fmt::Write::write_str(&mut writer, "\n");
        emergency_write_str("lockdep fatal violation\n");
        ax_hal::power::system_off()
    }

    #[cfg(target_arch = "riscv64")]
    fn emergency_write_str(s: &str) {
        for &byte in s.as_bytes() {
            #[allow(deprecated)]
            {
                sbi_rt::legacy::console_putchar(byte as usize);
            }
        }
    }

    #[cfg(not(target_arch = "riscv64"))]
    fn emergency_write_str(s: &str) {
        ax_hal::console::write_bytes(s.as_bytes());
    }
}

pub(super) use imp::*;
