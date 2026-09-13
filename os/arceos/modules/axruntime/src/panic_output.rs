//! Fatal output shared by the core panic handler and Rust `std` panic hook.

use core::fmt::Display;

/// Installs the Rust `std` panic hook after the global allocator is available.
///
/// Creating the boxed hook is a one-time boot allocation. Invoking the hook is
/// allocation-free and never enters the task-console or logging locks.
#[cfg(feature = "std-compat")]
pub(crate) fn install_std_hook() {
    std::panic::set_hook(std::boxed::Box::new(|info| panic_now(info)));
}

/// Emits one panic record through the emergency console and powers off.
pub(crate) fn panic_now(info: &impl Display) -> ! {
    let _oops_guard = begin_panic();
    panic_primary(info)
}

fn begin_panic() -> axpanic::OopsGuard {
    // Never acquire a task/preemption guard: a CPU fault may have interrupted
    // publication of the task anchor, or already hold that context's locks.
    ax_cpu::interrupt::disable_irqs();
    match axpanic::enter_panic(current_cpu_id()) {
        axpanic::PanicDisposition::Primary => axpanic::enter_oops(),
        axpanic::PanicDisposition::Recursive | axpanic::PanicDisposition::Concurrent => {
            panic_shutdown()
        }
    }
}

fn panic_primary(info: &impl Display) -> ! {
    let _ = crate::emergency_console::write_fmt(format_args!("ARCEOS_PANIC_EMERGENCY\n{info}\n"));
    if axbacktrace::is_enabled() && axpanic::should_emit_panic_backtrace() {
        let backtrace = axbacktrace::RawBacktrace::capture().kind("panic");
        let _ = crate::emergency_console::write_fmt(format_args!("{backtrace}"));
    }
    panic_shutdown()
}

fn panic_shutdown() -> ! {
    crate::hal::power::system_off()
}

fn current_cpu_id() -> usize {
    #[cfg(feature = "smp")]
    {
        // SAFETY: begin_panic masked IRQs and this terminal path never yields
        // or resumes scheduling, so the installed CPU area cannot migrate.
        unsafe { crate::hal::percpu::with_cpu_pin(crate::hal::percpu::this_cpu_id_pinned) }
            .unwrap_or_else(|_| panic_shutdown())
    }

    #[cfg(not(feature = "smp"))]
    {
        0
    }
}

struct RuntimeFatalTrap;

ax_cpu::trap::fatal::fatal_trap::impl_trait! {
    impl FatalTrap for RuntimeFatalTrap {
        fn terminate(record: core::fmt::Arguments<'_>) -> ! {
            let _oops_guard = begin_panic();
            let _ = crate::emergency_console::write_fmt(format_args!(
                "ARCEOS_PANIC_EMERGENCY\n{record}\n"
            ));
            // A machine fault can interrupt a guest transition with foreign
            // TLS/task registers. Do not invoke std panic hooks or unwind it.
            panic_shutdown()
        }
    }
}
