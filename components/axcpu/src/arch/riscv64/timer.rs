//! CPU view of the platform time counter.

/// Reads the 64-bit time CSR. Its frequency is supplied by the platform.
#[inline]
pub fn read_counter() -> u64 {
    let ticks;
    // SAFETY: the supervisor execution environment grants access to time.
    // Reading this CSR does not change timer or interrupt state.
    unsafe {
        core::arch::asm!("csrr {ticks}, time", ticks = out(reg) ticks, options(nomem, nostack));
    }
    ticks
}

/// Reads the architectural fixed cycle counter granted by the execution environment.
#[inline]
pub fn read_cycle_counter() -> u64 {
    let value;
    // SAFETY: the supervisor execution environment grants fixed-counter access.
    unsafe {
        core::arch::asm!("csrr {}, cycle", out(reg) value, options(nostack));
    }
    value
}

/// Reads the architectural fixed retired-instruction counter granted by the execution environment.
#[inline]
pub fn read_instruction_counter() -> u64 {
    let value;
    // SAFETY: the supervisor execution environment grants fixed-counter access.
    unsafe {
        core::arch::asm!("csrr {}, instret", out(reg) value, options(nostack));
    }
    value
}

/// Returns whether supervisor timer interrupt delivery is locally enabled.
pub fn irq_enabled() -> bool {
    let sie: usize;
    // SAFETY: supervisor code may read its local interrupt enable bank.
    unsafe {
        core::arch::asm!("csrr {}, sie", out(reg) sie, options(nostack));
    }
    sie & (1 << 5) != 0
}

/// Changes supervisor timer interrupt delivery without changing global SIE.
///
/// # Safety
/// Before enabling, the platform must install a matching handler and retain
/// the timer source owner until delivery is disabled and acknowledged.
pub unsafe fn set_irq_enabled(enabled: bool) {
    // SAFETY: only the caller-owned supervisor timer source is modified.
    unsafe {
        if enabled {
            core::arch::asm!("csrs sie, {}", in(reg) 1usize << 5, options(nostack));
        } else {
            core::arch::asm!("csrc sie, {}", in(reg) 1usize << 5, options(nostack));
        }
    }
}
