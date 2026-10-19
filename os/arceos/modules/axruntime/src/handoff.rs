//! Terminal runtime shutdown before transferring control to another kernel.
//!
//! Firmware context and target entry validation remain owned by someboot.

/// Stops runtime services and invokes `enter` with this CPU's IRQs and
/// preemption disabled. A shutdown failure or a returning target resets the
/// machine; the stopped runtime is never resumed.
///
/// This currently requires exactly one runtime CPU. Other initialized DMA
/// subsystems must be added to the shutdown sequence before they can be used
/// with this capability.
///
/// # Safety
/// The application must have stopped its other tasks and I/O producers, closed
/// network handles and committed persistent state. `enter` must only transfer
/// control to an independently owned image; it must not sleep, allocate or use
/// runtime services. That image must install its own exception/CPU-local state
/// and stack before enabling interrupts or freeing inherited runtime memory.
/// A returning entry must preserve the caller's stack, MMU mappings, CPU-local
/// state and interrupt mask, as required by its calling convention. A kernel
/// that has replaced that execution context must not return to this caller.
pub unsafe fn enter(enter: impl FnOnce()) -> ! {
    if crate::hal::cpu_num() != 1
        || cfg!(any(
            feature = "display",
            feature = "input",
            feature = "vsock"
        ))
    {
        reset_with_error("unsupported runtime handoff configuration");
    }

    #[cfg(feature = "fs")]
    {
        if let Err(error) = ax_fs_ng::shutdown_filesystems() {
            reset_with_error(error);
        }
        if let Err(error) = ax_fs_ng::release_block_irqs_for_passthrough() {
            reset_with_error(error);
        }
    }
    #[cfg(feature = "net")]
    if let Err(error) = ax_net::shutdown_network() {
        reset_with_error(error);
    }

    // Use the output transaction rather than enqueueing a log record which
    // could still be awaiting extraction when serial ownership is stopped.
    if let Ok(output) = crate::console::output() {
        if let Err(error) = output.write_text_all(b"runtime_handoff_devices_stopped\n") {
            reset_with_error(error);
        }
        if let Err(error) = output.drain() {
            reset_with_error(error);
        }
    }
    for runtime in crate::serial::runtimes() {
        if let Err(error) = runtime.shutdown() {
            reset_with_error(error);
        }
    }

    // With one CPU and no live device DMA, this prevents every old background
    // task and IRQ from accessing pages the target's allocator may reuse.
    // Keep this guard alive through entry and the terminal reset on return.
    let _guard = crate::task::sync::PreemptIrqSaveGuard::new();
    crate::clock_event_runtime::take_current_clock_event_offline();
    enter();
    crate::hal::power::system_reset()
}

fn reset_with_error(error: impl core::fmt::Display) -> ! {
    crate::emergency_console::write_fmt(format_args!("runtime_handoff_failed: {error}\n"));
    crate::hal::power::system_reset()
}
