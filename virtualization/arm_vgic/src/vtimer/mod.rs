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

extern crate alloc;

use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::time::Duration;

use ax_kspin::SpinNoIrq;
use axdevice_base::Device;

use crate::host;

mod cntp_ctl_el0;
pub use cntp_ctl_el0::SysCntpCtlEl0;

mod cntpct_el0;
pub use cntpct_el0::SysCntpctEl0;

mod cntp_tval_el0;
pub use cntp_tval_el0::SysCntpTvalEl0;

/// The PPI used by the ARM physical virtual timer.
pub const VIRTUAL_TIMER_IRQ: u8 = 30;

/// VM execution target captured when a guest programs its virtual timer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VtimerTarget {
    vm_id: usize,
    vcpu_id: usize,
}

impl VtimerTarget {
    /// Creates a VM-local timer delivery target.
    pub const fn new(vm_id: usize, vcpu_id: usize) -> Self {
        Self { vm_id, vcpu_id }
    }

    /// Returns the owning VM ID.
    pub const fn vm_id(self) -> usize {
        self.vm_id
    }

    /// Returns the owning vCPU ID.
    pub const fn vcpu_id(self) -> usize {
        self.vcpu_id
    }
}

/// Host operations used by one VM-local virtual timer instance.
///
/// The timer register devices keep all guest-visible state in [`VtimerState`]
/// and use this narrow port only for time, scheduling and interrupt delivery.
/// That keeps the register emulation testable and prevents it from reaching
/// into a VM runtime directly.
pub trait VtimerBackend: Send + Sync {
    /// Returns the current monotonic time in nanoseconds.
    fn current_time_nanos(&self) -> u64;

    /// Schedules one callback and returns its cancellation token.
    fn register_timer(
        &self,
        deadline: Duration,
        callback: Box<dyn FnOnce(Duration) + Send + 'static>,
    ) -> usize;

    /// Cancels a callback that has not fired yet.
    fn cancel_timer(&self, token: usize);

    /// Captures the VM and vCPU that program a timer deadline.
    fn current_target(&self) -> VtimerTarget;

    /// Delivers a virtual interrupt to a previously captured VM vCPU.
    fn inject_virtual_interrupt(&self, target: VtimerTarget, vector: u8);
}

/// Default backend that forwards timer operations to the ARM VGIC host port.
///
/// A separate value is created for every VM vtimer bundle.  The host callback
/// still owns the CPU-local timer wheel and interrupt-entry mechanics.
#[derive(Default)]
pub struct HostVtimerBackend;

impl VtimerBackend for HostVtimerBackend {
    fn current_time_nanos(&self) -> u64 {
        host::current_time_nanos()
    }

    fn register_timer(
        &self,
        deadline: Duration,
        callback: Box<dyn FnOnce(Duration) + Send + 'static>,
    ) -> usize {
        host::register_timer(deadline, callback)
    }

    fn cancel_timer(&self, token: usize) {
        host::cancel_timer(token);
    }

    fn current_target(&self) -> VtimerTarget {
        VtimerTarget::new(host::current_vm_id(), host::current_vcpu_id())
    }

    fn inject_virtual_interrupt(&self, target: VtimerTarget, vector: u8) {
        host::inject_vm_vcpu_interrupt(target.vm_id(), target.vcpu_id(), vector);
    }
}

#[derive(Default)]
struct VtimerRegisters {
    control: u32,
    deadline_ns: Option<u64>,
    timer_token: Option<usize>,
    expired: bool,
    generation: u64,
    target: Option<VtimerTarget>,
    suspended_remaining_ns: Option<u64>,
}

/// Shared guest-visible state for the CNT* register devices of one VM.
pub struct VtimerState {
    registers: SpinNoIrq<VtimerRegisters>,
}

impl VtimerState {
    /// Creates a stopped virtual timer.
    pub fn new() -> Self {
        Self {
            registers: SpinNoIrq::new(VtimerRegisters::default()),
        }
    }

    /// Returns the current CNTP_CTL_EL0 value, including the computed status bit.
    pub fn control(&self, now_ns: u64) -> u32 {
        let registers = self.registers.lock();
        let expired = registers.expired
            || registers
                .deadline_ns
                .is_some_and(|deadline| deadline <= now_ns);
        (registers.control & 0b11) | (u32::from(expired) << 2)
    }

    /// Updates the writable CNTP_CTL_EL0 enable and mask bits.
    pub fn write_control(&self, value: u32, backend: &dyn VtimerBackend) {
        let inject = {
            let mut registers = self.registers.lock();
            registers.control = value & 0b11;
            (registers.expired && Self::interrupt_enabled(&registers)).then_some(registers.target)
        };
        if let Some(Some(target)) = inject {
            backend.inject_virtual_interrupt(target, VIRTUAL_TIMER_IRQ);
        }
    }

    /// Returns the remaining timer value in nanoseconds.
    pub fn timer_value(&self, now_ns: u64) -> u64 {
        self.registers
            .lock()
            .deadline_ns
            .map(|deadline| deadline.saturating_sub(now_ns))
            .unwrap_or(0)
    }

    /// Starts (or restarts) the timer with a relative value in nanoseconds.
    pub fn write_timer_value(self: &Arc<Self>, value_ns: u64, backend: Arc<dyn VtimerBackend>) {
        let target = backend.current_target();
        self.schedule_timer_value(value_ns, target, backend);
    }

    fn schedule_timer_value(
        self: &Arc<Self>,
        value_ns: u64,
        target: VtimerTarget,
        backend: Arc<dyn VtimerBackend>,
    ) {
        let now_ns = backend.current_time_nanos();
        let deadline_ns = now_ns.saturating_add(value_ns);
        let (previous_token, generation) = {
            let mut registers = self.registers.lock();
            registers.deadline_ns = Some(deadline_ns);
            registers.expired = false;
            registers.generation = registers.generation.wrapping_add(1);
            registers.target = Some(target);
            registers.suspended_remaining_ns = None;
            (registers.timer_token.take(), registers.generation)
        };
        if let Some(token) = previous_token {
            backend.cancel_timer(token);
        }

        let state = Arc::clone(self);
        let callback_backend = Arc::clone(&backend);
        let token = backend.register_timer(
            Duration::from_nanos(deadline_ns),
            Box::new(move |_| state.expire(generation, target, callback_backend)),
        );
        let mut registers = self.registers.lock();
        if registers.generation == generation && !registers.expired {
            registers.timer_token = Some(token);
        }
    }

    /// Stops a pending host timer and preserves its remaining guest-visible time.
    pub fn suspend(&self, backend: &dyn VtimerBackend) {
        let now_ns = backend.current_time_nanos();
        let token = {
            let mut registers = self.registers.lock();
            if !registers.expired && registers.suspended_remaining_ns.is_none() {
                registers.suspended_remaining_ns = registers
                    .deadline_ns
                    .map(|deadline| deadline.saturating_sub(now_ns));
            }
            // A cancelled callback can race with this operation.  Invalidate it
            // before releasing the lock, so a paused timer cannot expire.
            registers.generation = registers.generation.wrapping_add(1);
            registers.deadline_ns = None;
            registers.timer_token.take()
        };
        if let Some(token) = token {
            backend.cancel_timer(token);
        }
    }

    /// Restarts a timer that was paused by [`Self::suspend`].
    pub fn resume(self: &Arc<Self>, backend: Arc<dyn VtimerBackend>) {
        let suspended = {
            let mut registers = self.registers.lock();
            registers
                .suspended_remaining_ns
                .take()
                .and_then(|remaining| registers.target.map(|target| (remaining, target)))
        };
        if let Some((remaining, target)) = suspended {
            self.schedule_timer_value(remaining, target, backend);
        }
    }

    /// Cancels pending work and restores the timer's power-on state.
    pub fn reset(&self, backend: &dyn VtimerBackend) {
        let token = {
            let mut registers = self.registers.lock();
            let token = registers.timer_token.take();
            // Cancellation is not a synchronization point with a callback
            // already executing on another CPU.  Preserve a distinct
            // generation across reset so that callback cannot affect a later
            // re-armed timer.
            let generation = registers.generation.wrapping_add(1);
            *registers = VtimerRegisters::default();
            registers.generation = generation;
            token
        };
        if let Some(token) = token {
            backend.cancel_timer(token);
        }
    }

    fn expire(&self, generation: u64, target: VtimerTarget, backend: Arc<dyn VtimerBackend>) {
        let inject = {
            let mut registers = self.registers.lock();
            if generation != registers.generation {
                return;
            }
            registers.timer_token = None;
            registers.expired = true;
            Self::interrupt_enabled(&registers)
        };
        if inject {
            backend.inject_virtual_interrupt(target, VIRTUAL_TIMER_IRQ);
        }
    }

    fn interrupt_enabled(registers: &VtimerRegisters) -> bool {
        registers.control & 0b11 == 0b1
    }
}

impl Default for VtimerState {
    fn default() -> Self {
        Self::new()
    }
}

/// Create a collection of system register devices.
pub fn get_sysreg_device() -> Vec<Arc<dyn Device>> {
    let backend: Arc<dyn VtimerBackend> = Arc::new(HostVtimerBackend);
    let state = Arc::new(VtimerState::new());
    vec![
        Arc::new(SysCntpCtlEl0::new(Arc::clone(&state), Arc::clone(&backend))),
        Arc::new(SysCntpctEl0::new(Arc::clone(&backend))),
        Arc::new(SysCntpTvalEl0::new(state, backend)),
    ]
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, sync::Arc, vec::Vec};
    use core::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use ax_kspin::SpinNoIrq;

    use super::{VIRTUAL_TIMER_IRQ, VtimerBackend, VtimerState, VtimerTarget};

    struct TestBackend {
        now_ns: u64,
        current_target: SpinNoIrq<Option<VtimerTarget>>,
        next_token: AtomicUsize,
        cancelled: SpinNoIrq<Vec<usize>>,
        injected: SpinNoIrq<Vec<(VtimerTarget, u8)>>,
        timers: SpinNoIrq<Vec<TestTimer>>,
    }

    struct TestTimer {
        token: usize,
        callback: Option<Box<dyn FnOnce(Duration) + Send + 'static>>,
    }

    impl TestBackend {
        fn new(now_ns: u64) -> Self {
            Self {
                now_ns,
                current_target: SpinNoIrq::new(Some(VtimerTarget::new(7, 3))),
                next_token: AtomicUsize::new(1),
                cancelled: SpinNoIrq::new(Vec::new()),
                injected: SpinNoIrq::new(Vec::new()),
                timers: SpinNoIrq::new(Vec::new()),
            }
        }

        fn set_current_target(&self, target: Option<VtimerTarget>) {
            *self.current_target.lock() = target;
        }

        fn fire_timer(&self, token: usize) {
            let callback = {
                let mut timers = self.timers.lock();
                timers
                    .iter_mut()
                    .find(|timer| timer.token == token)
                    .and_then(|timer| timer.callback.take())
            };
            if let Some(callback) = callback {
                callback(Duration::from_nanos(0));
            }
        }
    }

    impl VtimerBackend for TestBackend {
        fn current_time_nanos(&self) -> u64 {
            self.now_ns
        }

        fn register_timer(
            &self,
            _deadline: Duration,
            callback: Box<dyn FnOnce(Duration) + Send + 'static>,
        ) -> usize {
            let token = self.next_token.fetch_add(1, Ordering::Relaxed);
            self.timers.lock().push(TestTimer {
                token,
                callback: Some(callback),
            });
            token
        }

        fn cancel_timer(&self, token: usize) {
            self.cancelled.lock().push(token);
        }

        fn current_target(&self) -> VtimerTarget {
            self.current_target
                .lock()
                .expect("test backend has no current vCPU context")
        }

        fn inject_virtual_interrupt(&self, target: VtimerTarget, vector: u8) {
            self.injected.lock().push((target, vector));
        }
    }

    #[test]
    fn expired_timer_is_injected_when_enabled_and_unmasked() {
        let concrete_backend = Arc::new(TestBackend::new(100));
        let backend: Arc<dyn VtimerBackend> = concrete_backend.clone();
        let state = Arc::new(VtimerState::new());
        state.write_control(1, backend.as_ref());
        state.write_timer_value(20, Arc::clone(&backend));
        state.expire(1, VtimerTarget::new(7, 3), Arc::clone(&backend));

        assert_eq!(state.control(120), 0b101);
        assert_eq!(
            *concrete_backend.injected.lock(),
            [(VtimerTarget::new(7, 3), VIRTUAL_TIMER_IRQ)]
        );
    }

    #[test]
    fn stale_timer_callback_cannot_expire_a_restarted_timer() {
        let concrete_backend = Arc::new(TestBackend::new(100));
        let backend: Arc<dyn VtimerBackend> = concrete_backend.clone();
        let state = Arc::new(VtimerState::new());
        state.write_timer_value(20, Arc::clone(&backend));
        state.write_timer_value(30, Arc::clone(&backend));
        state.expire(1, VtimerTarget::new(7, 3), Arc::clone(&backend));

        assert_eq!(state.control(120), 0);
        assert_eq!(*concrete_backend.cancelled.lock(), [1]);
    }

    #[test]
    fn unmask_delivers_an_expired_timer_to_its_programming_vcpu() {
        let concrete_backend = Arc::new(TestBackend::new(100));
        let backend: Arc<dyn VtimerBackend> = concrete_backend.clone();
        let state = Arc::new(VtimerState::new());

        state.write_timer_value(20, Arc::clone(&backend));
        state.expire(1, VtimerTarget::new(11, 2), Arc::clone(&backend));
        assert!(concrete_backend.injected.lock().is_empty());

        state.write_control(1, backend.as_ref());
        assert_eq!(
            *concrete_backend.injected.lock(),
            [(VtimerTarget::new(7, 3), VIRTUAL_TIMER_IRQ)],
            "the timer must retain the target captured when it was programmed"
        );
    }

    #[test]
    fn suspend_and_reset_invalidate_racing_callbacks() {
        let concrete_backend = Arc::new(TestBackend::new(100));
        let backend: Arc<dyn VtimerBackend> = concrete_backend.clone();
        let state = Arc::new(VtimerState::new());
        state.write_control(1, backend.as_ref());
        state.write_timer_value(20, Arc::clone(&backend));

        state.suspend(backend.as_ref());
        state.expire(1, VtimerTarget::new(7, 3), Arc::clone(&backend));
        assert_eq!(state.control(120), 1);
        assert_eq!(*concrete_backend.cancelled.lock(), [1]);

        state.resume(Arc::clone(&backend));
        state.reset(backend.as_ref());
        state.expire(3, VtimerTarget::new(7, 3), backend);
        assert_eq!(state.control(120), 0);
        assert_eq!(*concrete_backend.injected.lock(), []);
        assert_eq!(*concrete_backend.cancelled.lock(), [1, 2]);
    }

    #[test]
    fn resume_reuses_programming_target_without_current_vcpu_context() {
        let concrete_backend = Arc::new(TestBackend::new(100));
        let backend: Arc<dyn VtimerBackend> = concrete_backend.clone();
        let state = Arc::new(VtimerState::new());
        let programming_target = VtimerTarget::new(11, 2);

        concrete_backend.set_current_target(Some(programming_target));
        state.write_control(1, backend.as_ref());
        state.write_timer_value(20, Arc::clone(&backend));

        state.suspend(backend.as_ref());

        concrete_backend.set_current_target(Some(VtimerTarget::new(11, 5)));
        state.resume(Arc::clone(&backend));
        concrete_backend.fire_timer(2);
        assert_eq!(
            *concrete_backend.injected.lock(),
            [(programming_target, VIRTUAL_TIMER_IRQ)],
            "resume must keep the vCPU target captured when the guest programmed the timer"
        );

        concrete_backend.injected.lock().clear();
        state.write_timer_value(20, Arc::clone(&backend));
        state.suspend(backend.as_ref());

        concrete_backend.set_current_target(None);
        state.resume(Arc::clone(&backend));
        concrete_backend.fire_timer(4);
        assert_eq!(
            *concrete_backend.injected.lock(),
            [(VtimerTarget::new(11, 5), VIRTUAL_TIMER_IRQ)],
            "resume must not read the current vCPU context"
        );
    }
}
