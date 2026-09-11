//! CPU timestamp counter; frequency and cross-CPU calibration belong to the platform.

/// Reads the raw TSC without instruction serialization.
/// This alone does not prove counter frequency, invariance or CPU synchronization.
#[inline]
pub fn read_counter() -> u64 {
    // SAFETY: the kernel has access to the timestamp counter. RDTSC changes
    // no memory or control state and does not require a runtime allocation.
    unsafe { x86::time::rdtsc() }
}

/// Reads this CPU's firmware/OS TSC adjustment. CPUID must advertise TSC_ADJUST.
///
/// # Safety
/// Execute at CPL0 on a CPU that implements IA32_TSC_ADJUST.
pub unsafe fn read_adjustment() -> u64 {
    // SAFETY: the caller checked CPUID and executes in the owning kernel.
    unsafe { x86::msr::rdmsr(x86::msr::IA32_TSC_ADJUST) }
}
