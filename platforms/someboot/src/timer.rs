use core::time::Duration;

use crate::ArchTrait;

const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Hardware counter contract exposed to the platform scheduler clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CounterStability {
    /// Every runtime CPU observes one synchronized system counter.
    Stable,
    /// The counter is CPU-local and requires per-CPU correction.
    Unstable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ArchTimerMode {
    El1Phys    = 0,
    El1Virt    = 1,
    El2HypPhys = 2,
}

impl ArchTimerMode {
    pub const fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::El1Virt,
            2 => Self::El2HypPhys,
            _ => Self::El1Phys,
        }
    }
}

static mut ARCH_TIMER_MODE: u8 = ArchTimerMode::El1Phys as u8;

pub const fn select_aarch64_timer_mode(kernel_in_el2: bool, el2_available: bool) -> ArchTimerMode {
    if kernel_in_el2 {
        ArchTimerMode::El2HypPhys
    } else if el2_available {
        ArchTimerMode::El1Phys
    } else {
        ArchTimerMode::El1Virt
    }
}

pub const fn aarch64_timer_irq_index(mode: ArchTimerMode) -> usize {
    match mode {
        ArchTimerMode::El1Phys => 1,
        ArchTimerMode::El1Virt => 2,
        ArchTimerMode::El2HypPhys => 3,
    }
}

pub fn set_aarch64_timer_mode(mode: ArchTimerMode) {
    // Written once by the primary CPU during early boot before secondary CPUs run.
    unsafe { ARCH_TIMER_MODE = mode as u8 };
}

pub fn aarch64_timer_mode() -> ArchTimerMode {
    // After early boot this mode is read-only platform state.
    unsafe { ArchTimerMode::from_raw(ARCH_TIMER_MODE) }
}

/// Enable the platform system timer so that timer IRQs can fire.
pub fn enable() {
    crate::arch::Arch::systimer_enable();
}

/// Disable the platform system timer to stop timer IRQs.
pub fn irq_disable() {
    crate::arch::Arch::systimer_irq_disable();
}

pub fn irq_enable() {
    crate::arch::Arch::systimer_irq_enable();
}

pub fn irq_is_enabled() -> bool {
    crate::arch::Arch::systimer_irq_is_enabled()
}

/// Configure the system timer with the desired interval.
pub fn set_next_event(interval: Duration) {
    let ticks = duration_to_ticks(interval);
    crate::arch::Arch::systimer_set_interval(ticks);
}

pub fn set_next_event_in_ticks(ticks: usize) {
    crate::arch::Arch::systimer_set_interval(ticks);
}

#[cfg(any(target_arch = "aarch64", test))]
pub(crate) mod aarch64_deadline {
    /// Converts a relative timer interval into an absolute counter compare value.
    ///
    /// Architectural counters and compare registers wrap together, so this must
    /// use wrapping rather than saturating arithmetic.
    pub(crate) const fn from_interval(current_ticks: u64, interval_ticks: u64) -> u64 {
        current_ticks.wrapping_add(interval_ticks)
    }

    #[cfg(any(not(feature = "hv"), test))]
    pub(crate) mod el1 {
        use super::{super::ArchTimerMode, from_interval};

        pub(crate) trait TimerRegisters {
            fn read_virtual_counter(&self) -> u64;
            fn read_physical_counter(&self) -> u64;
            fn write_virtual_compare(&self, deadline: u64);
            fn write_physical_compare(&self, deadline: u64);
        }

        pub(crate) fn program(
            registers: &impl TimerRegisters,
            mode: ArchTimerMode,
            interval_ticks: u64,
        ) {
            match mode {
                ArchTimerMode::El1Virt => registers.write_virtual_compare(from_interval(
                    registers.read_virtual_counter(),
                    interval_ticks,
                )),
                ArchTimerMode::El1Phys | ArchTimerMode::El2HypPhys => registers
                    .write_physical_compare(from_interval(
                        registers.read_physical_counter(),
                        interval_ticks,
                    )),
            }
        }
    }

    #[cfg(any(feature = "hv", test))]
    pub(crate) mod el2 {
        use super::from_interval;

        pub(crate) trait TimerRegisters {
            fn read_physical_counter(&self) -> u64;
            fn write_hyp_physical_compare(&self, deadline: u64);
        }

        pub(crate) fn program(registers: &impl TimerRegisters, interval_ticks: u64) {
            registers.write_hyp_physical_compare(from_interval(
                registers.read_physical_counter(),
                interval_ticks,
            ));
        }
    }
}

/// Acknowledge and clear the timer interrupt.
/// This must be called in the timer interrupt handler.
pub fn ack() {
    crate::arch::Arch::systimer_ack();
}

pub fn since_boot() -> Duration {
    elapsed()
}

/// Get the timer frequency in Hz.
#[inline]
pub fn freq() -> usize {
    crate::arch::Arch::systimer_freq()
}

/// Get the current timer tick count.
#[inline]
pub fn ticks() -> usize {
    crate::arch::Arch::systimer_tick()
}

/// Reports whether scheduler users may sample the raw counter on any CPU.
#[inline]
pub fn scheduler_clock_stability() -> CounterStability {
    crate::arch::Arch::systimer_stability()
}

/// Convert ticks to Duration.
#[inline]
pub fn ticks_to_duration(ticks: usize) -> Duration {
    let freq = freq();
    if freq == 0 {
        return Duration::ZERO;
    }
    // ticks * 1_000_000_000 / freq
    // Use u128 to avoid overflow
    let nanos = (ticks as u128 * NANOS_PER_SEC as u128) / freq as u128;
    Duration::from_nanos(nanos as u64)
}

/// Convert Duration to ticks.
#[inline]
pub fn duration_to_ticks(duration: Duration) -> usize {
    let freq = freq();
    if freq == 0 {
        return 0;
    }
    // duration.as_nanos() * freq / 1_000_000_000
    // Use u128 to avoid overflow
    let ticks = (duration.as_nanos() * freq as u128) / NANOS_PER_SEC as u128;
    ticks as _
}

/// Get the elapsed time since boot.
#[inline]
pub fn elapsed() -> Duration {
    ticks_to_duration(ticks())
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::{
        aarch64_deadline::{
            self,
            el1::{self, TimerRegisters as El1TimerRegisters},
            el2::{self, TimerRegisters as El2TimerRegisters},
        },
        *,
    };

    #[test]
    fn el2_kernel_uses_hyp_physical_timer() {
        assert_eq!(
            select_aarch64_timer_mode(true, true),
            ArchTimerMode::El2HypPhys
        );
        assert_eq!(
            select_aarch64_timer_mode(true, false),
            ArchTimerMode::El2HypPhys
        );
    }

    #[test]
    fn el1_kernel_uses_physical_timer_when_el2_is_available() {
        assert_eq!(
            select_aarch64_timer_mode(false, true),
            ArchTimerMode::El1Phys
        );
    }

    #[test]
    fn el1_kernel_uses_virtual_timer_when_el2_is_unavailable() {
        assert_eq!(
            select_aarch64_timer_mode(false, false),
            ArchTimerMode::El1Virt
        );
    }

    #[test]
    fn timer_mode_maps_to_fdt_interrupt_index() {
        assert_eq!(aarch64_timer_irq_index(ArchTimerMode::El1Phys), 1);
        assert_eq!(aarch64_timer_irq_index(ArchTimerMode::El1Virt), 2);
        assert_eq!(aarch64_timer_irq_index(ArchTimerMode::El2HypPhys), 3);
    }

    #[test]
    fn compare_value_preserves_intervals_beyond_tval_width() {
        let current = 0x1234_5678_0000_0000;
        let interval = u32::MAX as u64 + 17;

        assert_eq!(
            aarch64_deadline::from_interval(current, interval),
            current + interval
        );
        assert_eq!(aarch64_deadline::from_interval(u64::MAX - 3, 8), 4);
    }

    #[test]
    fn el1_virtual_timer_uses_virtual_counter_and_compare_register() {
        let registers = RecordingEl1TimerRegisters::new(0x1234_5678_0000_0000, 17);
        let interval = u32::MAX as u64 + 17;

        el1::program(&registers, ArchTimerMode::El1Virt, interval);

        assert_eq!(registers.virtual_compare.get(), Some(0x1234_5679_0000_0010));
        assert_eq!(registers.physical_compare.get(), None);
        assert_eq!(registers.virtual_counter_reads.get(), 1);
        assert_eq!(registers.physical_counter_reads.get(), 0);
    }

    #[test]
    fn el1_physical_timer_uses_physical_counter_and_compare_register() {
        let registers = RecordingEl1TimerRegisters::new(17, u64::MAX - 3);

        el1::program(&registers, ArchTimerMode::El1Phys, 8);

        assert_eq!(registers.virtual_compare.get(), None);
        assert_eq!(registers.physical_compare.get(), Some(4));
        assert_eq!(registers.virtual_counter_reads.get(), 0);
        assert_eq!(registers.physical_counter_reads.get(), 1);
    }

    #[test]
    fn el2_hyp_timer_uses_physical_counter_and_hyp_compare_register() {
        let registers = RecordingEl2TimerRegisters::new(u64::MAX - 3);

        el2::program(&registers, 8);

        assert_eq!(registers.hyp_physical_compare.get(), Some(4));
        assert_eq!(registers.physical_counter_reads.get(), 1);
    }

    struct RecordingEl1TimerRegisters {
        virtual_counter: u64,
        physical_counter: u64,
        virtual_counter_reads: Cell<usize>,
        physical_counter_reads: Cell<usize>,
        virtual_compare: Cell<Option<u64>>,
        physical_compare: Cell<Option<u64>>,
    }

    impl RecordingEl1TimerRegisters {
        fn new(virtual_counter: u64, physical_counter: u64) -> Self {
            Self {
                virtual_counter,
                physical_counter,
                virtual_counter_reads: Cell::new(0),
                physical_counter_reads: Cell::new(0),
                virtual_compare: Cell::new(None),
                physical_compare: Cell::new(None),
            }
        }
    }

    impl El1TimerRegisters for RecordingEl1TimerRegisters {
        fn read_virtual_counter(&self) -> u64 {
            self.virtual_counter_reads
                .set(self.virtual_counter_reads.get() + 1);
            self.virtual_counter
        }

        fn read_physical_counter(&self) -> u64 {
            self.physical_counter_reads
                .set(self.physical_counter_reads.get() + 1);
            self.physical_counter
        }

        fn write_virtual_compare(&self, deadline: u64) {
            self.virtual_compare.set(Some(deadline));
        }

        fn write_physical_compare(&self, deadline: u64) {
            self.physical_compare.set(Some(deadline));
        }
    }

    struct RecordingEl2TimerRegisters {
        physical_counter: u64,
        physical_counter_reads: Cell<usize>,
        hyp_physical_compare: Cell<Option<u64>>,
    }

    impl RecordingEl2TimerRegisters {
        fn new(physical_counter: u64) -> Self {
            Self {
                physical_counter,
                physical_counter_reads: Cell::new(0),
                hyp_physical_compare: Cell::new(None),
            }
        }
    }

    impl El2TimerRegisters for RecordingEl2TimerRegisters {
        fn read_physical_counter(&self) -> u64 {
            self.physical_counter_reads
                .set(self.physical_counter_reads.get() + 1);
            self.physical_counter
        }

        fn write_hyp_physical_compare(&self, deadline: u64) {
            self.hyp_physical_compare.set(Some(deadline));
        }
    }
}
