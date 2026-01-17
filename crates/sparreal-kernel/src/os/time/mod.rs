use core::time::Duration;

mod timer;

pub use timer::{TimerError, TimerHandle, one_shot_after, one_shot_at, time_list};

pub(crate) fn init() {
    crate::hal::al::cpu::systick_irq_disable();
    crate::hal::al::cpu::systick_enable();
    timer::init();
}

/// Time since the timer subsystem was initialised.
pub fn since_boot() -> Duration {
    let ticks = crate::hal::al::cpu::systick_ticks();
    ticks_to_duration(ticks)
}

pub fn ticks() -> usize {
    crate::hal::al::cpu::systick_ticks()
}

fn ticks_to_duration(ticks: usize) -> Duration {
    let freq = crate::hal::al::cpu::systick_frequency();
    let secs = ticks / freq;
    let nanos = ((ticks % freq) as u128 * 1_000_000_000u128) / (freq as u128);
    Duration::new(secs as u64, nanos as u32)
}

fn duration_to_ticks(dur: Duration) -> usize {
    let freq = crate::hal::al::cpu::systick_frequency();
    let secs_ticks = dur.as_secs().saturating_mul(freq as u64) as usize;
    let nanos_ticks = (dur.subsec_nanos() as u128 * (freq as u128)) / 1_000_000_000u128;
    secs_ticks.saturating_add(nanos_ticks as usize)
}
