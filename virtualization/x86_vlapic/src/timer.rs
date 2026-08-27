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
use core::{
    marker::PhantomData,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use crate::{
    X86TimerAction, X86VcpuId, X86VlapicError, X86VlapicResult, X86VmId,
    consts::RESET_LVT_REG,
    host::{self, X86VlapicHostOps},
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
/// - The value of the LVT Timer is read, at the time the deadline is reached, to determine
///   - if an interrupt should be generated (not masked),
///   - if the timer should be restarted (periodic mode), and
///   - the interrupt vector number to be used.
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

    where_am_i: (X86VmId, X86VcpuId), // (vm_id, vcpu_id)
    shared: Arc<ApicTimerShared<H>>,
    _host: PhantomData<fn() -> H>,
}

struct ApicTimerShared<H: X86VlapicHostOps> {
    registration: Arc<TimerRegistration<H>>,
    lvt_timer_register: AtomicU32,
    interval_ns: AtomicU64,
    deadline_ns: AtomicU64,
}

impl<H: X86VlapicHostOps> ApicTimer<H> {
    pub(crate) fn new(vm_id: X86VmId, vcpu_id: X86VcpuId) -> Self {
        Self {
            lvt_timer_register: LvtTimerRegisterLocal::new(RESET_LVT_REG), /* masked, one-shot, vector 0 */
            initial_count_register: 0,                                     // 0 (stopped)
            divide_configuration_register: 0,                              // divide by 2

            divide_shift: 1, /* as `divide_configuration_register` is 0, the shift is 1 (divide by 2) */
            where_am_i: (vm_id, vcpu_id),
            shared: Arc::new(ApicTimerShared {
                registration: Arc::new(TimerRegistration::new()),
                lvt_timer_register: AtomicU32::new(RESET_LVT_REG),
                interval_ns: AtomicU64::new(0),
                deadline_ns: AtomicU64::new(0),
            }),
            _host: PhantomData,
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
        // these two conditions are equivalent actually, we check both for clarity and robustness
        self.initial_count_register > 0 && self.shared.registration.is_armed()
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
        let (vm_id, vcpu_id) = self.where_am_i;

        self.shared
            .interval_ns
            .store(interval_ns, Ordering::Release);
        self.shared
            .deadline_ns
            .store(deadline_ns, Ordering::Release);

        schedule_apic_timer::<H>(
            current_ns.saturating_add(interval_ns),
            Arc::clone(&self.shared),
            vm_id,
            vcpu_id,
        )
    }

    pub fn stop_timer(&mut self) -> X86VlapicResult {
        // TODO: maybe disable irq here?
        self.shared.interval_ns.store(0, Ordering::Release);
        self.shared.deadline_ns.store(0, Ordering::Release);

        self.shared.registration.invalidate_and_cancel()
    }

    /// Whether the timer mode is periodic.
    pub fn is_periodic(&self) -> bool {
        self.timer_mode() == TimerMode::Periodic
    }
}

impl<H: X86VlapicHostOps> Drop for ApicTimer<H> {
    fn drop(&mut self) {
        self.shared.interval_ns.store(0, Ordering::Release);
        self.shared.deadline_ns.store(0, Ordering::Release);
        if let Err(error) = self.shared.registration.invalidate_and_cancel() {
            log::warn!("failed to cancel x86 APIC timer during teardown: {error:?}");
        }
    }
}

fn schedule_apic_timer<H>(
    deadline_nanos: u64,
    shared: Arc<ApicTimerShared<H>>,
    vm_id: X86VmId,
    vcpu_id: X86VcpuId,
) -> X86VlapicResult
where
    H: X86VlapicHostOps,
{
    let callback_shared = Arc::clone(&shared);
    shared.registration.register(
        deadline_nanos,
        Box::new(move |_| {
            let lvt = callback_shared.lvt_timer_register.load(Ordering::Acquire);
            let vector = (lvt & 0xff) as u8;
            let masked = (lvt & LVT_TIMER::Mask::SET.mask()) != 0;
            let mode = (lvt & LVT_TIMER::TimerMode::SET.mask()) >> 17;
            if !masked {
                let _ = host::inject_interrupt::<H>(vm_id, vcpu_id, vector);
            }

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

fn next_periodic_deadline_ns(deadline_ns: u64, interval_ns: u64, now_ns: u64) -> u64 {
    if deadline_ns > now_ns {
        return deadline_ns;
    }

    let missed_intervals = (now_ns - deadline_ns) / interval_ns + 1;
    deadline_ns.saturating_add(interval_ns.saturating_mul(missed_intervals))
}

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
        X86HostPhysAddr, X86HostVirtAddr, X86InterruptVector, X86TimerAction, X86TimerCallback,
        X86VcpuId, X86VlapicHostOps, X86VlapicResult, X86VmId,
        regs::lvt::LVT_TIMER::TimerMode::Value as TimerMode, timer::ApicTimer,
    };

    struct DummyHost;

    struct TestTimerState {
        callbacks: Vec<Option<X86TimerCallback>>,
        cancelled: Vec<usize>,
        block_injection: bool,
        injection_started: bool,
        allow_injection: bool,
    }

    static TEST_TIMER_STATE: Mutex<TestTimerState> = Mutex::new(TestTimerState {
        callbacks: Vec::new(),
        cancelled: Vec::new(),
        block_injection: false,
        injection_started: false,
        allow_injection: false,
    });
    static TEST_TIMER_EVENT: Condvar = Condvar::new();
    static TEST_TIMER_SERIAL: Mutex<()> = Mutex::new(());

    struct TimerHost;

    impl TimerHost {
        fn reset() {
            let mut state = TEST_TIMER_STATE.lock().unwrap();
            state.callbacks.clear();
            state.cancelled.clear();
            state.block_injection = false;
            state.injection_started = false;
            state.allow_injection = false;
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

        fn block_injection() {
            let mut state = TEST_TIMER_STATE.lock().unwrap();
            state.block_injection = true;
            state.injection_started = false;
            state.allow_injection = false;
        }

        fn wait_for_injection() {
            let mut state = TEST_TIMER_STATE.lock().unwrap();
            while !state.injection_started {
                state = TEST_TIMER_EVENT.wait(state).unwrap();
            }
        }

        fn release_injection() {
            let mut state = TEST_TIMER_STATE.lock().unwrap();
            state.allow_injection = true;
            TEST_TIMER_EVENT.notify_all();
        }
    }

    impl X86VlapicHostOps for DummyHost {
        type TimerHandle = usize;

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

        fn register_timer(
            _deadline_nanos: u64,
            _callback: X86TimerCallback,
        ) -> X86VlapicResult<Self::TimerHandle> {
            Err(crate::X86VlapicError::TimerUnavailable)
        }

        fn cancel_timer(_handle: Self::TimerHandle) -> X86VlapicResult {
            Ok(())
        }

        fn current_vm_id() -> X86VmId {
            0
        }

        fn current_vm_vcpu_num() -> usize {
            1
        }

        fn current_vm_active_vcpus() -> usize {
            1
        }

        fn active_vcpus(_vm_id: X86VmId) -> Option<usize> {
            Some(1)
        }

        fn inject_interrupt(
            _vm_id: X86VmId,
            _vcpu_id: X86VcpuId,
            _vector: X86InterruptVector,
        ) -> X86VlapicResult {
            Ok(())
        }
    }

    impl X86VlapicHostOps for TimerHost {
        type TimerHandle = usize;

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

        fn register_timer(
            _deadline_nanos: u64,
            callback: X86TimerCallback,
        ) -> X86VlapicResult<Self::TimerHandle> {
            let mut state = TEST_TIMER_STATE.lock().unwrap();
            state.callbacks.push(Some(callback));
            Ok(state.callbacks.len())
        }

        fn cancel_timer(token: Self::TimerHandle) -> X86VlapicResult {
            let mut state = TEST_TIMER_STATE.lock().unwrap();
            state.cancelled.push(token);
            state.callbacks[token - 1].take();
            Ok(())
        }

        fn current_vm_id() -> X86VmId {
            0
        }

        fn current_vm_vcpu_num() -> usize {
            1
        }

        fn current_vm_active_vcpus() -> usize {
            1
        }

        fn active_vcpus(_vm_id: X86VmId) -> Option<usize> {
            Some(1)
        }

        fn inject_interrupt(
            _vm_id: X86VmId,
            _vcpu_id: X86VcpuId,
            _vector: X86InterruptVector,
        ) -> X86VlapicResult {
            let mut state = TEST_TIMER_STATE.lock().unwrap();
            if state.block_injection {
                state.injection_started = true;
                TEST_TIMER_EVENT.notify_all();
                while !state.allow_injection {
                    state = TEST_TIMER_EVENT.wait(state).unwrap();
                }
            }
            Ok(())
        }
    }

    #[test]
    fn test_apic_timer_creation() {
        let vm_id = 1;
        let vcpu_id = 0;
        let timer = ApicTimer::<DummyHost>::new(vm_id, vcpu_id);
        // Initial state should be stopped
        assert!(!timer.is_started());
        assert_eq!(timer.read_icr(), 0);
        assert_eq!(timer.read_dcr(), 0);
        // assert_eq!(timer.read_ccr(), 0);
        assert!(timer.is_masked());
        assert_eq!(timer.timer_mode(), TimerMode::OneShot);
        assert_eq!(timer.read_lvt() & 0xff, 0);
    }

    #[test]
    fn test_lvt_register_operations() {
        let vm_id = 1;
        let vcpu_id = 0;
        let mut timer = ApicTimer::<DummyHost>::new(vm_id, vcpu_id);

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
        let mut timer = ApicTimer::<DummyHost>::new(vm_id, vcpu_id);

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
        let mut timer = ApicTimer::<DummyHost>::new(vm_id, vcpu_id);

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
        let mut timer = ApicTimer::<DummyHost>::new(vm_id, vcpu_id);

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
        let timer1 = ApicTimer::<DummyHost>::new(vm_id, 0);
        let timer2 = ApicTimer::<DummyHost>::new(vm_id, 1);

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
        let mut timer = ApicTimer::<TimerHost>::new(1, 0);
        timer.write_lvt(0x20040).unwrap();
        timer.write_icr(1).unwrap();

        TimerHost::fire(1, 2);
        assert_eq!(TimerHost::registration_count(), 1);
        timer.write_icr(0).unwrap();

        assert_eq!(TimerHost::cancelled(), self::std::vec![1]);
    }

    #[test]
    fn stopping_timer_waits_for_a_claimed_callback() {
        let _serial = TEST_TIMER_SERIAL.lock().unwrap();
        TimerHost::reset();
        TimerHost::block_injection();
        let timer = Arc::new(Mutex::new(ApicTimer::<TimerHost>::new(1, 0)));
        {
            let mut timer = timer.lock().unwrap();
            timer.write_lvt(0x20040).unwrap();
            timer.write_icr(1).unwrap();
        }

        let firing = thread::spawn(|| TimerHost::fire(1, 2));
        TimerHost::wait_for_injection();

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

        TimerHost::release_injection();
        firing.join().unwrap();
        let cancellation = returned_while_callback_was_running
            .unwrap_or_else(|| done_rx.recv_timeout(Duration::from_secs(1)).unwrap());
        cancelling.join().unwrap();

        assert!(
            returned_while_callback_was_running.is_none(),
            "timer stop returned before its claimed callback completed"
        );
        cancellation.unwrap();
    }
}
