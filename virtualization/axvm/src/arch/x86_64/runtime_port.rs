//! Run-scoped ports for x86 local-APIC and PIT interrupt devices.

use std::sync::{Arc, OnceLock};

use ax_std::os::arceos::sync::RawSpinLock;
use x86_vlapic::{
    IoApicInterrupt, X86InterruptVector, X86TimerAction, X86TimerCallback, X86VcpuId,
    X86VlapicError, X86VlapicResult, X86VlapicRuntimeOps, X86VmId,
};

use crate::{
    InterruptTriggerMode,
    host::{HostHardTimerAction, HostTimer, HostTimerAction, default_host},
    irq::model::{PendingVcpuInterrupt, VcpuTimerIngress, VirtualInterruptId},
    services::{RunSignals, SignalError},
};

pub(crate) type X86TimerHandle = <crate::host::arceos::ArceOsHost as HostTimer>::TimerHandle;

struct X86RunBindingState {
    signals: Option<Arc<RunSignals>>,
    pic: Option<Arc<dyn axdevice::X86PicDeviceOps>>,
    ioapic: Option<Arc<dyn axdevice::X86InterruptDomainOps>>,
}

impl X86RunBindingState {
    /// One unbound run identity with no lower capabilities installed.
    const fn empty() -> Self {
        Self {
            signals: None,
            pic: None,
            ioapic: None,
        }
    }
}

/// Shared run binding used by every x86 interrupt device in one VM.
pub(crate) struct X86RunBinding {
    state: RawSpinLock<X86RunBindingState>,
}

impl X86RunBinding {
    pub(crate) fn new() -> Self {
        Self {
            state: RawSpinLock::new(X86RunBindingState::empty()),
        }
    }

    /// Binds this run's lower capabilities. Task context, before any entry.
    ///
    /// Interrupts are saved for the whole critical section because the same
    /// state is read from hard-IRQ producers (see [`Self::kick_from_irq`]).
    pub(crate) fn bind(
        &self,
        signals: Arc<RunSignals>,
        pic: Option<Arc<dyn axdevice::X86PicDeviceOps>>,
        ioapic: Option<Arc<dyn axdevice::X86InterruptDomainOps>>,
    ) {
        // Swap the whole binding under the guard and drop the displaced state
        // after releasing it: the replaced `Arc`s may be the last reference to
        // a run's signals/controllers, and dropping them inside an IRQ-off raw
        // critical section could enter a sleeping or task-context teardown.
        let previous = {
            let mut state = self.state.lock_irqsave();
            std::mem::replace(
                &mut *state,
                X86RunBindingState {
                    signals: Some(signals),
                    pic,
                    ioapic,
                },
            )
        };
        drop(previous);
    }

    /// Retires the run binding so no stale timer or line can publish into it.
    pub(crate) fn clear(&self) {
        // Same reasoning as `bind`: retire the run identity under the guard but
        // drop the released lower capabilities outside it.
        let previous = {
            let mut state = self.state.lock_irqsave();
            std::mem::replace(&mut *state, X86RunBindingState::empty())
        };
        drop(previous);
    }

    /// Publishes one IRQ-source kick to `vcpu_id` over the bound run, if any.
    ///
    /// Safe to call from a hard IRQ producer: the raw guard is released before
    /// the wake is issued, so no sleeping path runs under the raw lock. A
    /// binding that has no run yet publishes nothing.
    pub(crate) fn kick_from_irq(&self, vcpu_id: X86VcpuId) -> Result<(), SignalError> {
        let signals = self.state.lock_irqsave().signals.clone();
        match signals {
            Some(signals) => signals.kick_from_irq(vcpu_id),
            None => Ok(()),
        }
    }

    fn snapshot(&self) -> X86RunBindingState {
        self.state.lock_irqsave().clone()
    }

    pub(super) fn current_signals(&self) -> Option<Arc<RunSignals>> {
        self.state.lock_irqsave().signals.clone()
    }
}

impl Default for X86RunBinding {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for X86RunBindingState {
    fn clone(&self) -> Self {
        Self {
            signals: self.signals.clone(),
            pic: self.pic.clone(),
            ioapic: self.ioapic.clone(),
        }
    }
}

/// A pre-bound source port for one vCPU-owned x86 interrupt device.
#[derive(Clone)]
pub(crate) struct AxvmX86VlapicRuntime {
    vm_id: X86VmId,
    vcpu_id: X86VcpuId,
    binding: Arc<X86RunBinding>,
    /// The first successful binding this device instance observed.
    ///
    /// Devices are rebuilt for every run, so one runtime instance belongs to
    /// exactly one run. Pinning that binding makes its timer callbacks and IPIs
    /// publish into the run that created them instead of into a later run that
    /// reuses the shared [`X86RunBinding`].
    port: OnceLock<X86RunBindingState>,
    timer_ingress: Arc<VcpuTimerIngress>,
}

impl AxvmX86VlapicRuntime {
    pub(crate) fn new(vm_id: X86VmId, vcpu_id: X86VcpuId, binding: Arc<X86RunBinding>) -> Self {
        Self {
            vm_id,
            vcpu_id,
            binding,
            port: OnceLock::new(),
            timer_ingress: Arc::new(VcpuTimerIngress::new()),
        }
    }

    /// Returns this device's run-bound port, pinning it on first use.
    fn bound_port(&self) -> X86VlapicResult<X86RunBindingState> {
        if let Some(port) = self.port.get() {
            return Ok(port.clone());
        }
        let snapshot = self.binding.snapshot();
        if snapshot.signals.is_none() {
            return Err(X86VlapicError::BadState);
        }
        Ok(self.port.get_or_init(|| snapshot).clone())
    }

    fn bound_signals(&self) -> X86VlapicResult<Arc<RunSignals>> {
        self.bound_port()?.signals.ok_or(X86VlapicError::BadState)
    }

    fn publish_virtual(
        &self,
        signals: &Arc<RunSignals>,
        target_vcpu_id: X86VcpuId,
        vector: X86InterruptVector,
        trigger: InterruptTriggerMode,
    ) -> X86VlapicResult {
        signals
            .publish(
                target_vcpu_id,
                PendingVcpuInterrupt {
                    id: VirtualInterruptId(vector.into()),
                    trigger,
                    source: None,
                },
            )
            .map_err(signal_error)?;
        signals.kick(target_vcpu_id).map_err(signal_error)?;
        Ok(())
    }

    fn publish_legacy_pic(
        &self,
        signals: &Arc<RunSignals>,
        target_vcpu_id: X86VcpuId,
        vector: u8,
    ) -> X86VlapicResult {
        signals
            .publish_queued(
                target_vcpu_id,
                crate::runtime::QueuedVcpuInterrupt::LegacyPic { vector },
            )
            .map_err(signal_error)?;
        signals.kick(target_vcpu_id).map_err(signal_error)?;
        Ok(())
    }

    fn wake_after_timer(&self, signals: &Arc<RunSignals>, from_irq: bool) -> X86VlapicResult {
        signals.notify_work().map_err(signal_error)?;
        if from_irq {
            signals.kick_from_irq(self.vcpu_id).map_err(signal_error)
        } else {
            signals.kick(self.vcpu_id).map_err(signal_error)
        }
    }

    fn route_pit_irq(&self, snapshot: &X86RunBindingState) -> X86VlapicResult {
        let signals = snapshot.signals.as_ref().ok_or(X86VlapicError::BadState)?;
        let pic_interrupt = snapshot.pic.as_ref().and_then(|pic| pic.claim_irq(0));
        let ioapic_interrupts = [
            snapshot
                .ioapic
                .as_ref()
                .and_then(|ioapic| ioapic.assert_gsi(0)),
            snapshot
                .ioapic
                .as_ref()
                .and_then(|ioapic| ioapic.assert_gsi(2)),
        ];

        let mut first_error = None;
        if let Some(claim) = pic_interrupt {
            let pic = snapshot
                .pic
                .as_ref()
                .expect("a PIC claim retains its originating controller");
            if let Err(error) = self.publish_legacy_pic(signals, self.vcpu_id, claim.vector()) {
                pic.restore_interrupt(claim);
                first_error = Some(error);
            }
        }

        for interrupt in ioapic_interrupts.into_iter().flatten() {
            let trigger = if interrupt.level_triggered {
                InterruptTriggerMode::LevelTriggered
            } else {
                InterruptTriggerMode::EdgeTriggered
            };
            if let Err(error) =
                self.publish_virtual(signals, self.vcpu_id, interrupt.vector, trigger)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl X86VlapicRuntimeOps for AxvmX86VlapicRuntime {
    type TimerHandle = X86TimerHandle;

    fn vm_id(&self) -> X86VmId {
        self.vm_id
    }

    fn vcpu_id(&self) -> X86VcpuId {
        self.vcpu_id
    }

    fn vcpu_count(&self) -> usize {
        self.bound_signals()
            .map(|signals| signals.vcpu_count())
            .unwrap_or(0)
    }

    fn active_vcpu_mask(&self) -> usize {
        self.bound_signals()
            .map(|signals| signals.active_mask())
            .unwrap_or(0)
    }

    fn inject_interrupt(
        &self,
        target_vcpu_id: X86VcpuId,
        vector: X86InterruptVector,
    ) -> X86VlapicResult {
        let signals = self.bound_signals()?;
        self.publish_virtual(
            &signals,
            target_vcpu_id,
            vector,
            InterruptTriggerMode::EdgeTriggered,
        )
    }

    fn inject_pit_irq(&self) -> X86VlapicResult {
        let port = self.bound_port()?;
        self.route_pit_irq(&port)
    }

    fn register_timer(
        &self,
        deadline_nanos: u64,
        mut callback: X86TimerCallback,
    ) -> X86VlapicResult<Self::TimerHandle> {
        let signals = self.bound_signals()?;
        let wake = self.clone();
        let generation = self
            .timer_ingress
            .arm()
            .ok_or(X86VlapicError::TimerUnavailable)?;
        default_host()
            .register_restartable_timer(
                std::time::Duration::from_nanos(deadline_nanos),
                Box::new(move |now| {
                    if !wake.timer_ingress.publish_expiry(generation) {
                        return HostTimerAction::Complete;
                    }
                    let action = callback(now.as_nanos() as u64);
                    let _ = wake.wake_after_timer(&signals, false);
                    match action {
                        X86TimerAction::Complete => HostTimerAction::Complete,
                        X86TimerAction::Rearm(deadline) => {
                            HostTimerAction::Rearm(std::time::Duration::from_nanos(deadline))
                        }
                    }
                }),
            )
            .map_err(|_| X86VlapicError::TimerUnavailable)
    }

    unsafe fn register_hard_timer(
        &self,
        deadline_nanos: u64,
        mut callback: X86TimerCallback,
    ) -> X86VlapicResult<Self::TimerHandle> {
        let signals = self.bound_signals()?;
        let wake = self.clone();
        let generation = self
            .timer_ingress
            .arm()
            .ok_or(X86VlapicError::TimerUnavailable)?;
        unsafe {
            // SAFETY: the vLAPIC callback publishes only atomics in its device
            // owner. This wrapper then uses the run-bound signal target; it
            // performs no registry lookup, allocation, sleeping lock, or
            // external callback while interrupts remain disabled.
            default_host()
                .register_hard_restartable_timer(
                    std::time::Duration::from_nanos(deadline_nanos),
                    Box::new(move |now| {
                        if !wake.timer_ingress.publish_expiry(generation) {
                            return HostHardTimerAction::Complete;
                        }
                        let action = callback(now.as_nanos() as u64);
                        let _ = wake.wake_after_timer(&signals, true);
                        match action {
                            X86TimerAction::Complete => HostHardTimerAction::Complete,
                            X86TimerAction::Rearm(deadline) => HostHardTimerAction::Rearm(
                                std::time::Duration::from_nanos(deadline),
                            ),
                        }
                    }),
                )
                .map(Into::into)
                .map_err(|_| X86VlapicError::TimerUnavailable)
        }
    }

    fn cancel_timer(&self, handle: Self::TimerHandle) -> X86VlapicResult {
        default_host()
            .cancel_timer_and_wait(handle)
            .map(|_| {
                self.timer_ingress.close();
            })
            .map_err(|_| X86VlapicError::TimerUnavailable)
    }

    fn wait_timer_progress(&self) {
        // Task context only: the cancel barrier for a timer whose callback was
        // preempted on this same CPU (or whose host payload is still being
        // reclaimed) must yield so that work can run. This never spins, and it
        // is always called outside every raw/task lock the callback needs.
        crate::host::task::yield_now();
    }

    fn consume_timer_expiries(&self) -> u64 {
        self.timer_ingress.take_current_expiries()
    }
}

fn signal_error(_error: crate::services::SignalError) -> X86VlapicError {
    X86VlapicError::BadState
}

#[allow(dead_code)]
fn retain_ioapic_type(_interrupt: IoApicInterrupt) {}
