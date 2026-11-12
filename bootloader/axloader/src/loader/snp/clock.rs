//! Monotonic network deadlines from the current target's timebase.

use smoltcp::time::Instant;

use crate::loader::arch;

#[derive(Clone, Copy)]
pub(super) struct Clock {
    frequency: u64,
    origin: u64,
}

impl Clock {
    pub(super) fn open() -> uefi::Result<Self> {
        let frequency = arch::clock_frequency()?;
        Ok(Self {
            frequency,
            origin: arch::clock_ticks(),
        })
    }

    pub(super) fn now(self) -> Instant {
        let milliseconds = u128::from(arch::clock_ticks().wrapping_sub(self.origin)) * 1000
            / u128::from(self.frequency);
        Instant::from_millis(i64::try_from(milliseconds).unwrap_or(i64::MAX))
    }
}
