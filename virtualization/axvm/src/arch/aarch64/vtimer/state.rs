//! Connects vCPU-owned architectural timer state to VGIC PPIs and host wakeups.

use std::{
    boxed::Box,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use aarch64_cpu_ext::registers::{CNTPCT_EL0, Readable};
use arm_vcpu::{ArmTimerKind, ArmTimerSnapshot};
use arm_vgic::{GicVcpuId, PpiId, VgicCore, VgicResult};
use ax_std::os::arceos::sync::IrqSafeMutex;

use crate::{
    arch::aarch64::gic::AxvmVgicBackend,
    host::{HostCpu, HostHardTimerAction, HostTime, HostTimer, default_host},
};

type KernelTimerHandle = <crate::host::arceos::ArceOsHost as HostTimer>::TimerHandle;

const NANOS_PER_SECOND: u128 = 1_000_000_000;

#[derive(Clone, Copy)]
struct HostTimerActivation {
    token: usize,
    owner_cpu: usize,
}

#[derive(Clone, Copy)]
struct ScheduledWaitTimer {
    handle: KernelTimerHandle,
    owner_cpu: usize,
    owner_thread: crate::host::task::ThreadId,
    epoch: u64,
}

fn wait_timer_owner_matches(
    owner_cpu: usize,
    owner_thread: crate::host::task::ThreadId,
    current_cpu: usize,
    current_thread: crate::host::task::ThreadId,
) -> bool {
    owner_cpu == current_cpu && owner_thread == current_thread
}

pub(in crate::arch::aarch64) type Aarch64TimerWaitToken = crate::vm::VcpuTimerWaitToken;

struct Aarch64TimerWaitState {
    completion: crate::vm::VcpuTimerWaitGeneration,
    deadline_counter: AtomicU64,
    armed_timer_epoch: AtomicU64,
    next_timer_epoch: AtomicU64,
    active_timer_epoch: AtomicU64,
}

impl Aarch64TimerWaitState {
    const fn new() -> Self {
        Self {
            completion: crate::vm::VcpuTimerWaitGeneration::new(),
            deadline_counter: AtomicU64::new(0),
            armed_timer_epoch: AtomicU64::new(0),
            next_timer_epoch: AtomicU64::new(0),
            active_timer_epoch: AtomicU64::new(0),
        }
    }

    fn arm(&self, deadline_counter: u64, timer_epoch: u64) -> Aarch64TimerWaitToken {
        self.deadline_counter
            .store(deadline_counter, Ordering::Relaxed);
        self.armed_timer_epoch.store(timer_epoch, Ordering::Relaxed);
        self.completion.arm()
    }

    fn armed_deadline_for_epoch(&self, timer_epoch: u64) -> Option<(Aarch64TimerWaitToken, u64)> {
        loop {
            let token = self.completion.armed_token()?;
            let deadline_counter = self.deadline_counter.load(Ordering::Relaxed);
            let armed_timer_epoch = self.armed_timer_epoch.load(Ordering::Relaxed);
            if self.completion.armed_token() == Some(token) {
                return (armed_timer_epoch == timer_epoch).then_some((token, deadline_counter));
            }
        }
    }

    fn begin_timer_epoch(&self) -> u64 {
        let epoch = self
            .next_timer_epoch
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |epoch| {
                epoch.checked_add(1)
            })
            .unwrap_or_else(|_| panic!("AArch64 hard timer epoch exhausted"))
            .checked_add(1)
            .expect("AArch64 hard timer epoch must remain finite");
        self.active_timer_epoch.store(epoch, Ordering::Release);
        epoch
    }

    fn timer_epoch_is_active(&self, epoch: u64) -> bool {
        self.active_timer_epoch.load(Ordering::Acquire) == epoch
    }

    fn retire_timer_epoch(&self, epoch: u64) {
        let _ =
            self.active_timer_epoch
                .compare_exchange(epoch, 0, Ordering::AcqRel, Ordering::Acquire);
    }

    fn publish_completion_for_epoch(&self, timer_epoch: u64, token: Aarch64TimerWaitToken) -> bool {
        if self.armed_timer_epoch.load(Ordering::Acquire) != timer_epoch {
            return false;
        }
        // The completed generation is the Linux kvm_vcpu_check_block-style
        // condition. Publish it before the direct wake so a wake-before-park
        // interleaving cannot merely wake and then re-enter the wait loop.
        self.completion.complete(token)
    }

    fn cancel_arm(&self, token: Aarch64TimerWaitToken) {
        self.completion.cancel(token);
    }

    fn is_completed(&self, token: Aarch64TimerWaitToken) -> bool {
        self.completion.is_completed(token)
    }

    fn invalidate(&self) -> bool {
        self.completion.invalidate()
    }
}

/// Bridges one vCPU's canonical timer contexts into its private VGIC lines.
///
/// The binding owns only delivery plumbing. Compare values, controls, and
/// interrupt conditions remain in `arm_vcpu`; pending/active/EOI state remains
/// in the VGIC.
pub(in crate::arch::aarch64) struct Aarch64TimerBinding {
    vgic: Arc<VgicCore>,
    backend: Arc<AxvmVgicBackend>,
    vcpu: GicVcpuId,
    virtual_ppi: PpiId,
    physical_ppi: PpiId,
    host_virtual_timer_intid: u32,
    frequency: u64,
    registered: AtomicBool,
    wait_state: Arc<Aarch64TimerWaitState>,
    scheduled: IrqSafeMutex<Option<ScheduledWaitTimer>>,
    host_activation: IrqSafeMutex<Option<HostTimerActivation>>,
}

impl Aarch64TimerBinding {
    pub(in crate::arch::aarch64) fn new(
        vgic: Arc<VgicCore>,
        backend: Arc<AxvmVgicBackend>,
        vcpu: GicVcpuId,
        virtual_ppi: PpiId,
        physical_ppi: PpiId,
        host_virtual_timer_intid: u32,
        frequency: u64,
    ) -> VgicResult<Arc<Self>> {
        let binding = Arc::new(Self {
            vgic,
            backend: backend.clone(),
            vcpu,
            virtual_ppi,
            physical_ppi,
            host_virtual_timer_intid,
            frequency,
            registered: AtomicBool::new(false),
            wait_state: Arc::new(Aarch64TimerWaitState::new()),
            scheduled: IrqSafeMutex::new(None),
            host_activation: IrqSafeMutex::new(None),
        });
        backend.register_timer_ppi(vcpu, virtual_ppi, Arc::downgrade(&binding))?;
        binding.registered.store(true, Ordering::Release);
        Ok(binding)
    }

    /// Completes a banked PPI activation before this vCPU migrates to another pCPU.
    pub(in crate::arch::aarch64) fn prepare_run(&self) -> VgicResult {
        let current_cpu = default_host().this_cpu_id();
        let activation = {
            let mut active = self.host_activation.lock();
            if active
                .as_ref()
                .is_some_and(|activation| activation.owner_cpu != current_cpu)
            {
                active.take()
            } else {
                None
            }
        };
        if let Some(activation) = activation {
            if let Err(error) = self.complete_host_activation(activation) {
                *self.host_activation.lock() = Some(activation);
                return Err(error);
            }
        }
        Ok(())
    }

    /// Claims one acknowledged host CNTV PPI without deactivating it.
    pub(in crate::arch::aarch64) fn accept_host_irq(&self, token: usize) -> bool {
        if super::super::gic::host_irq_intid(token) != self.host_virtual_timer_intid {
            return false;
        }
        let activation = HostTimerActivation {
            token,
            owner_cpu: default_host().this_cpu_id(),
        };
        let mut active = self.host_activation.lock();
        if active.is_some() {
            drop(active);
            super::super::gic::deactivate_host_irq(token);
        } else {
            *active = Some(activation);
        }
        true
    }

    /// Publishes the current timer output levels before VGIC state is saved.
    pub(in crate::arch::aarch64) fn synchronize(&self, snapshot: ArmTimerSnapshot) -> VgicResult {
        self.invalidate_wait();
        self.publish_levels(snapshot, physical_counter())
            .map(|_| ())
    }

    /// Recomputes timer outputs immediately before the VGIC is loaded.
    pub(in crate::arch::aarch64) fn publish_for_entry(
        &self,
        snapshot: ArmTimerSnapshot,
    ) -> VgicResult {
        self.publish_levels(snapshot, physical_counter())
            .map(|_| ())
    }

    /// Re-evaluates both timers and arms the earliest wakeup for guest WFI.
    pub(in crate::arch::aarch64) fn arm_wait(
        &self,
        snapshot: ArmTimerSnapshot,
    ) -> VgicResult<Option<Aarch64TimerWaitToken>> {
        self.invalidate_wait();
        let now_counter = physical_counter();
        if self.publish_levels(snapshot, now_counter)? {
            return Ok(None);
        }
        let Some(deadline_counter) = snapshot.earliest_deadline(now_counter) else {
            return Ok(None);
        };

        let deadline_ns = host_deadline_ns(deadline_counter, now_counter, self.frequency);
        let deadline = Duration::from_nanos(deadline_ns);
        let current_cpu = default_host().this_cpu_id();
        let current_thread = crate::host::task::current_thread();
        let owner_thread = current_thread.id();
        let existing = *self.scheduled.lock();
        if let Some(existing) = existing
            && wait_timer_owner_matches(
                existing.owner_cpu,
                existing.owner_thread,
                current_cpu,
                owner_thread,
            )
        {
            let wait_token = self.wait_state.arm(deadline_counter, existing.epoch);
            default_host()
                .arm_hard_timer(existing.handle, deadline)
                .map_err(|error| {
                    self.wait_state.cancel_arm(wait_token);
                    arm_vgic::VgicError::Backend {
                        operation: "arm blocked-vCPU architectural timer",
                        detail: std::format!("stable hard timer arm failed: {error}"),
                    }
                })?;
            return Ok(Some(wait_token));
        }

        if let Some(previous) = self.scheduled.lock().take() {
            self.wait_state.retire_timer_epoch(previous.epoch);
            cancel_wait_timer(previous.handle);
        }
        let epoch = self.wait_state.begin_timer_epoch();
        let wait_token = self.wait_state.arm(deadline_counter, epoch);
        let frequency = self.frequency;
        let wait_state = Arc::clone(&self.wait_state);
        let wake = current_thread.wake_handle();
        let registration = unsafe {
            // SAFETY: the stable callback reads only the architectural counter
            // and atomically published arm state, then invokes the prebound
            // hard-IRQ-safe ThreadWakeHandle. It does not allocate, free,
            // sleep, log, or acquire AxVM/VGIC/runtime locks.
            default_host().register_hard_restartable_timer(
                deadline,
                Box::new(move |_| {
                    if !wait_state.timer_epoch_is_active(epoch) {
                        return HostHardTimerAction::Complete;
                    }
                    let Some((wait_token, deadline_counter)) =
                        wait_state.armed_deadline_for_epoch(epoch)
                    else {
                        return HostHardTimerAction::Disarm;
                    };
                    let now_counter = physical_counter();
                    if !counter_reached_deadline(now_counter, deadline_counter) {
                        let deadline_ns =
                            host_deadline_ns(deadline_counter, now_counter, frequency);
                        return HostHardTimerAction::Rearm(Duration::from_nanos(deadline_ns));
                    }
                    if wait_state.publish_completion_for_epoch(epoch, wait_token) {
                        let _result = wake.wake();
                    }
                    HostHardTimerAction::Disarm
                }),
            )
        };
        let handle = registration.map_err(|error| {
            self.wait_state.retire_timer_epoch(epoch);
            self.wait_state.cancel_arm(wait_token);
            arm_vgic::VgicError::Backend {
                operation: "register blocked-vCPU architectural timer",
                detail: std::format!("stable hard timer registration failed: {error}"),
            }
        })?;
        *self.scheduled.lock() = Some(ScheduledWaitTimer {
            handle,
            owner_cpu: current_cpu,
            owner_thread,
            epoch,
        });
        Ok(Some(wait_token))
    }

    pub(in crate::arch::aarch64) fn timer_wait_completed(
        &self,
        token: Aarch64TimerWaitToken,
    ) -> bool {
        self.wait_state.is_completed(token)
    }

    /// Invalidates and remotely cancels any scheduled wait callback.
    pub(in crate::arch::aarch64) fn invalidate_wait(&self) {
        if !self.wait_state.invalidate() {
            return;
        }
        let scheduled = *self.scheduled.lock();
        if let Some(scheduled) = scheduled
            && let Err(error) = default_host().disarm_hard_timer(scheduled.handle)
        {
            warn!("failed to disarm blocked-vCPU architectural timer: {error}");
        }
    }

    /// Clears both private timer lines and invalidates all scheduled work.
    pub(in crate::arch::aarch64) fn reset(&self) -> VgicResult {
        self.invalidate_wait();
        let controller = self.vgic.controller();
        controller.set_ppi_level(self.vcpu, self.virtual_ppi, false)?;
        controller.set_ppi_level(self.vcpu, self.physical_ppi, false)?;
        self.retire_host_activation()
    }

    fn publish_levels(
        &self,
        snapshot: ArmTimerSnapshot,
        physical_counter: u64,
    ) -> VgicResult<bool> {
        let virtual_level = snapshot.irq_asserted(ArmTimerKind::Virtual, physical_counter);
        let physical_level = snapshot.irq_asserted(ArmTimerKind::Physical, physical_counter);
        let controller = self.vgic.controller();
        controller.set_ppi_level(self.vcpu, self.virtual_ppi, virtual_level)?;
        controller.set_ppi_level(self.vcpu, self.physical_ppi, physical_level)?;
        Ok(virtual_level || physical_level)
    }

    pub(in crate::arch::aarch64) fn retire_host_activation(&self) -> VgicResult {
        let activation = self.host_activation.lock().take();
        let Some(activation) = activation else {
            return Ok(());
        };
        if let Err(error) = self.complete_host_activation(activation) {
            *self.host_activation.lock() = Some(activation);
            return Err(error);
        }
        Ok(())
    }

    fn complete_host_activation(&self, activation: HostTimerActivation) -> VgicResult {
        let current_cpu = default_host().this_cpu_id();
        if activation.owner_cpu == current_cpu {
            super::super::gic::deactivate_host_irq(activation.token);
            return Ok(());
        }

        let mut token = activation.token;
        crate::host::task::run_on_cpu_sync(
            activation.owner_cpu,
            deactivate_host_timer_irq,
            (&mut token as *mut usize).cast(),
        )
        .map_err(|error| arm_vgic::VgicError::Backend {
            operation: "deactivate host virtual-timer PPI",
            detail: std::format!(
                "cannot run completion on owner CPU {}: {error:?}",
                activation.owner_cpu
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_timer_reuse_requires_the_same_cpu_and_thread_generation() {
        let owner = crate::host::task::ThreadId::from_parts(3, 7);

        assert!(wait_timer_owner_matches(2, owner, 2, owner));
        assert!(!wait_timer_owner_matches(
            2,
            owner,
            2,
            crate::host::task::ThreadId::from_parts(3, 8)
        ));
        assert!(!wait_timer_owner_matches(2, owner, 1, owner));
    }

    #[test]
    fn stale_timer_epoch_cannot_complete_rearmed_wait() {
        let state = Aarch64TimerWaitState::new();
        let old_epoch = state.begin_timer_epoch();
        let old_token = state.arm(10, old_epoch);

        // Model an old hard callback that crossed its first epoch check before
        // migration invalidated the registration and armed a new wait.
        assert!(state.timer_epoch_is_active(old_epoch));
        assert!(state.invalidate());
        let new_epoch = state.begin_timer_epoch();
        let new_token = state.arm(20, new_epoch);
        assert_ne!(old_token, new_token);
        assert_ne!(old_epoch, new_epoch);

        let (observed_token, _) = state.armed_deadline_for_epoch(new_epoch).unwrap();
        assert_eq!(observed_token, new_token);
        assert!(!state.publish_completion_for_epoch(old_epoch, observed_token));
        assert!(!state.is_completed(new_token));
    }
}

fn cancel_wait_timer(handle: KernelTimerHandle) {
    if let Err(error) = default_host().cancel_timer(handle) {
        warn!("failed to cancel blocked-vCPU architectural timer: {error}");
    }
}

impl Drop for Aarch64TimerBinding {
    fn drop(&mut self) {
        if self.registered.swap(false, Ordering::AcqRel) {
            self.backend
                .unregister_timer_ppi(self.vcpu, self.virtual_ppi);
        }
        self.wait_state.invalidate();
        if let Some(scheduled) = self.scheduled.lock().take() {
            self.wait_state.retire_timer_epoch(scheduled.epoch);
            cancel_wait_timer(scheduled.handle);
        }
        if let Some(activation) = self.host_activation.lock().take()
            && let Err(error) = self.complete_host_activation(activation)
        {
            warn!("failed to complete host timer PPI while dropping binding: {error}");
        }
    }
}

/// # Safety
///
/// `arg` must point to a live `usize` for the duration of the synchronous
/// cross-CPU call.
unsafe fn deactivate_host_timer_irq(arg: *mut ()) {
    let token = unsafe { *arg.cast::<usize>() };
    super::super::gic::deactivate_host_irq(token);
}

pub(in crate::arch::aarch64) fn physical_counter() -> u64 {
    CNTPCT_EL0.get()
}

fn host_deadline_ns(deadline_counter: u64, now_counter: u64, frequency: u64) -> u64 {
    let now_ns = default_host().monotonic_time().as_nanos();
    let remaining_ticks = deadline_counter.wrapping_sub(now_counter) as i64;
    if remaining_ticks <= 0 {
        return now_ns.min(u128::from(u64::MAX)) as u64;
    }
    let delta_ns = (remaining_ticks as u128)
        .saturating_mul(NANOS_PER_SECOND)
        .saturating_add(u128::from(frequency - 1))
        / u128::from(frequency);
    now_ns.saturating_add(delta_ns).min(u128::from(u64::MAX)) as u64
}

fn counter_reached_deadline(counter: u64, deadline: u64) -> bool {
    counter.wrapping_sub(deadline) as i64 >= 0
}
