//! AArch64 GIC host operations for the ArceOS-backed AxVM runtime.

use std::{
    sync::{Arc, Weak},
    vec::Vec,
};

use arm_gic_driver::v3::Trigger;
use arm_vgic::{
    AssignedSpiConfig, CpuInterfaceState, GicV3Backend, GicV3BackendError,
    GicV3HardwareCapabilities, GicV3Native, GicVcpuId, HostGicVersion, IntId,
    PhysicalInterruptBinding, PpiId, VgicBackendCapabilities, VgicError, VgicResult,
};
use ax_std::os::arceos::sync::RawSpinLock;
use axdevice_base::InterruptTrigger;

use super::vtimer::Aarch64TimerBinding;

mod cpu_interface;
mod host;
mod maintenance;
mod physical;

pub(crate) use host::prepare;
pub(crate) use physical::AssignedSpiRoutes;

pub(super) fn try_with_gic<T>(
    operation: &'static str,
    f: impl FnOnce(&mut rdif_intc::Intc) -> T,
) -> Result<T, GicV3BackendError> {
    // `rdrive` device locks are control-plane locks, not hard-IRQ-safe locks.
    // Callers may use this helper only while discovering or reconfiguring the
    // host controller. vCPU load/save and IRQ acknowledge/deactivate use the
    // cached CPU-interface capability in `cpu_interface` instead.
    let registered = rdrive::get_one::<rdif_intc::Intc>().ok_or_else(|| {
        GicV3BackendError::new(
            operation,
            "no host interrupt-controller driver is registered",
        )
    })?;
    let mut gic = registered
        .lock()
        .map_err(|_| GicV3BackendError::new(operation, "the host GIC driver lock is poisoned"))?;
    Ok(f(&mut gic))
}

#[derive(Clone, Copy, Debug)]
struct PhysicalSpiSnapshot {
    enabled: bool,
    trigger: Trigger,
    target: PhysicalSpiRegisterTarget,
}

#[derive(Clone, Copy, Debug)]
enum PhysicalSpiRegisterTarget {
    V2(arm_gic_driver::v2::TargetList),
    V3(Option<arm_gic_driver::v3::Affinity>),
}

#[derive(Clone, Copy, Debug)]
enum PhysicalSpiTarget {
    V2(arm_gic_driver::v2::CpuInterfaceTarget),
    V3(Option<arm_gic_driver::v3::Affinity>),
}

/// Checked bridge from the VM-local controller to the current host GIC.
pub(crate) struct AxvmVgicBackend {
    capabilities: VgicBackendCapabilities,
    physical_spis: RawSpinLock<Vec<Option<PhysicalSpiSnapshot>>>,
    timer_ppis: RawSpinLock<Vec<Option<Weak<Aarch64TimerBinding>>>>,
}

impl AxvmVgicBackend {
    /// Uses the host CPU-interface capabilities committed before CPU enable.
    pub(crate) fn new(vcpu_count: usize) -> Result<Self, GicV3BackendError> {
        let timer_slots = vcpu_count.checked_mul(32).ok_or_else(|| {
            GicV3BackendError::new("prepare timer PPI slots", "vCPU slot count overflow")
        })?;
        Ok(Self {
            capabilities: cpu_interface::capabilities()?,
            physical_spis: RawSpinLock::new(std::vec![None; 1020]),
            timer_ppis: RawSpinLock::new(std::vec![None; timer_slots]),
        })
    }

    pub(in crate::arch::aarch64) fn register_timer_ppi(
        &self,
        vcpu: GicVcpuId,
        ppi: PpiId,
        binding: Weak<Aarch64TimerBinding>,
    ) -> VgicResult {
        let key = timer_ppi_slot(vcpu, IntId::Ppi(ppi));
        let (conflict, previous) = {
            let mut timer_ppis = self.timer_ppis.lock_irqsave();
            match key.and_then(|key| timer_ppis.get_mut(key)) {
                Some(slot) if !slot.as_ref().is_some_and(|weak| weak.strong_count() != 0) => {
                    (false, slot.replace(binding))
                }
                _ => (true, Some(binding)),
            }
        };
        // A displaced Weak may release the final control allocation.
        drop(previous);
        if conflict {
            // The raw guard is released before the error allocates a message.
            return Err(VgicError::ResourceConflict {
                resource: "host virtual-timer PPI binding",
                detail: std::format!(
                    "vCPU {} INTID {} already has a live binding",
                    vcpu.raw(),
                    ppi.raw()
                ),
            });
        }
        Ok(())
    }

    pub(in crate::arch::aarch64) fn unregister_timer_ppi(&self, vcpu: GicVcpuId, ppi: PpiId) {
        let previous = timer_ppi_slot(vcpu, IntId::Ppi(ppi)).and_then(|key| {
            self.timer_ppis
                .lock_irqsave()
                .get_mut(key)
                .and_then(Option::take)
        });
        drop(previous);
    }

    fn physical_intid(
        &self,
        binding: PhysicalInterruptBinding,
        operation: &'static str,
    ) -> Result<arm_gic_driver::IntId, GicV3BackendError> {
        let raw = u32::try_from(binding.host().raw()).map_err(|_| {
            GicV3BackendError::value(
                operation,
                "host IRQ does not fit a GIC INTID",
                binding.host().raw(),
            )
        })?;
        if raw != binding.guest().raw() {
            return Err(GicV3BackendError::mismatch(
                operation,
                "identity forwarding requires equal guest and host INTIDs",
                binding.guest().raw() as u64,
                raw as u64,
            ));
        }
        arm_gic_driver::checked_intid(raw, 1020).map_err(|_| {
            GicV3BackendError::value(
                operation,
                "host INTID is outside the assignable GIC range",
                raw as u64,
            )
        })
    }
}

impl GicV3Backend for AxvmVgicBackend {
    fn capabilities(&self) -> VgicBackendCapabilities {
        self.capabilities
    }

    fn load_cpu_interface(
        &self,
        vcpu: GicVcpuId,
        state: &CpuInterfaceState,
    ) -> Result<(), GicV3BackendError> {
        cpu_interface::load(self.capabilities, vcpu, state)
    }

    fn save_cpu_interface(
        &self,
        vcpu: GicVcpuId,
        state: &mut CpuInterfaceState,
    ) -> Result<(), GicV3BackendError> {
        cpu_interface::save(self.capabilities, vcpu, state)
    }

    fn retire_emulated_interrupt(
        &self,
        vcpu: GicVcpuId,
        intid: IntId,
    ) -> Result<(), GicV3BackendError> {
        let Some(key) = timer_ppi_slot(vcpu, intid) else {
            return Ok(());
        };
        let binding = self
            .timer_ppis
            .lock_irqsave()
            .get(key)
            .and_then(Option::as_ref)
            .and_then(Weak::upgrade);
        let Some(binding) = binding else {
            return Ok(());
        };
        binding.retire_local_host_activation()
    }

    fn bind_physical_interrupt(
        &self,
        binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        let intid = self.physical_intid(binding, "bind physical interrupt")?;
        let snapshot = try_with_gic("bind physical interrupt", |gic| {
            if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
                return Some(PhysicalSpiSnapshot {
                    enabled: gic.is_irq_enable(intid),
                    trigger: gic.get_cfg(intid),
                    target: PhysicalSpiRegisterTarget::V2(gic.get_target_cpu(intid)),
                });
            }
            if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
                return Some(PhysicalSpiSnapshot {
                    enabled: gic.is_irq_enable(intid),
                    trigger: gic.get_cfg(intid),
                    target: PhysicalSpiRegisterTarget::V3(gic.get_target_cpu(intid)),
                });
            }
            None
        })?
        .ok_or_else(|| {
            GicV3BackendError::new(
                "bind physical interrupt",
                "the registered interrupt controller is neither GICv2 nor GICv3",
            )
        })?;
        let target = physical_spi_target(self.capabilities.host_version(), binding)?;
        let expected_trigger = match binding.trigger() {
            InterruptTrigger::EdgeTriggered => Trigger::Edge,
            InterruptTrigger::LevelTriggered => Trigger::Level,
        };
        let mut bindings = self.physical_spis.lock_irqsave();
        if bindings[binding.host().raw() as usize].is_some() {
            return Err(GicV3BackendError::value(
                "bind physical interrupt",
                "host INTID is already bound",
                binding.host().raw(),
            ));
        }
        bindings[binding.host().raw() as usize] = Some(snapshot);
        drop(bindings);
        if let Err(error) = configure_physical_interrupt(
            self.capabilities.host_version(),
            intid,
            expected_trigger,
            target,
        ) {
            self.physical_spis.lock_irqsave()[binding.host().raw() as usize].take();
            return Err(error);
        }
        Ok(())
    }

    fn set_physical_interrupt_enabled(
        &self,
        binding: PhysicalInterruptBinding,
        enabled: bool,
    ) -> Result<(), GicV3BackendError> {
        let intid = self.physical_intid(binding, "set physical interrupt enable state")?;
        if self.physical_spis.lock_irqsave()[binding.host().raw() as usize].is_none() {
            return Err(GicV3BackendError::value(
                "set physical interrupt enable state",
                "host INTID is not bound",
                binding.host().raw(),
            ));
        }
        set_physical_enabled(self.capabilities.host_version(), intid, enabled)
    }

    fn complete_physical_interrupt(
        &self,
        vcpu: GicVcpuId,
        binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        if vcpu != binding.target() {
            return Err(GicV3BackendError::mismatch(
                "complete physical interrupt",
                "vCPU does not own the physical activation",
                binding.target().raw() as u64,
                vcpu.raw() as u64,
            ));
        }
        let intid = self.physical_intid(binding, "complete physical interrupt")?;
        physical::complete_assigned_spi(binding.host(), || {
            cpu_interface::deactivate_spi(intid)?;
            instruction_sync_barrier();
            Ok(())
        })?
        .ok_or_else(|| {
            GicV3BackendError::value(
                "complete physical interrupt",
                "host INTID has no active assigned-SPI delivery",
                binding.host().raw(),
            )
        })
    }

    fn deactivate_physical_interrupt(
        &self,
        vcpu: GicVcpuId,
        binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        if vcpu != binding.target() {
            return Err(GicV3BackendError::mismatch(
                "deactivate physical interrupt",
                "vCPU does not own the physical activation",
                binding.target().raw() as u64,
                vcpu.raw() as u64,
            ));
        }
        let intid = self.physical_intid(binding, "deactivate physical interrupt")?;
        let completed = physical::complete_assigned_spi(binding.host(), || {
            cpu_interface::deactivate_spi(intid)?;
            instruction_sync_barrier();
            Ok(())
        })?;
        if completed.is_none() {
            return Err(GicV3BackendError::value(
                "deactivate physical interrupt",
                "host INTID has no active assigned-SPI delivery",
                binding.host().raw(),
            ));
        }
        Ok(())
    }

    fn unbind_physical_interrupt(
        &self,
        binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        let intid = self.physical_intid(binding, "unbind physical interrupt")?;
        let snapshot = self.physical_spis.lock_irqsave()[binding.host().raw() as usize]
            .take()
            .ok_or_else(|| {
                GicV3BackendError::value(
                    "unbind physical interrupt",
                    "host INTID is not bound",
                    binding.host().raw(),
                )
            })?;
        if let Err(error) =
            restore_physical_interrupt(self.capabilities.host_version(), intid, snapshot)
        {
            self.physical_spis.lock_irqsave()[binding.host().raw() as usize] = Some(snapshot);
            return Err(error);
        }
        Ok(())
    }
}

fn configure_physical_interrupt(
    version: HostGicVersion,
    intid: arm_gic_driver::IntId,
    expected_trigger: Trigger,
    expected_target: PhysicalSpiTarget,
) -> Result<(), GicV3BackendError> {
    try_with_gic("configure assigned physical interrupt", |gic| {
        match (version, expected_target) {
            (HostGicVersion::V2, PhysicalSpiTarget::V2(target)) => {
                gic.typed_mut::<arm_gic_driver::v2::Gic>().map(|gic| {
                    gic.set_irq_enable(intid, false);
                    gic.set_cfg(intid, expected_trigger);
                    gic.route_interrupt_to_cpu(intid, target);
                })
            }
            (HostGicVersion::V3, PhysicalSpiTarget::V3(target)) => {
                gic.typed_mut::<arm_gic_driver::v3::Gic>().map(|gic| {
                    gic.set_irq_enable(intid, false);
                    gic.set_cfg(intid, expected_trigger);
                    gic.set_target_cpu(intid, target);
                })
            }
            _ => None,
        }
    })?
    .ok_or_else(|| {
        GicV3BackendError::new(
            "configure assigned physical interrupt",
            "the registered interrupt controller does not match the selected GIC version",
        )
    })
}

fn physical_spi_target(
    version: HostGicVersion,
    binding: PhysicalInterruptBinding,
) -> Result<PhysicalSpiTarget, GicV3BackendError> {
    let affinity = binding.affinity();
    match version {
        HostGicVersion::V2 => {
            let hardware_cpu_id = usize::try_from(affinity.mpidr()).map_err(|_| {
                GicV3BackendError::value(
                    "target assigned physical interrupt",
                    "host CPU affinity does not fit usize",
                    affinity.mpidr(),
                )
            })?;
            let target = try_with_gic("target assigned physical interrupt", |intc| {
                intc.typed_mut::<arm_gic_driver::v2::Gic>()
                    .and_then(|gic| gic.cpu_interface_target_for_hardware_cpu(hardware_cpu_id))
            })?
            .ok_or_else(|| {
                GicV3BackendError::value(
                    "target assigned physical interrupt",
                    "GICv2 host CPU route is not initialized",
                    affinity.mpidr(),
                )
            })?;
            Ok(PhysicalSpiTarget::V2(target))
        }
        HostGicVersion::V3 => Ok(PhysicalSpiTarget::V3(Some(
            arm_gic_driver::v3::Affinity::from_mpidr(affinity.mpidr()),
        ))),
    }
}

fn restore_physical_interrupt(
    version: HostGicVersion,
    intid: arm_gic_driver::IntId,
    snapshot: PhysicalSpiSnapshot,
) -> Result<(), GicV3BackendError> {
    try_with_gic("restore assigned physical interrupt", |gic| {
        match (version, snapshot.target) {
            (HostGicVersion::V2, PhysicalSpiRegisterTarget::V2(target)) => {
                gic.typed_mut::<arm_gic_driver::v2::Gic>().map(|gic| {
                    gic.set_cfg(intid, snapshot.trigger);
                    gic.set_target_cpu(intid, target);
                    gic.set_irq_enable(intid, snapshot.enabled);
                })
            }
            (HostGicVersion::V3, PhysicalSpiRegisterTarget::V3(target)) => {
                gic.typed_mut::<arm_gic_driver::v3::Gic>().map(|gic| {
                    gic.set_cfg(intid, snapshot.trigger);
                    gic.set_target_cpu(intid, target);
                    gic.set_irq_enable(intid, snapshot.enabled);
                })
            }
            _ => None,
        }
    })?
    .ok_or_else(|| {
        GicV3BackendError::new(
            "restore assigned physical interrupt",
            "the registered interrupt controller does not match the selected GIC version",
        )
    })
}

fn set_physical_enabled(
    version: HostGicVersion,
    intid: arm_gic_driver::IntId,
    enabled: bool,
) -> Result<(), GicV3BackendError> {
    try_with_gic("set physical interrupt enable state", |gic| match version {
        HostGicVersion::V2 => gic
            .typed_mut::<arm_gic_driver::v2::Gic>()
            .map(|gic| gic.set_irq_enable(intid, enabled)),
        HostGicVersion::V3 => gic
            .typed_mut::<arm_gic_driver::v3::Gic>()
            .map(|gic| gic.set_irq_enable(intid, enabled)),
    })?
    .ok_or_else(|| {
        GicV3BackendError::new(
            "set physical interrupt enable state",
            "the registered interrupt controller does not match the selected GIC version",
        )
    })
}

fn instruction_sync_barrier() {
    // SAFETY: `isb` only synchronizes preceding GIC register operations on the
    // current CPU and does not access Rust memory.
    unsafe { std::arch::asm!("isb", options(nostack, preserves_flags)) };
}

pub(crate) fn backend(vcpu_count: usize) -> Result<Arc<AxvmVgicBackend>, GicV3BackendError> {
    AxvmVgicBackend::new(vcpu_count).map(Arc::new)
}

fn timer_ppi_slot(vcpu: GicVcpuId, intid: IntId) -> Option<usize> {
    let IntId::Ppi(ppi) = intid else {
        return None;
    };
    vcpu.raw().checked_mul(32)?.checked_add(ppi.raw() as usize)
}

pub(crate) fn host_irq_config() -> Result<ax_cpu::virtualization::HostIrqConfig, GicV3BackendError>
{
    cpu_interface::host_irq_config()
}

pub(crate) fn register_assigned_spi_routes(
    controller: &GicV3Native,
    assigned_spis: &[AssignedSpiConfig],
) -> Result<Arc<AssignedSpiRoutes>, GicV3BackendError> {
    AssignedSpiRoutes::register(controller, assigned_spis)
}

pub(crate) fn host_spi_count() -> Result<usize, GicV3BackendError> {
    let typer = try_with_gic("inspect host SPI capacity", |gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            return Some(gic.typer_raw());
        }
        gic.typed_mut::<arm_gic_driver::v3::Gic>()
            .map(|gic| gic.typer_raw())
    })?
    .ok_or_else(|| {
        GicV3BackendError::new(
            "inspect host SPI capacity",
            "the registered interrupt controller is neither GICv2 nor GICv3",
        )
    })?;
    // GICv2 and GICv3 share the GICD_TYPER.ITLinesNumber encoding. Decode
    // that field directly: `max_intid()` has different meanings in the two
    // host drivers and is not an SPI-count capability.
    GicV3HardwareCapabilities::from_distributor_typer(typer)
        .map(|capabilities| capabilities.spi_count())
        .map_err(|_| {
            GicV3BackendError::value(
                "inspect host SPI capacity",
                "invalid distributor capacity",
                typer as u64,
            )
        })
}

/// Acknowledges one host Group1 IRQ and performs only the priority drop.
///
/// The returned token retains the GICv2 SGI source field when applicable.
/// Physical guest-owned SPIs intentionally remain active until guest DIR.
pub(crate) fn acknowledge_host_irq() -> Option<usize> {
    cpu_interface::acknowledge_host_irq()
        .inspect_err(|error| warn!("{error}"))
        .ok()
        .flatten()
}

/// Completes an IAR acknowledgement captured before the guest timer was
/// stopped by the lower-EL IRQ exit assembly.
pub(crate) fn finish_pending_host_irq(raw_ack: u32) -> Option<usize> {
    cpu_interface::finish_pending_host_irq(raw_ack)
        .inspect_err(|error| warn!("{error}"))
        .ok()
        .flatten()
}

/// Returns the architectural INTID carried by one host acknowledgement token.
pub(crate) const fn host_irq_intid(token: usize) -> u32 {
    (token & 0x00ff_ffff) as u32
}

/// Deactivates a previously acknowledged host IRQ token.
pub(crate) fn deactivate_host_irq(token: usize) {
    if let Err(error) = cpu_interface::deactivate_host_irq(token) {
        warn!("{error}");
    }
}

/// Dispatches an already acknowledged IRQ through the host dynamic framework.
pub(crate) fn dispatch_acknowledged_host_irq(token: usize) {
    let raw = host_irq_intid(token);
    let irq = match ax_std::os::arceos::modules::ax_hal::irq::resolve_percpu_irq(
        ax_std::os::arceos::modules::ax_hal::irq::HwIrq(raw),
    ) {
        Ok(irq) => irq,
        Err(error) => {
            warn!("Cannot resolve acknowledged host IRQ {raw}: {error:?}");
            deactivate_host_irq(token);
            return;
        }
    };
    let outcome = ax_std::os::arceos::modules::ax_hal::irq::handle_acknowledged_irq(irq, || {
        deactivate_host_irq(token);
    });
    if !outcome.handled {
        if outcome.called == 0 {
            warn!("Unhandled acknowledged host IRQ {raw}");
        } else {
            debug!("Spurious acknowledged host IRQ {raw}");
        }
    }
}

/// Routes an acknowledged host IRQ to its assigned VGIC or the host framework.
///
/// Both lower-EL VM exits and current-EL IRQ entries use this function so an
/// assigned physical SPI cannot be consumed by whichever entry path happened
/// to observe it first.
pub(crate) fn route_acknowledged_host_irq(token: usize) -> Result<(), GicV3BackendError> {
    if maintenance::matches_token(token) {
        deactivate_host_irq(token);
        return Ok(());
    }
    physical::route_acknowledged_host_irq(token)
}

pub(crate) fn enable_current_cpu() -> axvm_types::VmBackendResult {
    cpu_interface::initialize_current_cpu().map_err(|error| {
        error!("failed to initialize current host GIC CPU interface: {error:?}");
        axvm_types::VmBackendError::InvalidState
    })?;
    maintenance::enable_current_cpu()
}

pub(crate) fn disable_current_cpu() -> axvm_types::VmBackendResult {
    maintenance::disable_current_cpu()
}
