//! Platform-owned scheduler clock correction.
//!
//! A stable system counter can be sampled directly on the calling CPU. An
//! unstable per-CPU counter instead advances only its local published clock;
//! remote readers couple the calling and target publications without reading
//! the target CPU's hardware counter.

use core::{
    hint::spin_loop,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};

use crate::time::{SchedulerClockError, SchedulerClockStability};

const CPU_OFFLINE: u8 = 0;
const CPU_INITIALIZING: u8 = 1;
const CPU_ONLINE: u8 = 2;

const CLOCK_UNINITIALIZED: u8 = 0;
const CLOCK_STABLE: u8 = 1;
const CLOCK_TRANSITIONING: u8 = 2;
const CLOCK_UNSTABLE: u8 = 3;

static SCHEDULER_CLOCK: SchedulerClock = SchedulerClock::new();

#[ax_percpu::def_percpu]
static SCHEDULER_CLOCK_CPU: CpuSchedulerClock = CpuSchedulerClock::new();

#[derive(Debug)]
pub(crate) struct CpuSchedulerClock {
    lifecycle: AtomicU8,
    anchor_version: AtomicU64,
    anchor_raw: AtomicU64,
    anchor_clock: AtomicU64,
    published: AtomicU64,
}

impl CpuSchedulerClock {
    pub(crate) const fn new() -> Self {
        Self {
            lifecycle: AtomicU8::new(CPU_OFFLINE),
            anchor_version: AtomicU64::new(0),
            anchor_raw: AtomicU64::new(0),
            anchor_clock: AtomicU64::new(0),
            published: AtomicU64::new(0),
        }
    }

    fn begin_online(&self) -> Result<(), SchedulerClockError> {
        self.lifecycle
            .compare_exchange(
                CPU_OFFLINE,
                CPU_INITIALIZING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| SchedulerClockError::CpuAlreadyOnline)
    }

    fn finish_online(&self) {
        self.lifecycle.store(CPU_ONLINE, Ordering::Release);
    }

    fn take_offline(&self) -> Result<(), SchedulerClockError> {
        self.lifecycle
            .compare_exchange(CPU_ONLINE, CPU_OFFLINE, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| SchedulerClockError::CpuOffline)
    }

    fn ensure_online(&self) -> Result<(), SchedulerClockError> {
        if self.lifecycle.load(Ordering::Acquire) == CPU_ONLINE {
            Ok(())
        } else {
            Err(SchedulerClockError::CpuOffline)
        }
    }

    fn initialize(&self, raw_clock: u64, clock: u64, generation: u64) {
        self.anchor_raw.store(raw_clock, Ordering::Relaxed);
        self.anchor_clock.store(clock, Ordering::Relaxed);
        self.published.store(clock, Ordering::Relaxed);
        self.anchor_version
            .store(generation.wrapping_mul(2), Ordering::Release);
    }

    fn ensure_anchor_generation(&self, raw_clock: u64, generation: u64) {
        let expected = generation.wrapping_mul(2);
        loop {
            let observed = self.anchor_version.load(Ordering::Acquire);
            if observed == expected {
                return;
            }
            if observed & 1 != 0 {
                spin_loop();
                continue;
            }
            if self
                .anchor_version
                .compare_exchange(observed, observed | 1, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                continue;
            }

            let continuity_clock = self.published.load(Ordering::Acquire);
            self.anchor_raw.store(raw_clock, Ordering::Relaxed);
            self.anchor_clock.store(continuity_clock, Ordering::Relaxed);
            self.anchor_version.store(expected, Ordering::Release);
            return;
        }
    }

    fn candidate_from_anchor(&self, raw_clock: u64) -> u64 {
        loop {
            let before = self.anchor_version.load(Ordering::Acquire);
            if before & 1 != 0 {
                spin_loop();
                continue;
            }
            let anchor_raw = self.anchor_raw.load(Ordering::Relaxed);
            let anchor_clock = self.anchor_clock.load(Ordering::Relaxed);
            let after = self.anchor_version.load(Ordering::Acquire);
            if before != after {
                continue;
            }

            let raw_delta = raw_clock.wrapping_sub(anchor_raw);
            return if raw_delta as i64 >= 0 {
                anchor_clock.wrapping_add(raw_delta)
            } else {
                anchor_clock
            };
        }
    }
}

#[derive(Debug)]
pub(crate) struct SchedulerClock {
    lifecycle_serialized: AtomicBool,
    mode: AtomicU8,
    generation: AtomicU64,
    global_clock: AtomicU64,
}

struct SchedulerClockLifecycleGuard<'a> {
    serialized: &'a AtomicBool,
}

impl Drop for SchedulerClockLifecycleGuard<'_> {
    fn drop(&mut self) {
        self.serialized.store(false, Ordering::Release);
    }
}

impl SchedulerClock {
    pub(crate) const fn new() -> Self {
        Self {
            lifecycle_serialized: AtomicBool::new(false),
            mode: AtomicU8::new(CLOCK_UNINITIALIZED),
            generation: AtomicU64::new(0),
            global_clock: AtomicU64::new(0),
        }
    }

    pub(crate) fn online_cpu(
        &self,
        cpu: &CpuSchedulerClock,
        raw_clock: u64,
        stability: SchedulerClockStability,
    ) -> Result<(), SchedulerClockError> {
        cpu.begin_online()?;
        let _lifecycle_guard = self.lock_lifecycle();
        let (mode, first_cpu) = self.observe_stability_serialized(stability);
        let generation = self.generation.load(Ordering::Acquire);
        let initial_clock = if first_cpu || mode == CLOCK_STABLE {
            raw_clock
        } else {
            self.global_clock.load(Ordering::Acquire)
        };
        cpu.initialize(raw_clock, initial_clock, generation);
        promote_clock(&self.global_clock, initial_clock);
        cpu.finish_online();
        Ok(())
    }

    pub(crate) fn offline_cpu(&self, cpu: &CpuSchedulerClock) -> Result<(), SchedulerClockError> {
        cpu.take_offline()
    }

    pub(crate) fn source(
        &self,
        calling: &CpuSchedulerClock,
        target: &CpuSchedulerClock,
        calling_raw_clock: u64,
    ) -> Result<u64, SchedulerClockError> {
        target.ensure_online()?;
        calling.ensure_online()?;
        match self.wait_for_mode() {
            CLOCK_STABLE => Ok(calling_raw_clock),
            CLOCK_UNSTABLE => {
                let local_clock = self.unstable_local_source(calling, calling_raw_clock);
                if core::ptr::eq(calling, target) {
                    Ok(local_clock)
                } else {
                    Ok(couple_clocks(calling, target))
                }
            }
            _ => unreachable!("scheduler clock mode must be initialized"),
        }
    }

    pub(crate) fn tick(
        &self,
        cpu: &CpuSchedulerClock,
        raw_clock: u64,
        stability: SchedulerClockStability,
    ) -> Result<u64, SchedulerClockError> {
        cpu.ensure_online()?;
        match self.observe_stability(stability).0 {
            CLOCK_STABLE => Ok(self.publish_stable_tick(cpu, raw_clock)),
            CLOCK_UNSTABLE => self.source(cpu, cpu, raw_clock),
            _ => unreachable!("scheduler clock mode must be initialized"),
        }
    }

    fn observe_stability(&self, stability: SchedulerClockStability) -> (u8, bool) {
        loop {
            match self.mode.load(Ordering::Acquire) {
                CLOCK_UNINITIALIZED => {
                    let _lifecycle_guard = self.lock_lifecycle();
                    return self.observe_stability_serialized(stability);
                }
                CLOCK_STABLE if stability == SchedulerClockStability::Unstable => {
                    let _lifecycle_guard = self.lock_lifecycle();
                    return self.observe_stability_serialized(stability);
                }
                CLOCK_STABLE => return (CLOCK_STABLE, false),
                CLOCK_UNSTABLE => return (CLOCK_UNSTABLE, false),
                CLOCK_TRANSITIONING => spin_loop(),
                _ => unreachable!("invalid scheduler clock mode"),
            }
        }
    }

    fn observe_stability_serialized(&self, stability: SchedulerClockStability) -> (u8, bool) {
        match self.mode.load(Ordering::Acquire) {
            CLOCK_UNINITIALIZED => {
                let initial_mode = match stability {
                    SchedulerClockStability::Stable => CLOCK_STABLE,
                    SchedulerClockStability::Unstable => CLOCK_UNSTABLE,
                };
                if initial_mode == CLOCK_UNSTABLE {
                    self.generation.store(1, Ordering::Relaxed);
                }
                self.mode.store(initial_mode, Ordering::Release);
                (initial_mode, true)
            }
            CLOCK_STABLE if stability == SchedulerClockStability::Unstable => {
                self.mode.store(CLOCK_TRANSITIONING, Ordering::Release);
                self.generation.fetch_add(1, Ordering::AcqRel);
                self.mode.store(CLOCK_UNSTABLE, Ordering::Release);
                (CLOCK_UNSTABLE, false)
            }
            CLOCK_STABLE => (CLOCK_STABLE, false),
            CLOCK_UNSTABLE => (CLOCK_UNSTABLE, false),
            CLOCK_TRANSITIONING => {
                unreachable!("scheduler clock transition holds lifecycle serialization")
            }
            _ => unreachable!("invalid scheduler clock mode"),
        }
    }

    fn lock_lifecycle(&self) -> SchedulerClockLifecycleGuard<'_> {
        while self
            .lifecycle_serialized
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
        SchedulerClockLifecycleGuard {
            serialized: &self.lifecycle_serialized,
        }
    }

    fn wait_for_mode(&self) -> u8 {
        loop {
            let mode = self.mode.load(Ordering::Acquire);
            if mode != CLOCK_TRANSITIONING {
                return mode;
            }
            spin_loop();
        }
    }

    fn publish_stable_tick(&self, cpu: &CpuSchedulerClock, raw_clock: u64) -> u64 {
        promote_clock(&cpu.published, raw_clock);
        promote_clock(&self.global_clock, raw_clock);
        raw_clock
    }

    fn unstable_local_source(&self, cpu: &CpuSchedulerClock, raw_clock: u64) -> u64 {
        let generation = self.generation.load(Ordering::Acquire);
        cpu.ensure_anchor_generation(raw_clock, generation);
        let candidate = cpu.candidate_from_anchor(raw_clock);
        let published = promote_clock(&cpu.published, candidate);
        promote_clock(&self.global_clock, published);
        published
    }
}

fn couple_clocks(calling: &CpuSchedulerClock, target: &CpuSchedulerClock) -> u64 {
    loop {
        let calling_clock = calling.published.load(Ordering::Acquire);
        let target_clock = target.published.load(Ordering::Acquire);
        if scheduler_clock_before(calling_clock, target_clock) {
            if promote_clock(&calling.published, target_clock) == target_clock {
                return target_clock;
            }
        } else if promote_clock(&target.published, calling_clock) == calling_clock {
            return calling_clock;
        }
    }
}

fn promote_clock(clock: &AtomicU64, candidate: u64) -> u64 {
    let mut observed = clock.load(Ordering::Acquire);
    loop {
        if !scheduler_clock_before(observed, candidate) {
            return observed;
        }
        match clock.compare_exchange_weak(observed, candidate, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => return candidate,
            Err(actual) => observed = actual,
        }
    }
}

const fn scheduler_clock_before(left: u64, right: u64) -> bool {
    (left.wrapping_sub(right) as i64) < 0
}

pub(crate) unsafe fn online_current_cpu(
    cpu_id: usize,
    raw_clock: u64,
    stability: SchedulerClockStability,
) -> Result<(), SchedulerClockError> {
    // SAFETY: the public boundary requires this CPU to remain offline and
    // non-migrating for the complete initialization transaction.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            ensure_current_cpu(pin, cpu_id)?;
            SCHEDULER_CLOCK_CPU.with_current(pin, |cpu| {
                SCHEDULER_CLOCK.online_cpu(cpu, raw_clock, stability)
            })
        })
    }
    .map_err(|_| SchedulerClockError::CurrentCpuUnavailable)?
}

pub(crate) unsafe fn offline_current_cpu(cpu_id: usize) -> Result<(), SchedulerClockError> {
    // SAFETY: the public boundary requires remote admission, migration and
    // local re-entry to be closed before the offline publication.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            ensure_current_cpu(pin, cpu_id)?;
            SCHEDULER_CLOCK_CPU.with_current(pin, |cpu| SCHEDULER_CLOCK.offline_cpu(cpu))
        })
    }
    .map_err(|_| SchedulerClockError::CurrentCpuUnavailable)?
}

pub(crate) unsafe fn source(
    target_cpu_id: usize,
    calling_raw_clock: u64,
) -> Result<u64, SchedulerClockError> {
    let target = cpu_state(target_cpu_id)?;
    // SAFETY: the public boundary requires a migration pin for the complete
    // local sample and remote publication coupling transaction.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            SCHEDULER_CLOCK_CPU.with_current(pin, |calling| {
                SCHEDULER_CLOCK.source(calling, target, calling_raw_clock)
            })
        })
    }
    .map_err(|_| SchedulerClockError::CurrentCpuUnavailable)?
}

pub(crate) unsafe fn tick(
    raw_clock: u64,
    stability: SchedulerClockStability,
) -> Result<u64, SchedulerClockError> {
    // SAFETY: the public boundary requires local IRQ/migration exclusion.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            SCHEDULER_CLOCK_CPU
                .with_current(pin, |cpu| SCHEDULER_CLOCK.tick(cpu, raw_clock, stability))
        })
    }
    .map_err(|_| SchedulerClockError::CurrentCpuUnavailable)?
}

fn cpu_state(cpu_id: usize) -> Result<&'static CpuSchedulerClock, SchedulerClockError> {
    let cpu_index = ax_percpu::CpuIndex::try_from(cpu_id)
        .map_err(|_| SchedulerClockError::InvalidCpu { cpu_id })?;
    let area =
        ax_percpu::area(cpu_index).map_err(|_| SchedulerClockError::InvalidCpu { cpu_id })?;
    let pointer = SCHEDULER_CLOCK_CPU.remote_ptr(area);
    // SAFETY: the frozen per-CPU layout constructs this Sync atomic object in
    // shutdown-lifetime storage. All mutable state is accessed atomically.
    Ok(unsafe { pointer.as_ref() })
}

fn ensure_current_cpu(
    pin: &ax_percpu::CpuPin<'_>,
    expected_cpu_id: usize,
) -> Result<(), SchedulerClockError> {
    let actual_cpu_id = ax_percpu::current_cpu_index(pin).as_usize();
    if actual_cpu_id == expected_cpu_id {
        Ok(())
    } else {
        Err(SchedulerClockError::WrongCurrentCpu {
            expected_cpu_id,
            actual_cpu_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_source_corrects_different_raw_counter_offsets() {
        let clock = SchedulerClock::new();
        let cpu0 = CpuSchedulerClock::new();
        let cpu1 = CpuSchedulerClock::new();

        clock
            .online_cpu(&cpu0, 10_000, SchedulerClockStability::Unstable)
            .unwrap();
        clock
            .online_cpu(&cpu1, 100, SchedulerClockStability::Unstable)
            .unwrap();

        assert_eq!(clock.source(&cpu1, &cpu0, 110).unwrap(), 10_010);
        assert_eq!(clock.source(&cpu0, &cpu0, 10_020).unwrap(), 10_020);
        assert_eq!(clock.source(&cpu1, &cpu0, 115).unwrap(), 10_020);
        assert_eq!(cpu1.published.load(Ordering::Acquire), 10_020);
    }

    #[test]
    fn stable_source_does_not_enter_per_cpu_correction() {
        let clock = SchedulerClock::new();
        let cpu0 = CpuSchedulerClock::new();
        let cpu1 = CpuSchedulerClock::new();

        clock
            .online_cpu(&cpu0, 100, SchedulerClockStability::Stable)
            .unwrap();
        clock
            .online_cpu(&cpu1, 100, SchedulerClockStability::Stable)
            .unwrap();

        assert_eq!(clock.source(&cpu0, &cpu1, 150).unwrap(), 150);
        assert_eq!(cpu0.published.load(Ordering::Acquire), 100);
        assert_eq!(cpu1.published.load(Ordering::Acquire), 100);
    }

    #[test]
    fn stable_to_unstable_transition_preserves_continuity() {
        let clock = SchedulerClock::new();
        let cpu = CpuSchedulerClock::new();

        clock
            .online_cpu(&cpu, 100, SchedulerClockStability::Stable)
            .unwrap();
        assert_eq!(
            clock
                .tick(&cpu, 150, SchedulerClockStability::Stable)
                .unwrap(),
            150
        );

        assert_eq!(
            clock
                .tick(&cpu, 10_000, SchedulerClockStability::Unstable)
                .unwrap(),
            150
        );
        assert_eq!(clock.source(&cpu, &cpu, 10_010).unwrap(), 160);
    }

    #[test]
    fn unstable_local_source_preserves_wrapping_counter_order() {
        let clock = SchedulerClock::new();
        let cpu = CpuSchedulerClock::new();

        clock
            .online_cpu(&cpu, u64::MAX - 5, SchedulerClockStability::Unstable)
            .unwrap();

        assert_eq!(clock.source(&cpu, &cpu, 2).unwrap(), 2);
    }

    #[test]
    fn offline_cpu_rejects_sources_and_reonline_reanchors() {
        let clock = SchedulerClock::new();
        let cpu0 = CpuSchedulerClock::new();
        let cpu1 = CpuSchedulerClock::new();

        clock
            .online_cpu(&cpu0, 1_000, SchedulerClockStability::Unstable)
            .unwrap();
        clock
            .online_cpu(&cpu1, 100, SchedulerClockStability::Unstable)
            .unwrap();
        clock.offline_cpu(&cpu1).unwrap();

        assert_eq!(
            clock.source(&cpu0, &cpu1, 1_010),
            Err(SchedulerClockError::CpuOffline)
        );

        clock
            .online_cpu(&cpu1, 20_000, SchedulerClockStability::Unstable)
            .unwrap();
        assert_eq!(clock.source(&cpu1, &cpu1, 20_010).unwrap(), 1_010);
    }

    #[test]
    fn concurrent_remote_coupling_publishes_one_comparable_clock() {
        let clock = SchedulerClock::new();
        let cpu0 = CpuSchedulerClock::new();
        let cpu1 = CpuSchedulerClock::new();
        clock
            .online_cpu(&cpu0, 10_000, SchedulerClockStability::Unstable)
            .unwrap();
        clock
            .online_cpu(&cpu1, 100, SchedulerClockStability::Unstable)
            .unwrap();

        std::thread::scope(|scope| {
            scope.spawn(|| {
                for raw_clock in 10_001..11_001 {
                    clock.source(&cpu0, &cpu1, raw_clock).unwrap();
                }
            });
            scope.spawn(|| {
                for raw_clock in 101..1_101 {
                    clock.source(&cpu1, &cpu0, raw_clock).unwrap();
                }
            });
        });

        assert_eq!(
            cpu0.published.load(Ordering::Acquire),
            cpu1.published.load(Ordering::Acquire)
        );
        assert!(cpu0.published.load(Ordering::Acquire) >= 11_000);
    }
}
