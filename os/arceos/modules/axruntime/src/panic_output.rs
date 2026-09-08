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
    match axpanic::enter_panic(current_cpu_id()) {
        axpanic::PanicDisposition::Primary => panic_primary(info),
        axpanic::PanicDisposition::Recursive | axpanic::PanicDisposition::Concurrent => {
            panic_shutdown()
        }
    }
}

fn panic_primary(info: &impl Display) -> ! {
    let _oops_guard = axpanic::enter_oops();
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
        crate::hal::percpu::this_cpu_id()
    }

    #[cfg(not(feature = "smp"))]
    {
        0
    }
}
