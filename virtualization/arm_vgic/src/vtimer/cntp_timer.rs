#[cfg(target_arch = "aarch64")]
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
#[cfg(target_arch = "aarch64")]
use core::{arch::asm, time::Duration};

#[cfg(target_arch = "aarch64")]
use crate::host;

const CNTP_CTL_ENABLE: u32 = 1 << 0;
const CNTP_CTL_IMASK: u32 = 1 << 1;
const CNTP_CTL_ISTATUS: u32 = 1 << 2;
const CNTP_PPI: u8 = 30;
const NANOS_PER_SECOND: u64 = 1_000_000_000;
const TIMER_TOKEN_NONE: usize = usize::MAX;

pub(super) struct CntpTimerState {
    cval: AtomicU64,
    ctl: AtomicU32,
    generation: AtomicU64,
    timer_token: AtomicUsize,
}

impl CntpTimerState {
    pub(super) const fn new() -> Self {
        Self {
            cval: AtomicU64::new(0),
            ctl: AtomicU32::new(0),
            generation: AtomicU64::new(0),
            timer_token: AtomicUsize::new(TIMER_TOKEN_NONE),
        }
    }

    pub(super) fn read_cval(&self) -> u64 {
        self.cval.load(Ordering::Acquire)
    }

    pub(super) fn write_cval(self: &Arc<Self>, value: u64) {
        self.cval.store(value, Ordering::Release);
        self.rearm();
    }

    pub(super) fn read_ctl(&self) -> u32 {
        let ctl = self.ctl.load(Ordering::Acquire);
        let expired =
            ctl & CNTP_CTL_ENABLE != 0 && counter_ticks() >= self.cval.load(Ordering::Acquire);

        if expired {
            ctl | CNTP_CTL_ISTATUS
        } else {
            ctl & !CNTP_CTL_ISTATUS
        }
    }

    pub(super) fn write_ctl(self: &Arc<Self>, value: u32) {
        self.ctl.store(
            value & (CNTP_CTL_ENABLE | CNTP_CTL_IMASK),
            Ordering::Release,
        );
        self.rearm();
    }

    pub(super) fn read_tval(&self) -> u32 {
        self.cval
            .load(Ordering::Acquire)
            .wrapping_sub(counter_ticks()) as u32
    }

    pub(super) fn write_tval(self: &Arc<Self>, value: u32) {
        let delta = value as i32 as i64;
        let now = counter_ticks();
        let cval = if delta >= 0 {
            now.wrapping_add(delta as u64)
        } else {
            now.wrapping_sub(delta.unsigned_abs())
        };

        self.cval.store(cval, Ordering::Release);
        self.rearm();
    }

    #[cfg(target_arch = "aarch64")]
    fn rearm(self: &Arc<Self>) {
        let generation = self
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        self.cancel_pending_timer();
        let ctl = self.ctl.load(Ordering::Acquire);
        if ctl & CNTP_CTL_ENABLE == 0 || ctl & CNTP_CTL_IMASK != 0 {
            return;
        }

        let now_ticks = counter_ticks();
        let deadline_ticks = self.cval.load(Ordering::Acquire);
        let delay_ticks = deadline_ticks.saturating_sub(now_ticks);
        let delay_ns = ticks_to_nanos_ceil(delay_ticks, counter_frequency_hz());
        let deadline_ns = host::current_time_nanos().saturating_add(delay_ns);
        let state = Arc::clone(self);

        let token = host::register_timer(
            Duration::from_nanos(deadline_ns),
            Box::new(move |_| state.fire(generation)),
        );
        self.timer_token.store(token, Ordering::Release);
    }

    #[cfg(not(target_arch = "aarch64"))]
    fn rearm(self: &Arc<Self>) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.timer_token.store(TIMER_TOKEN_NONE, Ordering::Release);
    }

    #[cfg(target_arch = "aarch64")]
    fn cancel_pending_timer(&self) {
        let token = self.timer_token.swap(TIMER_TOKEN_NONE, Ordering::AcqRel);
        if token != TIMER_TOKEN_NONE {
            host::cancel_timer(token);
        }
    }

    #[cfg(target_arch = "aarch64")]
    fn fire(self: &Arc<Self>, expected_generation: u64) {
        if self.generation.load(Ordering::Acquire) != expected_generation {
            return;
        }

        let ctl = self.ctl.load(Ordering::Acquire);
        if ctl & CNTP_CTL_ENABLE == 0 || ctl & CNTP_CTL_IMASK != 0 {
            return;
        }

        if counter_ticks() < self.cval.load(Ordering::Acquire) {
            self.rearm();
            return;
        }

        crate::api_reexp::hardware_inject_virtual_interrupt(CNTP_PPI);
    }
}

#[cfg(target_arch = "aarch64")]
fn counter_ticks() -> u64 {
    let value: u64;

    unsafe {
        asm!("mrs {value}, CNTPCT_EL0", value = out(reg) value);
    }
    value
}

#[cfg(not(target_arch = "aarch64"))]
fn counter_ticks() -> u64 {
    0
}

#[cfg(target_arch = "aarch64")]
fn counter_frequency_hz() -> u64 {
    let value: u64;

    unsafe {
        asm!("mrs {value}, CNTFRQ_EL0", value = out(reg) value);
    }
    value
}

#[cfg(not(target_arch = "aarch64"))]
fn counter_frequency_hz() -> u64 {
    NANOS_PER_SECOND
}

fn ticks_to_nanos_ceil(ticks: u64, frequency_hz: u64) -> u64 {
    if ticks == 0 {
        return 0;
    }

    let frequency_hz = frequency_hz.max(1);
    let numerator = u128::from(ticks) * u128::from(NANOS_PER_SECOND) + u128::from(frequency_hz - 1);
    let nanos = numerator / u128::from(frequency_hz);

    nanos.min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_counter_ticks_with_ceiling() {
        assert_eq!(ticks_to_nanos_ceil(0, 100), 0);
        assert_eq!(ticks_to_nanos_ceil(1, 3), 333_333_334);
        assert_eq!(ticks_to_nanos_ceil(24_000, 24_000_000), 1_000_000);
    }

    #[test]
    fn clamps_zero_frequency() {
        assert_eq!(ticks_to_nanos_ceil(1, 0), NANOS_PER_SECOND);
    }
}
