//! CPU constant counter and its CPUCFG frequency description.

/// Reads the architecture's constant timer counter.
#[inline]
pub fn read_counter() -> u64 {
    loongArch64::time::Time::read() as u64
}

/// Reads the constant-counter frequency described by CPUCFG, in Hz.
/// Platform validation of the advertised frequency remains with the caller.
pub fn counter_frequency() -> u64 {
    loongArch64::time::get_timer_freq() as u64
}

/// Acknowledges the current CPU's timer interrupt without changing its comparator.
pub fn acknowledge_interrupt() {
    // SAFETY: TICLR.TI is write-one-to-clear and changes only the local timer latch.
    unsafe { super::registers::write_csr::<0x44>(1) };
}
