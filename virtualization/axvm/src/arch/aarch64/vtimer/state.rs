//! Connects vCPU-owned architectural timer state to VGIC PPIs and host wakeups.

use std::{
    boxed::Box,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use aarch64_cpu_ext::registers::{CNTPCT_EL0, Readable};
use arm_vcpu::{ArmTimerKind, ArmTimerSnapshot};
use arm_vgic::{GicVcpuId, PpiId, VgicCore, VgicResult};
use ax_std::os::arceos::sync::IrqSafeMutex;

use crate::{
    arch::aarch64::gic::AxvmVgicBackend,
    host::{HostCpu, HostTime, default_host},
    timer::VmTimerHandle,
};

const NANOS_PER_SECOND: u128 = 1_000_000_000;

#[derive(Clone, Copy)]
struct HostTimerActivation {
    token: usize,
    owner_cpu: usize,
}

/// Bridges one vCPU's canonical timer contexts into its private VGIC lines.
///
/// The binding owns only delivery plumbing. Compare values, controls, and
/// interrupt conditions remain in `arm_vcpu`; pending/active/EOI state remains
/// in the VGIC.
pub(in crate::arch::aarch64) struct Aarch64TimerBinding {
    vm_id: usize,
    vgic: Arc<VgicCore>,
    backend: Arc<AxvmVgicBackend>,
    vcpu: GicVcpuId,
    virtual_ppi: PpiId,
    physical_ppi: PpiId,
    host_virtual_timer_intid: u32,
    frequency: u64,
    registered: AtomicBool,
    wait_generation: AtomicU64,
    scheduled: IrqSafeMutex<Option<VmTimerHandle>>,
    host_activation: IrqSafeMutex<Option<HostTimerActivation>>,
}

impl Aarch64TimerBinding {
    pub(in crate::arch::aarch64) fn new(
        vm_id: usize,
        vgic: Arc<VgicCore>,
        backend: Arc<AxvmVgicBackend>,
        vcpu: GicVcpuId,
        virtual_ppi: PpiId,
        physical_ppi: PpiId,
        host_virtual_timer_intid: u32,
        frequency: u64,
    ) -> VgicResult<Arc<Self>> {
        let binding = Arc::new(Self {
            vm_id,
            vgic,
            backend: backend.clone(),
            vcpu,
            virtual_ppi,
            physical_ppi,
            host_virtual_timer_intid,
            frequency,
            registered: AtomicBool::new(false),
            wait_generation: AtomicU64::new(0),
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

    /// Re-evaluates both timers and arms the earliest wakeup for guest WFI.
    pub(in crate::arch::aarch64) fn arm_wait(
        self: &Arc<Self>,
        snapshot: ArmTimerSnapshot,
    ) -> VgicResult {
        self.invalidate_wait();
        let now_counter = physical_counter();
        if self.publish_levels(snapshot, now_counter)? {
            return Ok(());
        }
        let Some(deadline_counter) = snapshot.earliest_deadline(now_counter) else {
            return Ok(());
        };

        let generation = self
            .wait_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let deadline_ns = host_deadline_ns(deadline_counter, now_counter, self.frequency);
        let binding = Arc::downgrade(self);
        let handle = crate::timer::register_timer_handle(
            deadline_ns,
            Box::new(move |_| {
                let Some(binding) = binding.upgrade() else {
                    return;
                };
                if binding
                    .wait_generation
                    .compare_exchange(
                        generation,
                        generation.wrapping_add(1),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    return;
                }
                binding.scheduled.lock().take();
                if let Err(error) =
                    crate::runtime::vcpus::notify_vcpu(binding.vm_id, binding.vcpu.raw())
                {
                    warn!(
                        "failed to wake VM[{}] vCPU {} for architectural timer: {error:?}",
                        binding.vm_id,
                        binding.vcpu.raw()
                    );
                }
            }),
        );

        let (stale, previous) = {
            let mut scheduled = self.scheduled.lock();
            if self.wait_generation.load(Ordering::Acquire) != generation {
                (true, None)
            } else {
                (false, scheduled.replace(handle))
            }
        };
        if stale {
            crate::timer::cancel_timer_handle(handle);
        } else if let Some(previous) = previous {
            crate::timer::cancel_timer_handle(previous);
        }
        Ok(())
    }

    /// Invalidates and remotely cancels any scheduled wait callback.
    pub(in crate::arch::aarch64) fn invalidate_wait(&self) {
        self.wait_generation.fetch_add(1, Ordering::AcqRel);
        let scheduled = self.scheduled.lock().take();
        if let Some(handle) = scheduled {
            crate::timer::cancel_timer_handle(handle);
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

impl Drop for Aarch64TimerBinding {
    fn drop(&mut self) {
        if self.registered.swap(false, Ordering::AcqRel) {
            self.backend
                .unregister_timer_ppi(self.vcpu, self.virtual_ppi);
        }
        self.invalidate_wait();
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
