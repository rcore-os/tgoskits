//! Timer state transitions and generation-checked host scheduling.

use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{AtomicU64, Ordering};

use aarch64_cpu::registers::{CNTFRQ_EL0, CNTPCT_EL0, Readable};
use ax_kspin::SpinNoIrq;
use axdevice::{IrqLine, IrqResult};

use crate::host::{HostTime, default_host};

const CONTROL_ENABLE: u32 = 1 << 0;
const CONTROL_MASK: u32 = 1 << 1;
const CONTROL_STATUS: u32 = 1 << 2;
const NANOS_PER_SECOND: u128 = 1_000_000_000;

pub(super) struct VirtualPhysicalTimer {
    line: IrqLine,
    frequency: u64,
    state: SpinNoIrq<TimerState>,
    generation: AtomicU64,
    scheduled_token: SpinNoIrq<Option<usize>>,
}

impl VirtualPhysicalTimer {
    pub(super) const fn new(line: IrqLine, frequency: u64) -> Self {
        Self {
            line,
            frequency,
            state: SpinNoIrq::new(TimerState {
                compare: u64::MAX,
                control: 0,
            }),
            generation: AtomicU64::new(0),
            scheduled_token: SpinNoIrq::new(None),
        }
    }

    pub(super) fn read_control(&self) -> u64 {
        let state = *self.state.lock();
        let status = state.enabled() && physical_counter() >= state.compare;
        u64::from(state.control | if status { CONTROL_STATUS } else { 0 })
    }

    pub(super) fn read_tval(&self) -> u64 {
        u64::from(self.state.lock().compare.wrapping_sub(physical_counter()) as u32)
    }

    pub(super) fn read_compare(&self) -> u64 {
        self.state.lock().compare
    }

    pub(super) fn write_control(self: &Arc<Self>, value: u32) -> IrqResult {
        self.update(|state| state.control = value & (CONTROL_ENABLE | CONTROL_MASK))
    }

    pub(super) fn write_tval(self: &Arc<Self>, value: u32) -> IrqResult {
        let delta = i64::from(value as i32);
        self.update(|state| state.compare = physical_counter().wrapping_add_signed(delta))
    }

    pub(super) fn write_compare(self: &Arc<Self>, value: u64) -> IrqResult {
        self.update(|state| state.compare = value)
    }

    fn update(self: &Arc<Self>, update: impl FnOnce(&mut TimerState)) -> IrqResult {
        let action = {
            let mut state = self.state.lock();
            update(&mut state);
            let generation = self
                .generation
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1);
            state.action(physical_counter(), generation)
        };
        self.apply(action)
    }

    fn apply(self: &Arc<Self>, action: TimerAction) -> IrqResult {
        self.cancel_scheduled();
        match action {
            TimerAction::Lower => self.line.lower(),
            TimerAction::Raise => self.line.raise(),
            TimerAction::Schedule {
                compare,
                generation,
            } => {
                self.line.lower()?;
                self.schedule(compare, generation);
                Ok(())
            }
        }
    }

    fn schedule(self: &Arc<Self>, compare: u64, generation: u64) {
        let deadline = host_deadline_ns(compare, self.frequency);
        let timer = Arc::downgrade(self);
        let token = super::super::vm_timer_scheduler::register(
            deadline,
            Box::new(move |_| {
                let Some(timer) = timer.upgrade() else {
                    return;
                };
                if let Err(error) = timer.expire(generation) {
                    warn!("failed to deliver AArch64 physical timer interrupt: {error}");
                }
            }),
        );
        let (stale, previous) = {
            // Generation publication and slot replacement share this critical
            // section. A concurrent reprogram increments `generation` before
            // taking the slot, so it either makes this install stale or removes
            // the token immediately afterwards.
            let mut scheduled = self.scheduled_token.lock();
            if self.generation.load(Ordering::Acquire) != generation {
                (true, None)
            } else {
                (false, scheduled.replace(token))
            }
        };
        if stale {
            super::super::vm_timer_scheduler::cancel(token);
            return;
        }
        if let Some(previous) = previous {
            super::super::vm_timer_scheduler::cancel(previous);
        }
    }

    fn cancel_scheduled(&self) {
        let token = self.scheduled_token.lock().take();
        if let Some(token) = token {
            super::super::vm_timer_scheduler::cancel(token);
        }
    }

    fn expire(self: &Arc<Self>, generation: u64) -> IrqResult {
        if self.generation.load(Ordering::Acquire) != generation {
            return Ok(());
        }
        let action = self.state.lock().action(physical_counter(), generation);
        self.apply(action)
    }
}

#[derive(Clone, Copy)]
struct TimerState {
    compare: u64,
    control: u32,
}

impl TimerState {
    const fn enabled(self) -> bool {
        self.control & CONTROL_ENABLE != 0
    }

    const fn masked(self) -> bool {
        self.control & CONTROL_MASK != 0
    }

    fn action(self, now: u64, generation: u64) -> TimerAction {
        if !self.enabled() || self.masked() {
            TimerAction::Lower
        } else if now >= self.compare {
            TimerAction::Raise
        } else {
            TimerAction::Schedule {
                compare: self.compare,
                generation,
            }
        }
    }
}

enum TimerAction {
    Lower,
    Raise,
    Schedule { compare: u64, generation: u64 },
}

pub(super) fn physical_counter() -> u64 {
    CNTPCT_EL0.get()
}

pub(super) fn counter_frequency() -> u64 {
    CNTFRQ_EL0.get()
}

fn host_deadline_ns(compare: u64, frequency: u64) -> u64 {
    let now_counter = physical_counter();
    let now_ns = default_host().monotonic_time().as_nanos();
    let delta_ticks = compare.saturating_sub(now_counter) as u128;
    let delta_ns = delta_ticks
        .saturating_mul(NANOS_PER_SECOND)
        .saturating_add(u128::from(frequency - 1))
        / u128::from(frequency);
    now_ns.saturating_add(delta_ns).min(u128::from(u64::MAX)) as u64
}
