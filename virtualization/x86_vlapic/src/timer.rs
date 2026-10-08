// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::{
    X86TimerAction, X86VcpuId, X86VlapicError, X86VlapicResult, X86VmId,
    consts::RESET_LVT_REG,
    host::{self, X86VlapicHostOps, X86VlapicRuntimeOps},
    regs::lvt::{
        LVT_TIMER::{self, TimerMode::Value as TimerMode},
        LvtTimerRegisterLocal,
    },
    timer_registration::{
        TimerRegistration, limit_periodic_timer_period_ns, restart_periodic_deadline_ns,
    },
};

const APIC_TIMER_TICKS_PER_NANO: u64 = 1;

/// A virtual local APIC timer. (SDM Vol. 3C, Section 11.5.4)
///
/// This struct virtualizes the access to 4 registers in the Local APIC:
///
/// - LVT Timer Register. (SDM Vol. 3A, Section 11.5.1, Figure 11-8, offset 0x320, MSR 0x832, Read/Write)
/// - Divide Configuration Register. (SDM Vol. 3A, Section 11.5.4, Figure 11-10, offset 0x3E0, MSR 0x83E, Read/Write)
/// - Initial Count Register. (SDM Vol. 3A, Section 11.5.4, Figure 11-11, offset 0x380, MSR 0x838, Read/Write)
/// - Current Count Register. (SDM Vol. 3A, Section 11.5.4, Figure 11-11, offset 0x390, MSR 0x839, Read Only)
///
/// The timer works in the following way:
///
/// - Timer is started by and only by writing to the Initial Count Register.
/// - The deadline is determined by the Initial Count Register and the Divide Configuration Register, at the time of the start.
/// - Any modification to the Divide Configuration Register or the LVT Timer Register will not affect the current timer.
/// - Any write to the Initial Count Register will restart the timer.
/// - Deadline expiry atomically publishes a pending edge and wakes the owning
///   vCPU. The vCPU reads the LVT Timer before its next guest entry to determine
///   whether the edge is unmasked and which vector to inject.
/// - The value of the LVT Timer is read at expiry to determine whether the
///   timer should be restarted in periodic mode.
/// - The delivery status field in the LVT Timer Register is not supported and always returns 0.
/// - The timer stops when:
///   - the deadline is reached, and the timer is in one-shot mode, or
///   - a 0 is written to the Initial Count Register.
pub struct ApicTimer<H: X86VlapicHostOps> {
    // the raw value of writable registers
    /// Local Vector Table Timer Register. These's another copy in [`VirtualApicRegs`](crate::VirtualApicRegs), but we
    /// keep a separate copy here for easier access.
    lvt_timer_register: LvtTimerRegisterLocal,
    /// Initial Count Register. This is the value that determines when the timer will fire.
    initial_count_register: u32,
    /// Divide Configuration Register. This determines the frequency of the timer.
    divide_configuration_register: u32,

    // internal states
    divide_shift: u8,

    /// Set while a VM suspend quiesced a live host arm that still owes a
    /// resume. Task-side only; the guest registers and the canonical
    /// deadline/interval/pending live in `shared`.
    suspended: bool,

    runtime: H::Runtime,
    shared: Arc<ApicTimerShared<H::Runtime>>,
}

struct ApicTimerShared<R: X86VlapicRuntimeOps> {
    registration: Arc<TimerRegistration<R>>,
    lvt_timer_register: AtomicU32,
    interval_ns: AtomicU64,
    deadline_ns: AtomicU64,
    pending: AtomicU32,
}

impl<H: X86VlapicHostOps> ApicTimer<H> {
    pub(crate) fn new(runtime: H::Runtime, _vm_id: X86VmId, _vcpu_id: X86VcpuId) -> Self {
        Self {
            lvt_timer_register: LvtTimerRegisterLocal::new(RESET_LVT_REG), /* masked, one-shot, vector 0 */
            initial_count_register: 0,                                     // 0 (stopped)
            divide_configuration_register: 0,                              // divide by 2

            divide_shift: 1, /* as `divide_configuration_register` is 0, the shift is 1 (divide by 2) */
            suspended: false,
            shared: Arc::new(ApicTimerShared {
                registration: Arc::new(TimerRegistration::new()),
                lvt_timer_register: AtomicU32::new(RESET_LVT_REG),
                interval_ns: AtomicU64::new(0),
                deadline_ns: AtomicU64::new(0),
                pending: AtomicU32::new(0),
            }),
            runtime,
        }
    }

    #[allow(dead_code)]
    pub fn read_lvt(&self) -> u32 {
        self.lvt_timer_register.get()
    }

    pub fn write_lvt(&mut self, mut value: u32) -> X86VlapicResult {
        // valid bits: 0-7, 12, 16-18
        const LVT_MASK: u32 = 0x0007_10FF;

        value &= LVT_MASK;
        self.lvt_timer_register.set(value);
        self.shared
            .lvt_timer_register
            .store(value, Ordering::Release);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn read_icr(&self) -> u32 {
        self.initial_count_register
    }

    pub fn write_icr(&mut self, value: u32) -> X86VlapicResult {
        // stop the timer no matter whether it is started, and no matter the value
        self.stop_timer()?;
        self.initial_count_register = value;

        if value > 0 {
            self.start_timer()
        } else {
            Ok(())
        }
    }

    /// Read from the Divide Configuration Register.
    #[allow(dead_code)]
    pub fn read_dcr(&self) -> u32 {
        self.divide_configuration_register
    }

    /// Write to the Divide Configuration Register.
    pub fn write_dcr(&mut self, mut value: u32) {
        const DCR_MASK: u32 = 0b1011;

        value &= DCR_MASK;
        let shift = match value {
            0b0000 => 1, // divide by 2
            0b0001 => 2, // divide by 4
            0b0010 => 3, // divide by 8
            0b0011 => 4, // divide by 16
            0b1000 => 5, // divide by 32
            0b1001 => 6, // divide by 64
            0b1010 => 7, // divide by 128
            0b1011 => 0, // divide by 1
            _ => unreachable!(
                "internal error: invalid divide configuration register value after mask"
            ),
        };

        self.divide_configuration_register = value;
        self.divide_shift = shift as u8;
    }

    /// Current Count Register.
    pub fn read_ccr(&self) -> u32 {
        if !self.is_started() {
            return 0;
        }
        let mut deadline_ns = self.shared.deadline_ns.load(Ordering::Acquire);
        let now_ns = host::current_time_nanos::<H>();
        if now_ns >= deadline_ns {
            if !self.is_periodic() {
                return 0;
            }

            let interval_ns = self.shared.interval_ns.load(Ordering::Acquire);
            if interval_ns == 0 {
                return 0;
            }

            deadline_ns = next_periodic_deadline_ns(deadline_ns, interval_ns, now_ns);
            self.shared
                .deadline_ns
                .store(deadline_ns, Ordering::Release);
        }
        let remaining_ns = deadline_ns - now_ns;
        let remaining_ticks = remaining_ns * APIC_TIMER_TICKS_PER_NANO;
        (remaining_ticks >> self.divide_shift) as _
    }

    /// Get the timer mode.
    pub fn timer_mode(&self) -> TimerMode {
        self.lvt_timer_register
            .read_as_enum(LVT_TIMER::TimerMode)
            .unwrap() // just panic if the value is invalid
    }

    /// Check whether the timer interrupt is masked.
    #[allow(dead_code)]
    pub fn is_masked(&self) -> bool {
        self.lvt_timer_register.is_set(LVT_TIMER::Mask)
    }

    /// Check whether the timer is started.
    pub fn is_started(&self) -> bool {
        // A completed one-shot callback leaves a retained host handle but no
        // live arm, so the guest-visible running state keys on the arm phase.
        self.initial_count_register > 0 && self.shared.registration.is_active()
    }

    /// Returns whether an unmasked timer edge is waiting for vCPU entry.
    pub(crate) fn has_pending_interrupt(&self) -> bool {
        self.shared.pending.load(Ordering::Acquire) != 0
            && self.pending_interrupt_vector().is_some()
    }

    /// Coalesces accumulated timer expirations into one local-APIC edge.
    pub(crate) fn take_pending_interrupt(&self) -> Option<u8> {
        let vector = self.pending_interrupt_vector()?;
        (self.shared.pending.swap(0, Ordering::AcqRel) != 0).then_some(vector)
    }

    /// Restart the timer. Will not start the timer if it is not started.
    pub fn restart_timer(&mut self) -> X86VlapicResult {
        if !self.is_started() {
            Ok(())
        } else {
            self.stop_timer()?;
            self.start_timer()
        }
    }

    /// Start the timer.
    pub fn start_timer(&mut self) -> X86VlapicResult {
        if self.is_started() {
            return Err(X86VlapicError::BadState);
        }

        let current_ns = host::current_time_nanos::<H>();
        let interval_ticks = (self.initial_count_register as u64) << self.divide_shift;
        let interval_ns = interval_ticks / APIC_TIMER_TICKS_PER_NANO;
        let interval_ns = if self.is_periodic() {
            limit_periodic_timer_period_ns(interval_ns)
        } else {
            interval_ns
        };
        let deadline_ns = current_ns.saturating_add(interval_ns);
        self.shared
            .interval_ns
            .store(interval_ns, Ordering::Release);
        self.shared
            .deadline_ns
            .store(deadline_ns, Ordering::Release);
        self.shared.pending.store(0, Ordering::Release);

        schedule_apic_timer::<H>(deadline_ns, self.runtime.clone(), Arc::clone(&self.shared))
    }

    pub fn stop_timer(&mut self) -> X86VlapicResult {
        // Cancel before retiring the guest-visible state: a host cancellation
        // failure must leave both the retained handle and the deadline/interval
        // intact so the caller can retry instead of dropping a live timer. The
        // cancel barrier also covers a callback that already completed but whose
        // host handle has not been reclaimed yet, so "stopped" really means the
        // producer is quiet.
        self.shared
            .registration
            .invalidate_and_cancel(&self.runtime)?;
        self.shared.interval_ns.store(0, Ordering::Release);
        self.shared.deadline_ns.store(0, Ordering::Release);
        self.shared.pending.store(0, Ordering::Release);
        self.suspended = false;
        Ok(())
    }

    /// Quiesces the host timer for a task-side VM suspend.
    ///
    /// The guest LVT/ICR/DCR registers, the canonical deadline and interval,
    /// and any already published pending edge are all retained, so
    /// [`Self::resume_timer`] reinstalls the same timer instead of restarting
    /// the countdown. A host cancellation failure keeps the arm and returns the
    /// error so the caller retries rather than silently dropping the handle.
    ///
    /// The full cancel barrier runs even when the callback already completed,
    /// so the pause ACK only happens once no callback or payload reclamation is
    /// still in flight. A completed one-shot owes no resume.
    pub fn suspend_timer(&mut self) -> X86VlapicResult {
        if self.suspended {
            return Ok(());
        }
        let resume_owed = self
            .shared
            .registration
            .invalidate_and_cancel(&self.runtime)?;
        self.suspended = resume_owed;
        Ok(())
    }

    /// Reinstalls the host timer quiesced by [`Self::suspend_timer`].
    ///
    /// The retained absolute deadline is reused, so a deadline that expired
    /// while the VM was suspended fires one edge promptly instead of being
    /// pushed into the future. Reinstalling happens at most once per suspend;
    /// a second resume is a no-op, and a registration failure keeps the
    /// suspend outstanding for retry.
    pub fn resume_timer(&mut self) -> X86VlapicResult {
        if !self.suspended {
            return Ok(());
        }
        if self.shared.registration.has_registration() {
            self.suspended = false;
            return Ok(());
        }
        let deadline_ns = self.shared.deadline_ns.load(Ordering::Acquire);
        if deadline_ns != 0 {
            schedule_apic_timer::<H>(deadline_ns, self.runtime.clone(), Arc::clone(&self.shared))?;
        }
        self.suspended = false;
        Ok(())
    }

    /// Whether the timer mode is periodic.
    pub fn is_periodic(&self) -> bool {
        self.timer_mode() == TimerMode::Periodic
    }

    fn pending_interrupt_vector(&self) -> Option<u8> {
        let lvt = self.shared.lvt_timer_register.load(Ordering::Acquire);
        ((lvt & LVT_TIMER::Mask::SET.mask()) == 0).then_some((lvt & 0xff) as u8)
    }
}

impl<H: X86VlapicHostOps> Drop for ApicTimer<H> {
    fn drop(&mut self) {
        // The task-side lifecycle must quiesce this producer before the owning
        // backend is retired: `stop_timer`/`suspend_timer` return cancellation
        // failures so the caller retries. Reaching drop with a live arm is a
        // contract violation that must not be hidden behind a warn-and-drop.
        // A completed one-shot may still carry a retained (retired) handle; that
        // is not a live producer and needs no cancellation.
        assert!(
            !self.shared.registration.is_active(),
            "x86 APIC timer dropped while its host registration was live",
        );
    }
}

fn schedule_apic_timer<H>(
    deadline_nanos: u64,
    runtime: H::Runtime,
    shared: Arc<ApicTimerShared<H::Runtime>>,
) -> X86VlapicResult
where
    H: X86VlapicHostOps,
{
    let callback_shared = Arc::clone(&shared);
    unsafe {
        // SAFETY: the hard callback touches only atomics and the timer
        // registration's IRQ-safe spin state. The host wrapper owns the
        // pre-bound vCPU wake capability; no registry lookup, allocation,
        // destruction, logging, or sleepable lock is used here.
        shared.registration.register_hard(
            &runtime,
            deadline_nanos,
            Box::new(move |_| {
                let lvt = callback_shared.lvt_timer_register.load(Ordering::Acquire);
                let mode = (lvt & LVT_TIMER::TimerMode::SET.mask()) >> 17;
                callback_shared.pending.fetch_add(1, Ordering::Release);

                if mode == TimerMode::Periodic as u32 {
                    let interval_ns = callback_shared.interval_ns.load(Ordering::Acquire);
                    if interval_ns != 0 {
                        let old_deadline = callback_shared.deadline_ns.load(Ordering::Acquire);
                        let next_deadline_ns = restart_periodic_deadline_ns(
                            old_deadline,
                            interval_ns,
                            host::current_time_nanos::<H>(),
                        );
                        callback_shared
                            .deadline_ns
                            .store(next_deadline_ns, Ordering::Release);
                        return X86TimerAction::Rearm(next_deadline_ns);
                    }
                }
                X86TimerAction::Complete
            }),
        )
    }
}

fn next_periodic_deadline_ns(deadline_ns: u64, interval_ns: u64, now_ns: u64) -> u64 {
    if deadline_ns > now_ns {
        return deadline_ns;
    }

    let missed_intervals = (now_ns - deadline_ns) / interval_ns + 1;
    deadline_ns.saturating_add(interval_ns.saturating_mul(missed_intervals))
}

// Component-protocol tests for the vLAPIC timer state machine.
//
// The host timer below is a deterministic registration/cancel/deadline ledger
// substitute, not a hardware timer: it proves the component protocol (retained
// pending, cancel-failure retry, resume-once) and deliberately makes no claim
// about native IRQ delivery, which the owner verifies on QEMU.
#[cfg(test)]
mod tests {
    extern crate std;

    use self::std::{
        sync::{Arc, Condvar, Mutex, mpsc},
        thread,
        time::Duration,
        vec::Vec,
    };
    use crate::{
        EmulatedPit, X86AccessWidth, X86HostPhysAddr, X86HostVirtAddr, X86InterruptVector, X86Port,
        X86TimerAction, X86TimerCallback, X86VcpuId, X86VlapicHostOps, X86VlapicResult,
        X86VlapicRuntimeOps, X86VmId, regs::lvt::LVT_TIMER::TimerMode::Value as TimerMode,
        timer::ApicTimer,
    };

    struct DummyHost;

    #[derive(Clone)]
    struct DummyRuntime {
        vm_id: X86VmId,
        vcpu_id: X86VcpuId,
    }

    struct TestTimerState {
        callbacks: Vec<Option<X86TimerCallback>>,
        cancelled: Vec<usize>,
        deadlines: Vec<u64>,
        fail_cancels: usize,
        block_time_read: bool,
        time_read_started: bool,
        allow_time_read: bool,
    }

    static TEST_TIMER_STATE: Mutex<TestTimerState> = Mutex::new(TestTimerState {
        callbacks: Vec::new(),
        cancelled: Vec::new(),
        deadlines: Vec::new(),
        fail_cancels: 0,
        block_time_read: false,
        time_read_started: false,
        allow_time_read: false,
    });
    static TEST_TIMER_EVENT: Condvar = Condvar::new();
    static TEST_TIMER_SERIAL: Mutex<()> = Mutex::new(());

    struct TimerHost;

    #[derive(Clone)]
    struct TimerRuntime {
        vm_id: X86VmId,
        vcpu_id: X86VcpuId,
    }

    impl TimerHost {
        fn reset() {
            let mut state = TEST_TIMER_STATE.lock().unwrap();
            state.callbacks.clear();
            state.cancelled.clear();
            state.deadlines.clear();
            state.fail_cancels = 0;
            state.block_time_read = false;
            state.time_read_started = false;
            state.allow_time_read = false;
        }

        fn fire(token: usize, now_nanos: u64) {
            let mut callback = {
                TEST_TIMER_STATE.lock().unwrap().callbacks[token - 1]
                    .take()
                    .expect("test timer callback must remain registered")
            };
            if matches!(callback(now_nanos), X86TimerAction::Rearm(_)) {
                TEST_TIMER_STATE.lock().unwrap().callbacks[token - 1] = Some(callback);
            }
        }

        fn cancelled() -> Vec<usize> {
            TEST_TIMER_STATE.lock().unwrap().cancelled.clone()
        }

        fn registration_count() -> usize {
            TEST_TIMER_STATE.lock().unwrap().callbacks.len()
        }

        fn deadlines() -> Vec<u64> {
            TEST_TIMER_STATE.lock().unwrap().deadlines.clone()
        }

        fn fail_next_cancel() {
            TEST_TIMER_STATE.lock().unwrap().fail_cancels += 1;
        }

        fn block_time_read() {
            let mut state = TEST_TIMER_STATE.lock().unwrap();
            state.block_time_read = true;
            state.time_read_started = false;
            state.allow_time_read = false;
        }

        fn wait_for_time_read() {
            let mut state = TEST_TIMER_STATE.lock().unwrap();
            while !state.time_read_started {
                state = TEST_TIMER_EVENT.wait(state).unwrap();
            }
        }

        fn release_time_read() {
            let mut state = TEST_TIMER_STATE.lock().unwrap();
            state.allow_time_read = true;
            TEST_TIMER_EVENT.notify_all();
        }
    }

    impl X86VlapicHostOps for DummyHost {
        type TimerHandle = usize;
        type Runtime = DummyRuntime;

        fn alloc_frame() -> Option<X86HostPhysAddr> {
            None
        }

        fn dealloc_frame(_paddr: X86HostPhysAddr) {}

        fn phys_to_virt(paddr: X86HostPhysAddr) -> X86HostVirtAddr {
            X86HostVirtAddr::from_usize(paddr.as_usize())
        }

        fn virt_to_phys(vaddr: X86HostVirtAddr) -> X86HostPhysAddr {
            X86HostPhysAddr::from_usize(vaddr.as_usize())
        }

        fn current_time_nanos() -> u64 {
            0
        }

        fn unbound_runtime(vm_id: X86VmId, vcpu_id: X86VcpuId) -> Self::Runtime {
            Self::Runtime { vm_id, vcpu_id }
        }
    }

    impl X86VlapicRuntimeOps for DummyRuntime {
        type TimerHandle = usize;

        fn vm_id(&self) -> X86VmId {
            self.vm_id
        }

        fn vcpu_id(&self) -> X86VcpuId {
            self.vcpu_id
        }

        fn vcpu_count(&self) -> usize {
            1
        }

        fn active_vcpu_mask(&self) -> usize {
            1
        }

        fn inject_interrupt(
            &self,
            _target_vcpu_id: X86VcpuId,
            _vector: X86InterruptVector,
        ) -> X86VlapicResult {
            Ok(())
        }

        fn inject_pit_irq(&self) -> X86VlapicResult {
            Ok(())
        }

        fn register_timer(
            &self,
            _deadline_nanos: u64,
            _callback: X86TimerCallback,
        ) -> X86VlapicResult<Self::TimerHandle> {
            Err(crate::X86VlapicError::TimerUnavailable)
        }

        unsafe fn register_hard_timer(
            &self,
            deadline_nanos: u64,
            callback: X86TimerCallback,
        ) -> X86VlapicResult<Self::TimerHandle> {
            self.register_timer(deadline_nanos, callback)
        }

        fn wait_timer_progress(&self) {
            // This fixture never keeps an arm registered, so cancellation never
            // needs to wait for a callback.
        }

        fn cancel_timer(&self, _handle: Self::TimerHandle) -> X86VlapicResult {
            Ok(())
        }
    }

    impl X86VlapicHostOps for TimerHost {
        type TimerHandle = usize;
        type Runtime = TimerRuntime;

        fn alloc_frame() -> Option<X86HostPhysAddr> {
            None
        }

        fn dealloc_frame(_paddr: X86HostPhysAddr) {}

        fn phys_to_virt(paddr: X86HostPhysAddr) -> X86HostVirtAddr {
            X86HostVirtAddr::from_usize(paddr.as_usize())
        }

        fn virt_to_phys(vaddr: X86HostVirtAddr) -> X86HostPhysAddr {
            X86HostPhysAddr::from_usize(vaddr.as_usize())
        }

        fn current_time_nanos() -> u64 {
            let mut state = TEST_TIMER_STATE.lock().unwrap();
            if state.block_time_read {
                state.time_read_started = true;
                TEST_TIMER_EVENT.notify_all();
                while !state.allow_time_read {
                    state = TEST_TIMER_EVENT.wait(state).unwrap();
                }
            }
            0
        }

        fn unbound_runtime(vm_id: X86VmId, vcpu_id: X86VcpuId) -> Self::Runtime {
            Self::Runtime { vm_id, vcpu_id }
        }
    }

    impl X86VlapicRuntimeOps for TimerRuntime {
        type TimerHandle = usize;

        fn vm_id(&self) -> X86VmId {
            self.vm_id
        }

        fn vcpu_id(&self) -> X86VcpuId {
            self.vcpu_id
        }

        fn vcpu_count(&self) -> usize {
            1
        }

        fn active_vcpu_mask(&self) -> usize {
            1
        }

        fn inject_interrupt(
            &self,
            _target_vcpu_id: X86VcpuId,
            _vector: X86InterruptVector,
        ) -> X86VlapicResult {
            Ok(())
        }

        fn inject_pit_irq(&self) -> X86VlapicResult {
            Ok(())
        }

        fn register_timer(
            &self,
            deadline_nanos: u64,
            callback: X86TimerCallback,
        ) -> X86VlapicResult<Self::TimerHandle> {
            let mut state = TEST_TIMER_STATE.lock().unwrap();
            state.callbacks.push(Some(callback));
            state.deadlines.push(deadline_nanos);
            Ok(state.callbacks.len())
        }

        unsafe fn register_hard_timer(
            &self,
            deadline_nanos: u64,
            callback: X86TimerCallback,
        ) -> X86VlapicResult<Self::TimerHandle> {
            self.register_timer(deadline_nanos, callback)
        }

        fn wait_timer_progress(&self) {
            // Real task-context yield, never a spin: a cancel barrier may be
            // waiting for this test's in-flight callback thread.
            thread::yield_now();
        }

        fn cancel_timer(&self, token: Self::TimerHandle) -> X86VlapicResult {
            let mut state = TEST_TIMER_STATE.lock().unwrap();
            if state.fail_cancels > 0 {
                state.fail_cancels -= 1;
                return Err(crate::X86VlapicError::TimerUnavailable);
            }
            state.cancelled.push(token);
            state.callbacks[token - 1].take();
            Ok(())
        }
    }

    #[test]
    fn test_lvt_register_operations() {
        let vm_id = 1;
        let vcpu_id = 0;
        let mut timer =
            ApicTimer::<DummyHost>::new(DummyRuntime { vm_id, vcpu_id }, vm_id, vcpu_id);

        // Test LVT write with valid bits
        assert!(timer.write_lvt(0x000710FF).is_ok());
        assert_eq!(timer.read_lvt() & 0x000710FF, 0x000710FF);

        // Test LVT write with invalid bits (should be masked)
        assert!(timer.write_lvt(0xFFFFFFFF).is_ok());
        assert_eq!(timer.read_lvt() & !0x000710FF, 0);

        // Test vector number
        assert!(timer.write_lvt(0x50).is_ok()); // vector 0x50
        assert_eq!(timer.read_lvt() & 0xff, 0x50);
    }

    #[test]
    fn test_divide_configuration_register() {
        let vm_id = 1;
        let vcpu_id = 0;
        let mut timer =
            ApicTimer::<DummyHost>::new(DummyRuntime { vm_id, vcpu_id }, vm_id, vcpu_id);

        // Test different divide values
        timer.write_dcr(0b0000); // divide by 2
        assert_eq!(timer.read_dcr(), 0b0000);

        timer.write_dcr(0b0001); // divide by 4
        assert_eq!(timer.read_dcr(), 0b0001);

        timer.write_dcr(0b1011); // divide by 1
        assert_eq!(timer.read_dcr(), 0b1011);

        // Test invalid bits are masked
        timer.write_dcr(0xFFFFFFFF);
        assert_eq!(timer.read_dcr() & !0b1011, 0);
    }

    #[test]
    fn test_timer_mode() {
        let vm_id = 1;
        let vcpu_id = 0;
        let mut timer =
            ApicTimer::<DummyHost>::new(DummyRuntime { vm_id, vcpu_id }, vm_id, vcpu_id);

        // Default should be one-shot
        assert_eq!(timer.timer_mode(), TimerMode::OneShot);
        assert!(!timer.is_periodic());

        // Set periodic mode (bit 17 = 1)
        assert!(timer.write_lvt(0x20000).is_ok());
        assert_eq!(timer.timer_mode(), TimerMode::Periodic);
        assert!(timer.is_periodic());
    }

    #[test]
    fn test_timer_mask() {
        let vm_id = 1;
        let vcpu_id = 0;
        let mut timer =
            ApicTimer::<DummyHost>::new(DummyRuntime { vm_id, vcpu_id }, vm_id, vcpu_id);

        // Default should be masked
        assert!(timer.is_masked());

        // Unmask timer (bit 16 = 0)
        assert!(timer.write_lvt(0x50).is_ok()); // vector 0x50, not masked
        assert!(!timer.is_masked());

        // Mask timer (bit 16 = 1)
        assert!(timer.write_lvt(0x10050).is_ok()); // vector 0x50, masked
        assert!(timer.is_masked());
    }

    #[test]
    fn test_multiple_timers() {
        let vm_id = 1;
        let timer1 = ApicTimer::<DummyHost>::new(DummyRuntime { vm_id, vcpu_id: 0 }, vm_id, 0);
        let timer2 = ApicTimer::<DummyHost>::new(DummyRuntime { vm_id, vcpu_id: 1 }, vm_id, 1);

        // Both timers should be independent
        assert!(!timer1.is_started());
        assert!(!timer2.is_started());
        assert_eq!(timer1.read_icr(), timer2.read_icr());
        assert_eq!(timer1.read_dcr(), timer2.read_dcr());
    }

    #[test]
    fn periodic_timer_reuses_one_host_registration_until_stopped() {
        let _serial = TEST_TIMER_SERIAL.lock().unwrap();
        TimerHost::reset();
        let mut timer = ApicTimer::<TimerHost>::new(
            TimerRuntime {
                vm_id: 1,
                vcpu_id: 0,
            },
            1,
            0,
        );
        timer.write_lvt(0x20040).unwrap();
        timer.write_icr(1).unwrap();

        TimerHost::fire(1, 2);
        assert_eq!(TimerHost::registration_count(), 1);
        timer.write_icr(0).unwrap();

        assert_eq!(TimerHost::cancelled(), self::std::vec![1]);
    }

    #[test]
    fn hard_expiry_publishes_one_edge_for_the_next_vcpu_entry() {
        let _serial = TEST_TIMER_SERIAL.lock().unwrap();
        TimerHost::reset();
        let mut timer = ApicTimer::<TimerHost>::new(
            TimerRuntime {
                vm_id: 1,
                vcpu_id: 0,
            },
            1,
            0,
        );
        timer.write_lvt(0x40).unwrap();
        timer.write_icr(1).unwrap();

        TimerHost::fire(1, 2);

        assert!(timer.has_pending_interrupt());
        assert_eq!(timer.take_pending_interrupt(), Some(0x40));
        assert!(!timer.has_pending_interrupt());
        assert_eq!(timer.take_pending_interrupt(), None);
    }

    #[test]
    fn stopping_timer_waits_for_a_claimed_callback() {
        let _serial = TEST_TIMER_SERIAL.lock().unwrap();
        TimerHost::reset();
        let timer = Arc::new(Mutex::new(ApicTimer::<TimerHost>::new(
            TimerRuntime {
                vm_id: 1,
                vcpu_id: 0,
            },
            1,
            0,
        )));
        {
            let mut timer = timer.lock().unwrap();
            timer.write_lvt(0x20040).unwrap();
            timer.write_icr(1).unwrap();
        }
        TimerHost::block_time_read();

        let firing = thread::spawn(|| TimerHost::fire(1, 2));
        TimerHost::wait_for_time_read();

        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let cancelling_timer = Arc::clone(&timer);
        let cancelling = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = cancelling_timer.lock().unwrap().write_icr(0);
            done_tx.send(result).unwrap();
        });
        started_rx.recv().unwrap();
        let returned_while_callback_was_running =
            done_rx.recv_timeout(Duration::from_millis(100)).ok();

        TimerHost::release_time_read();
        firing.join().unwrap();
        let cancellation = returned_while_callback_was_running
            .unwrap_or_else(|| done_rx.recv_timeout(Duration::from_secs(1)).unwrap());
        cancelling.join().unwrap();

        assert!(
            returned_while_callback_was_running.is_none(),
            "timer stop returned before its claimed callback completed"
        );
        cancellation.unwrap();
        assert_eq!(
            TimerHost::cancelled(),
            self::std::vec![1],
            "the claimed callback's handle must be cancelled exactly once after it retires",
        );
        assert!(
            !timer.lock().unwrap().is_started(),
            "a completed stop must leave the guest timer stopped",
        );
    }

    #[test]
    fn suspend_keeps_pending_and_resume_reinstalls_at_most_once() {
        let _serial = TEST_TIMER_SERIAL.lock().unwrap();
        TimerHost::reset();
        let mut timer = ApicTimer::<TimerHost>::new(
            TimerRuntime {
                vm_id: 1,
                vcpu_id: 0,
            },
            1,
            0,
        );
        // Periodic, unmasked, vector 0x40: the arm stays live across expiry.
        timer.write_lvt(0x20040).unwrap();
        timer.write_icr(1).unwrap();

        TimerHost::fire(1, 2);
        assert!(timer.has_pending_interrupt());
        let live_deadline = TimerHost::deadlines()[0];

        timer.suspend_timer().unwrap();
        assert_eq!(TimerHost::cancelled(), self::std::vec![1]);
        assert!(
            timer.has_pending_interrupt(),
            "a quiesced timer must retain its published pending edge",
        );

        timer.resume_timer().unwrap();
        timer.resume_timer().unwrap();
        assert_eq!(
            TimerHost::registration_count(),
            2,
            "resume must reinstall exactly one host registration",
        );
        assert!(
            TimerHost::deadlines()[1] > live_deadline,
            "resume must reuse the retained advanced deadline, not restart the countdown",
        );
        assert!(
            timer.has_pending_interrupt(),
            "resume must not discard the retained pending edge",
        );

        // The reinstalled registration is the live producer again.
        TimerHost::fire(2, live_deadline);
        timer.stop_timer().unwrap();
        assert_eq!(TimerHost::cancelled(), self::std::vec![1, 2]);
    }

    #[test]
    fn stop_keeps_state_until_a_failed_cancel_is_retried() {
        let _serial = TEST_TIMER_SERIAL.lock().unwrap();
        TimerHost::reset();
        let mut timer = ApicTimer::<TimerHost>::new(
            TimerRuntime {
                vm_id: 1,
                vcpu_id: 0,
            },
            1,
            0,
        );
        timer.write_lvt(0x20040).unwrap();
        timer.write_icr(7).unwrap();
        assert!(timer.is_started());

        TimerHost::fail_next_cancel();
        assert!(
            timer.stop_timer().is_err(),
            "a failed host cancel must be reported, not dropped",
        );
        assert!(
            timer.is_started(),
            "a failed cancel must keep the live arm for retry",
        );
        assert_eq!(
            timer.read_icr(),
            7,
            "a failed cancel must keep the guest timer state",
        );
        assert!(
            TimerHost::cancelled().is_empty(),
            "a failed cancel must not retire the host registration",
        );

        timer.stop_timer().unwrap();
        assert!(!timer.is_started());
        assert_eq!(TimerHost::cancelled(), self::std::vec![1]);
    }

    /// A callback that completes before any task-side cancel must not erase the
    /// registration: the stable host handle is retained so a later stop still
    /// observes the real host retirement instead of falsely claiming quiet.
    #[test]
    fn completed_one_shot_keeps_its_handle_until_a_cancel_observes_retirement() {
        let _serial = TEST_TIMER_SERIAL.lock().unwrap();
        TimerHost::reset();
        let mut timer = ApicTimer::<TimerHost>::new(
            TimerRuntime {
                vm_id: 1,
                vcpu_id: 0,
            },
            1,
            0,
        );
        // One-shot, unmasked, vector 0x40.
        timer.write_lvt(0x40).unwrap();
        timer.write_icr(1).unwrap();
        assert!(timer.is_started());

        // The callback completes and retires the arm; the handle stays owned.
        TimerHost::fire(1, 2);
        assert!(
            !timer.is_started(),
            "a completed one-shot must leave no live arm",
        );
        assert!(timer.has_pending_interrupt());

        // Stopping still has to cancel the retained handle and reclaim it.
        timer.stop_timer().unwrap();
        assert_eq!(
            TimerHost::cancelled(),
            self::std::vec![1],
            "a completed callback must not be mistaken for host quiescence",
        );
        assert!(!timer.has_pending_interrupt());
    }

    /// New guest programming must retire a completed arm behind the full cancel
    /// barrier before installing exactly one fresh registration.
    #[test]
    fn reprogram_after_a_completed_arm_cancels_the_retained_handle_first() {
        let _serial = TEST_TIMER_SERIAL.lock().unwrap();
        TimerHost::reset();
        let mut timer = ApicTimer::<TimerHost>::new(
            TimerRuntime {
                vm_id: 1,
                vcpu_id: 0,
            },
            1,
            0,
        );
        timer.write_lvt(0x40).unwrap();
        timer.write_icr(1).unwrap();
        TimerHost::fire(1, 2);
        assert!(!timer.is_started());

        timer.write_icr(4).unwrap();
        assert!(timer.is_started());
        assert_eq!(
            TimerHost::cancelled(),
            self::std::vec![1],
            "the completed arm must be cancelled before the new arm is installed",
        );
        assert_eq!(
            TimerHost::registration_count(),
            2,
            "reprogram must install exactly one fresh host registration",
        );

        timer.stop_timer().unwrap();
        assert_eq!(TimerHost::cancelled(), self::std::vec![1, 2]);
    }

    /// The PIT core shares the same host-timer protocol and reuses this ledger
    /// harness instead of introducing a parallel constructor or substitute.
    #[test]
    fn pit_core_suspends_resumes_and_stops_through_the_same_lifecycle() {
        let _serial = TEST_TIMER_SERIAL.lock().unwrap();
        TimerHost::reset();
        let pit = EmulatedPit::<TimerHost>::new_for_vcpu_with_runtime(
            TimerRuntime {
                vm_id: 1,
                vcpu_id: 0,
            },
            1,
            0,
        );

        // Channel 0, low-then-high, rate generator: a live periodic IRQ0.
        pit.handle_write(X86Port::new(0x43), X86AccessWidth::Byte, 0x34)
            .unwrap();
        pit.handle_write(X86Port::new(0x40), X86AccessWidth::Byte, 0x00)
            .unwrap();
        pit.handle_write(X86Port::new(0x40), X86AccessWidth::Byte, 0x00)
            .unwrap();
        assert_eq!(TimerHost::registration_count(), 1);
        let live_deadline = TimerHost::deadlines()[0];

        TimerHost::fire(1, 1);
        pit.suspend().unwrap();
        assert_eq!(TimerHost::cancelled(), self::std::vec![1]);

        pit.resume().unwrap();
        pit.resume().unwrap();
        assert_eq!(
            TimerHost::registration_count(),
            2,
            "PIT resume must reinstall exactly one host registration",
        );
        assert!(
            TimerHost::deadlines()[1] > live_deadline,
            "PIT resume must reuse the callback-advanced deadline, not the program-time reload",
        );

        pit.stop().unwrap();
        assert_eq!(TimerHost::cancelled(), self::std::vec![1, 2]);
    }

    /// A completed one-shot IRQ0 registration must be retired behind the cancel
    /// barrier before the guest can program a fresh countdown, so a stale host
    /// handle is never leaked or double-owned.
    #[test]
    fn pit_reprogram_after_a_completed_arm_cancels_the_retained_handle_first() {
        let _serial = TEST_TIMER_SERIAL.lock().unwrap();
        TimerHost::reset();
        let pit = EmulatedPit::<TimerHost>::new_for_vcpu_with_runtime(
            TimerRuntime {
                vm_id: 1,
                vcpu_id: 0,
            },
            1,
            0,
        );

        // Channel 0, low-then-high, mode 0 (interrupt on terminal count): a
        // one-shot IRQ0 that completes after a single edge.
        pit.handle_write(X86Port::new(0x43), X86AccessWidth::Byte, 0x30)
            .unwrap();
        pit.handle_write(X86Port::new(0x40), X86AccessWidth::Byte, 0x00)
            .unwrap();
        pit.handle_write(X86Port::new(0x40), X86AccessWidth::Byte, 0x00)
            .unwrap();
        assert_eq!(TimerHost::registration_count(), 1);

        TimerHost::fire(1, 1);

        // Reprogramming must cancel the completed arm first and then install a
        // single fresh registration.
        pit.handle_write(X86Port::new(0x40), X86AccessWidth::Byte, 0x00)
            .unwrap();
        pit.handle_write(X86Port::new(0x40), X86AccessWidth::Byte, 0x00)
            .unwrap();
        assert_eq!(
            TimerHost::cancelled(),
            self::std::vec![1],
            "a completed IRQ0 arm must be cancelled before the new arm",
        );
        assert_eq!(TimerHost::registration_count(), 2);

        pit.stop().unwrap();
        assert_eq!(TimerHost::cancelled(), self::std::vec![1, 2]);
    }
}
