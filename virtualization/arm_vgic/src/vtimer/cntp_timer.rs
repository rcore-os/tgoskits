#[cfg(target_arch = "aarch64")]
use alloc::boxed::Box;
use alloc::{collections::BTreeMap, sync::Arc};
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
#[cfg(target_arch = "aarch64")]
use core::{arch::asm, time::Duration};
#[cfg(test)]
use std::sync::{Mutex as TimerBankLock, MutexGuard as TimerBankLockGuard};

#[cfg(not(test))]
use ax_kspin::{SpinNoIrq as TimerBankLock, SpinNoIrqGuard as TimerBankLockGuard};

#[cfg(target_arch = "aarch64")]
use crate::host;

const CNTP_CTL_ENABLE: u32 = 1 << 0;
const CNTP_CTL_IMASK: u32 = 1 << 1;
const CNTP_CTL_ISTATUS: u32 = 1 << 2;
#[cfg(any(test, target_arch = "aarch64"))]
const CNTP_PPI: u8 = 30;
#[cfg(any(test, target_arch = "aarch64"))]
const NANOS_PER_SECOND: u64 = 1_000_000_000;
const TIMER_TOKEN_NONE: usize = usize::MAX;
const TIMER_REMAINING_NONE: u64 = u64::MAX;

pub struct CntpTimerState {
    banks: TimerBankLock<BTreeMap<(usize, usize), Arc<CntpTimerBank>>>,
    #[cfg(test)]
    test_identity: TimerBankLock<(usize, usize)>,
    #[cfg(test)]
    test_counter_ticks: AtomicU64,
}

struct CntpTimerBank {
    #[cfg(any(test, target_arch = "aarch64"))]
    vm_id: usize,
    #[cfg(any(test, target_arch = "aarch64"))]
    vcpu_id: usize,
    cval: AtomicU64,
    ctl: AtomicU32,
    generation: AtomicU64,
    timer_token: AtomicUsize,
    suspended_remaining_ticks: AtomicU64,
    lifecycle: TimerBankLock<()>,
    #[cfg(test)]
    test_counter_ticks: AtomicU64,
}

impl CntpTimerState {
    pub(super) fn new() -> Self {
        Self {
            banks: TimerBankLock::new(BTreeMap::new()),
            #[cfg(test)]
            test_identity: TimerBankLock::new((0, 0)),
            #[cfg(test)]
            test_counter_ticks: AtomicU64::new(0),
        }
    }

    pub(super) fn read_cval(&self) -> u64 {
        self.current_bank().read_cval()
    }

    pub(super) fn write_cval(&self, value: u64) {
        self.current_bank().write_cval(value);
    }

    pub(super) fn read_ctl(&self) -> u32 {
        self.current_bank().read_ctl()
    }

    pub(super) fn write_ctl(&self, value: u32) {
        self.current_bank().write_ctl(value);
    }

    pub(super) fn read_tval(&self) -> u32 {
        self.current_bank().read_tval()
    }

    pub(super) fn write_tval(&self, value: u32) {
        self.current_bank().write_tval(value);
    }

    pub fn reset(&self) {
        let banks = self.snapshot_banks();
        for bank in &banks {
            bank.reset();
        }
        lock_timer_banks(&self.banks).clear();
    }

    pub fn suspend(&self) {
        for bank in self.snapshot_banks() {
            bank.suspend();
        }
    }

    pub fn resume(&self) {
        for bank in self.snapshot_banks() {
            bank.resume();
        }
    }

    fn current_bank(&self) -> Arc<CntpTimerBank> {
        let (vm_id, vcpu_id) = self.current_timer_identity();
        #[cfg(test)]
        let test_counter_ticks = self.test_counter_ticks.load(Ordering::Acquire);
        let mut banks = lock_timer_banks(&self.banks);
        Arc::clone(banks.entry((vm_id, vcpu_id)).or_insert_with(|| {
            let bank = Arc::new(CntpTimerBank::new(vm_id, vcpu_id));
            #[cfg(test)]
            bank.set_test_counter_ticks(test_counter_ticks);
            bank
        }))
    }

    fn snapshot_banks(&self) -> alloc::vec::Vec<Arc<CntpTimerBank>> {
        lock_timer_banks(&self.banks).values().cloned().collect()
    }

    #[cfg(test)]
    fn current_timer_identity(&self) -> (usize, usize) {
        *lock_test_identity(&self.test_identity)
    }

    #[cfg(not(test))]
    fn current_timer_identity(&self) -> (usize, usize) {
        current_timer_identity()
    }

    #[cfg(test)]
    pub(super) fn set_test_current_identity(&self, vm_id: usize, vcpu_id: usize) {
        *lock_test_identity(&self.test_identity) = (vm_id, vcpu_id);
    }

    #[cfg(test)]
    pub(super) fn current_fire_target_for_test(&self) -> Option<(usize, usize, u8)> {
        let bank = self.current_bank();
        bank.fire_target(bank.generation.load(Ordering::Acquire))
    }

    #[cfg(test)]
    pub(super) fn set_test_counter_ticks(&self, ticks: u64) {
        self.test_counter_ticks.store(ticks, Ordering::Release);
        for bank in self.snapshot_banks() {
            bank.set_test_counter_ticks(ticks);
        }
    }
}

impl Default for CntpTimerState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
fn lock_timer_banks(
    banks: &TimerBankLock<BTreeMap<(usize, usize), Arc<CntpTimerBank>>>,
) -> TimerBankLockGuard<'_, BTreeMap<(usize, usize), Arc<CntpTimerBank>>> {
    banks.lock().expect("CNTP timer test lock poisoned")
}

#[cfg(not(test))]
fn lock_timer_banks(
    banks: &TimerBankLock<BTreeMap<(usize, usize), Arc<CntpTimerBank>>>,
) -> TimerBankLockGuard<'_, BTreeMap<(usize, usize), Arc<CntpTimerBank>>> {
    banks.lock()
}

#[cfg(test)]
fn lock_test_identity(
    identity: &TimerBankLock<(usize, usize)>,
) -> TimerBankLockGuard<'_, (usize, usize)> {
    identity
        .lock()
        .expect("CNTP timer test identity lock poisoned")
}

#[cfg(test)]
fn lock_timer_bank(bank: &TimerBankLock<()>) -> TimerBankLockGuard<'_, ()> {
    bank.lock().expect("CNTP timer test bank lock poisoned")
}

#[cfg(not(test))]
fn lock_timer_bank(bank: &TimerBankLock<()>) -> TimerBankLockGuard<'_, ()> {
    bank.lock()
}

impl CntpTimerBank {
    fn new(vm_id: usize, vcpu_id: usize) -> Self {
        #[cfg(not(any(test, target_arch = "aarch64")))]
        let _ = (vm_id, vcpu_id);

        Self {
            #[cfg(any(test, target_arch = "aarch64"))]
            vm_id,
            #[cfg(any(test, target_arch = "aarch64"))]
            vcpu_id,
            cval: AtomicU64::new(0),
            ctl: AtomicU32::new(0),
            generation: AtomicU64::new(0),
            timer_token: AtomicUsize::new(TIMER_TOKEN_NONE),
            suspended_remaining_ticks: AtomicU64::new(TIMER_REMAINING_NONE),
            lifecycle: TimerBankLock::new(()),
            #[cfg(test)]
            test_counter_ticks: AtomicU64::new(0),
        }
    }

    fn read_cval(&self) -> u64 {
        self.cval.load(Ordering::Acquire)
    }

    fn write_cval(self: &Arc<Self>, value: u64) {
        let _guard = lock_timer_bank(&self.lifecycle);
        self.suspended_remaining_ticks
            .store(TIMER_REMAINING_NONE, Ordering::Release);
        self.cval.store(value, Ordering::Release);
        self.rearm_locked();
    }

    fn read_ctl(&self) -> u32 {
        let ctl = self.ctl.load(Ordering::Acquire);
        let expired =
            ctl & CNTP_CTL_ENABLE != 0 && self.counter_ticks() >= self.cval.load(Ordering::Acquire);

        if expired {
            ctl | CNTP_CTL_ISTATUS
        } else {
            ctl & !CNTP_CTL_ISTATUS
        }
    }

    fn write_ctl(self: &Arc<Self>, value: u32) {
        let _guard = lock_timer_bank(&self.lifecycle);
        self.suspended_remaining_ticks
            .store(TIMER_REMAINING_NONE, Ordering::Release);
        self.ctl.store(
            value & (CNTP_CTL_ENABLE | CNTP_CTL_IMASK),
            Ordering::Release,
        );
        self.rearm_locked();
    }

    fn read_tval(&self) -> u32 {
        self.cval
            .load(Ordering::Acquire)
            .wrapping_sub(self.counter_ticks()) as u32
    }

    fn write_tval(self: &Arc<Self>, value: u32) {
        let _guard = lock_timer_bank(&self.lifecycle);
        let delta = value as i32 as i64;
        let now = self.counter_ticks();
        let cval = if delta >= 0 {
            now.wrapping_add(delta as u64)
        } else {
            now.wrapping_sub(delta.unsigned_abs())
        };

        self.suspended_remaining_ticks
            .store(TIMER_REMAINING_NONE, Ordering::Release);
        self.cval.store(cval, Ordering::Release);
        self.rearm_locked();
    }

    #[cfg(target_arch = "aarch64")]
    fn rearm_locked(self: &Arc<Self>) {
        let generation = self
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        self.cancel_pending_timer_locked();
        let ctl = self.ctl.load(Ordering::Acquire);
        if ctl & CNTP_CTL_ENABLE == 0 || ctl & CNTP_CTL_IMASK != 0 {
            return;
        }

        let now_ticks = self.counter_ticks();
        let deadline_ticks = self.cval.load(Ordering::Acquire);
        let delay_ticks = deadline_ticks.saturating_sub(now_ticks);
        let delay_ns = ticks_to_nanos_ceil(delay_ticks, counter_frequency_hz());
        let deadline_ns = host::current_time_nanos().saturating_add(delay_ns);
        let bank = Arc::clone(self);

        let token = host::register_timer(
            Duration::from_nanos(deadline_ns),
            Box::new(move |_| bank.fire(generation)),
        );
        self.timer_token.store(token, Ordering::Release);
    }

    #[cfg(not(target_arch = "aarch64"))]
    fn rearm_locked(self: &Arc<Self>) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.timer_token.store(TIMER_TOKEN_NONE, Ordering::Release);
    }

    #[cfg(target_arch = "aarch64")]
    fn cancel_pending_timer_locked(&self) {
        let token = self.timer_token.swap(TIMER_TOKEN_NONE, Ordering::AcqRel);
        if token != TIMER_TOKEN_NONE {
            host::cancel_timer(token);
        }
    }

    #[cfg(not(target_arch = "aarch64"))]
    fn cancel_pending_timer_locked(&self) {
        self.timer_token.store(TIMER_TOKEN_NONE, Ordering::Release);
    }

    fn reset(&self) {
        let _guard = lock_timer_bank(&self.lifecycle);
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.ctl.store(0, Ordering::Release);
        self.cval.store(0, Ordering::Release);
        self.suspended_remaining_ticks
            .store(TIMER_REMAINING_NONE, Ordering::Release);
        self.cancel_pending_timer_locked();
    }

    fn suspend(&self) {
        let _guard = lock_timer_bank(&self.lifecycle);
        self.generation.fetch_add(1, Ordering::AcqRel);
        let ctl = self.ctl.load(Ordering::Acquire);
        let remaining_ticks = if ctl & CNTP_CTL_ENABLE != 0 {
            self.cval
                .load(Ordering::Acquire)
                .saturating_sub(self.counter_ticks())
        } else {
            TIMER_REMAINING_NONE
        };
        self.suspended_remaining_ticks
            .store(remaining_ticks, Ordering::Release);
        self.cancel_pending_timer_locked();
    }

    fn resume(self: &Arc<Self>) {
        let _guard = lock_timer_bank(&self.lifecycle);
        let remaining_ticks = self
            .suspended_remaining_ticks
            .swap(TIMER_REMAINING_NONE, Ordering::AcqRel);
        if remaining_ticks != TIMER_REMAINING_NONE {
            self.cval.store(
                self.counter_ticks().saturating_add(remaining_ticks),
                Ordering::Release,
            );
        }
        self.rearm_locked();
    }

    #[cfg(target_arch = "aarch64")]
    fn fire(self: &Arc<Self>, expected_generation: u64) {
        let _guard = lock_timer_bank(&self.lifecycle);
        if !self.is_armed_for(expected_generation) {
            return;
        }

        if self.counter_ticks() < self.cval.load(Ordering::Acquire) {
            if self.is_armed_for(expected_generation) {
                self.rearm_locked();
            }
            return;
        }

        if !self.is_armed_for(expected_generation) {
            return;
        }
        host::queue_virtual_interrupt(self.vm_id, self.vcpu_id, CNTP_PPI);
    }

    #[cfg(test)]
    fn fire_target(&self, expected_generation: u64) -> Option<(usize, usize, u8)> {
        let _guard = lock_timer_bank(&self.lifecycle);
        if !self.is_armed_for(expected_generation)
            || self.counter_ticks() < self.cval.load(Ordering::Acquire)
        {
            return None;
        }
        Some((self.vm_id, self.vcpu_id, CNTP_PPI))
    }

    #[cfg(any(test, target_arch = "aarch64"))]
    fn is_armed_for(&self, expected_generation: u64) -> bool {
        if self.generation.load(Ordering::Acquire) != expected_generation {
            return false;
        }

        let ctl = self.ctl.load(Ordering::Acquire);
        ctl & CNTP_CTL_ENABLE != 0 && ctl & CNTP_CTL_IMASK == 0
    }

    fn counter_ticks(&self) -> u64 {
        #[cfg(test)]
        {
            self.test_counter_ticks.load(Ordering::Acquire)
        }
        #[cfg(all(not(test), target_arch = "aarch64"))]
        {
            counter_ticks()
        }
        #[cfg(all(not(test), not(target_arch = "aarch64")))]
        {
            0
        }
    }

    #[cfg(test)]
    fn set_test_counter_ticks(&self, ticks: u64) {
        self.test_counter_ticks.store(ticks, Ordering::Release);
    }
}

#[cfg(all(not(test), target_arch = "aarch64"))]
fn current_timer_identity() -> (usize, usize) {
    (host::current_vm_id(), host::current_vcpu_id())
}

#[cfg(all(not(test), not(target_arch = "aarch64")))]
fn current_timer_identity() -> (usize, usize) {
    (0, 0)
}

#[cfg(target_arch = "aarch64")]
fn counter_ticks() -> u64 {
    let value: u64;

    unsafe {
        asm!("mrs {value}, CNTPCT_EL0", value = out(reg) value);
    }
    value
}

#[cfg(target_arch = "aarch64")]
fn counter_frequency_hz() -> u64 {
    let value: u64;

    unsafe {
        asm!("mrs {value}, CNTFRQ_EL0", value = out(reg) value);
    }
    value
}

#[cfg(any(test, target_arch = "aarch64"))]
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

    #[test]
    fn banks_timer_state_per_vcpu_identity() {
        let state = CntpTimerState::new();

        state.set_test_current_identity(11, 0);
        state.write_cval(0x1111);
        state.write_ctl(CNTP_CTL_ENABLE | CNTP_CTL_IMASK);

        state.set_test_current_identity(11, 1);
        state.write_cval(0x2222);
        state.write_ctl(CNTP_CTL_ENABLE);

        state.set_test_current_identity(11, 0);
        assert_eq!(state.read_cval(), 0x1111);
        assert_eq!(state.read_ctl() & (CNTP_CTL_ENABLE | CNTP_CTL_IMASK), 0x3);

        state.set_test_current_identity(11, 1);
        assert_eq!(state.read_cval(), 0x2222);
        assert_eq!(state.read_ctl() & (CNTP_CTL_ENABLE | CNTP_CTL_IMASK), 0x1);
    }

    #[test]
    fn resolves_ppi30_delivery_target_from_each_vcpu_bank() {
        let state = CntpTimerState::new();

        state.set_test_current_identity(11, 0);
        state.write_cval(0);
        state.write_ctl(CNTP_CTL_ENABLE);
        let vcpu0_target = state.current_fire_target_for_test();

        state.set_test_current_identity(11, 1);
        state.write_cval(0);
        state.write_ctl(CNTP_CTL_ENABLE);
        let vcpu1_target = state.current_fire_target_for_test();

        assert_eq!(vcpu0_target, Some((11, 0, CNTP_PPI)));
        assert_eq!(vcpu1_target, Some((11, 1, CNTP_PPI)));

        state.set_test_current_identity(11, 0);
        state.write_ctl(CNTP_CTL_ENABLE | CNTP_CTL_IMASK);
        assert_eq!(state.current_fire_target_for_test(), None);

        state.set_test_current_identity(11, 1);
        assert_eq!(
            state.current_fire_target_for_test(),
            Some((11, 1, CNTP_PPI))
        );
    }

    #[test]
    fn reset_invalidates_stale_callbacks_for_all_banks() {
        let state = CntpTimerState::new();

        state.set_test_current_identity(11, 0);
        state.write_cval(0);
        state.write_ctl(CNTP_CTL_ENABLE);
        let bank0 = state.current_bank();
        let generation0 = bank0.generation.load(Ordering::Acquire);

        state.set_test_current_identity(11, 1);
        state.write_cval(0);
        state.write_ctl(CNTP_CTL_ENABLE);
        let bank1 = state.current_bank();
        let generation1 = bank1.generation.load(Ordering::Acquire);

        state.reset();

        assert_eq!(bank0.fire_target(generation0), None);
        assert_eq!(bank1.fire_target(generation1), None);

        state.set_test_current_identity(11, 0);
        assert_eq!(state.read_cval(), 0);
        assert_eq!(state.read_ctl() & (CNTP_CTL_ENABLE | CNTP_CTL_IMASK), 0);
    }

    #[test]
    fn suspend_resume_preserves_remaining_ticks_for_enabled_bank() {
        let state = CntpTimerState::new();

        state.set_test_current_identity(11, 0);
        state.set_test_counter_ticks(100);
        state.write_cval(1_100);
        state.write_ctl(CNTP_CTL_ENABLE);
        assert_eq!(state.read_tval(), 1_000);
        assert_eq!(state.current_fire_target_for_test(), None);

        let bank = state.current_bank();
        let generation = bank.generation.load(Ordering::Acquire);
        state.set_test_counter_ticks(700);
        state.suspend();

        assert_eq!(bank.fire_target(generation), None);
        assert_eq!(state.read_tval(), 400);

        state.set_test_counter_ticks(5_000);
        state.resume();

        assert_eq!(state.read_cval(), 5_400);
        assert_eq!(state.read_tval(), 400);
        assert_eq!(state.current_fire_target_for_test(), None);

        state.set_test_counter_ticks(5_400);
        assert_eq!(
            state.current_fire_target_for_test(),
            Some((11, 0, CNTP_PPI))
        );
    }

    #[test]
    fn suspend_invalidates_stale_callbacks_and_resume_rearms_all_banks() {
        let state = CntpTimerState::new();

        state.set_test_current_identity(11, 0);
        state.write_cval(0);
        state.write_ctl(CNTP_CTL_ENABLE);
        let bank0 = state.current_bank();
        let generation0 = bank0.generation.load(Ordering::Acquire);

        state.set_test_current_identity(11, 1);
        state.write_cval(0x2222);
        state.write_ctl(CNTP_CTL_ENABLE | CNTP_CTL_IMASK);
        let bank1 = state.current_bank();
        let generation1 = bank1.generation.load(Ordering::Acquire);

        state.suspend();

        assert_eq!(bank0.fire_target(generation0), None);
        assert_eq!(bank1.fire_target(generation1), None);

        state.set_test_current_identity(11, 1);
        assert_eq!(state.read_cval(), 0x2222);
        assert_eq!(state.read_ctl() & (CNTP_CTL_ENABLE | CNTP_CTL_IMASK), 0x3);

        state.resume();

        state.set_test_current_identity(11, 0);
        assert_eq!(
            state.current_fire_target_for_test(),
            Some((11, 0, CNTP_PPI))
        );
        state.set_test_current_identity(11, 1);
        assert_eq!(state.current_fire_target_for_test(), None);
    }
}
