//! AArch64 GIC host operations for the ArceOS-backed AxVM runtime.

use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};

use arm_gic_driver::v3::Trigger;
use arm_vgic::{
    CpuInterfaceState, GicV3Backend, GicV3BackendError, GicV3HardwareCapabilities, GicVcpuId,
    HostGicVersion, IntId, PhysicalInterruptBinding, PhysicalIrqId, PpiId, VgicBackendCapabilities,
    VgicCore, VgicError, VgicResult,
};
use ax_std::os::arceos::sync::IrqSafeMutex;
use axdevice_base::InterruptTrigger;

use super::vtimer::Aarch64TimerBinding;

mod cpu_interface;
mod maintenance;
mod physical;

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
    physical_spis: IrqSafeMutex<BTreeMap<PhysicalIrqId, PhysicalSpiSnapshot>>,
    timer_ppis: IrqSafeMutex<BTreeMap<(GicVcpuId, IntId), Weak<Aarch64TimerBinding>>>,
}

impl AxvmVgicBackend {
    /// Discovers immutable host CPU-interface capabilities once.
    pub(crate) fn new() -> Result<Self, GicV3BackendError> {
        Ok(Self {
            capabilities: cpu_interface::capabilities()?,
            physical_spis: IrqSafeMutex::new(BTreeMap::new()),
            timer_ppis: IrqSafeMutex::new(BTreeMap::new()),
        })
    }

    pub(in crate::arch::aarch64) fn register_timer_ppi(
        &self,
        vcpu: GicVcpuId,
        ppi: PpiId,
        binding: Weak<Aarch64TimerBinding>,
    ) -> VgicResult {
        let key = (vcpu, IntId::Ppi(ppi));
        let mut timer_ppis = self.timer_ppis.lock();
        if timer_ppis.get(&key).and_then(Weak::upgrade).is_some() {
            return Err(VgicError::ResourceConflict {
                resource: "host virtual-timer PPI binding",
                detail: std::format!(
                    "vCPU {} INTID {} already has a live binding",
                    vcpu.raw(),
                    ppi.raw()
                ),
            });
        }
        timer_ppis.insert(key, binding);
        Ok(())
    }

    pub(in crate::arch::aarch64) fn unregister_timer_ppi(&self, vcpu: GicVcpuId, ppi: PpiId) {
        self.timer_ppis.lock().remove(&(vcpu, IntId::Ppi(ppi)));
    }

    fn physical_intid(
        &self,
        binding: PhysicalInterruptBinding,
        operation: &'static str,
    ) -> Result<arm_gic_driver::IntId, GicV3BackendError> {
        let raw = u32::try_from(binding.host().raw()).map_err(|_| {
            GicV3BackendError::new(
                operation,
                std::format!("host IRQ {} does not fit a GIC INTID", binding.host().raw()),
            )
        })?;
        if raw != binding.guest().raw() {
            return Err(GicV3BackendError::new(
                operation,
                std::format!(
                    "identity forwarding requires guest INTID {} to equal host INTID {raw}",
                    binding.guest().raw()
                ),
            ));
        }
        arm_gic_driver::checked_intid(raw, 1020).map_err(|_| {
            GicV3BackendError::new(
                operation,
                std::format!("host INTID {raw} is outside the assignable GIC range"),
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
        let binding = self
            .timer_ppis
            .lock()
            .get(&(vcpu, intid))
            .and_then(Weak::upgrade);
        let Some(binding) = binding else {
            return Ok(());
        };
        binding.retire_host_activation().map_err(|error| {
            GicV3BackendError::new("retire host virtual-timer PPI", std::format!("{error}"))
        })
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
        let mut bindings = self.physical_spis.lock();
        if bindings.contains_key(&binding.host()) {
            return Err(GicV3BackendError::new(
                "bind physical interrupt",
                std::format!("host INTID {} is already bound", binding.host().raw()),
            ));
        }
        bindings.insert(binding.host(), snapshot);
        drop(bindings);
        if let Err(error) = configure_physical_interrupt(
            self.capabilities.host_version(),
            intid,
            expected_trigger,
            target,
        ) {
            self.physical_spis.lock().remove(&binding.host());
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
        if !self.physical_spis.lock().contains_key(&binding.host()) {
            return Err(GicV3BackendError::new(
                "set physical interrupt enable state",
                std::format!("host INTID {} is not bound", binding.host().raw()),
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
            return Err(GicV3BackendError::new(
                "complete physical interrupt",
                std::format!(
                    "binding targets vCPU {}, but vCPU {} issued DIR",
                    binding.target().raw(),
                    vcpu.raw()
                ),
            ));
        }
        let intid = self.physical_intid(binding, "complete physical interrupt")?;
        physical::complete_assigned_spi(binding.host(), || {
            cpu_interface::deactivate_spi(intid)?;
            instruction_sync_barrier();
            Ok(())
        })?
        .ok_or_else(|| {
            GicV3BackendError::new(
                "complete physical interrupt",
                std::format!(
                    "host INTID {} has no active assigned-SPI delivery",
                    binding.host().raw()
                ),
            )
        })
    }

    fn deactivate_physical_interrupt(
        &self,
        vcpu: GicVcpuId,
        binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        if vcpu != binding.target() {
            return Err(GicV3BackendError::new(
                "deactivate physical interrupt",
                std::format!(
                    "binding targets vCPU {}, but vCPU {} requested forced deactivation",
                    binding.target().raw(),
                    vcpu.raw()
                ),
            ));
        }
        let intid = self.physical_intid(binding, "deactivate physical interrupt")?;
        let completed = physical::complete_assigned_spi(binding.host(), || {
            cpu_interface::deactivate_spi(intid)?;
            instruction_sync_barrier();
            Ok(())
        })?;
        if completed.is_none() {
            return Err(GicV3BackendError::new(
                "deactivate physical interrupt",
                std::format!(
                    "host INTID {} has no active assigned-SPI delivery",
                    binding.host().raw()
                ),
            ));
        }
        Ok(())
    }

    fn unbind_physical_interrupt(
        &self,
        binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        let intid = self.physical_intid(binding, "unbind physical interrupt")?;
        let snapshot = self
            .physical_spis
            .lock()
            .remove(&binding.host())
            .ok_or_else(|| {
                GicV3BackendError::new(
                    "unbind physical interrupt",
                    std::format!("host INTID {} is not bound", binding.host().raw()),
                )
            })?;
        if let Err(error) =
            restore_physical_interrupt(self.capabilities.host_version(), intid, snapshot)
        {
            self.physical_spis.lock().insert(binding.host(), snapshot);
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
            std::format!("the registered interrupt controller does not match {version:?}"),
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
                GicV3BackendError::new(
                    "target assigned physical interrupt",
                    std::format!("host CPU affinity {affinity:?} does not fit usize"),
                )
            })?;
            let target = try_with_gic("target assigned physical interrupt", |intc| {
                intc.typed_mut::<arm_gic_driver::v2::Gic>()
                    .and_then(|gic| gic.cpu_interface_target_for_hardware_cpu(hardware_cpu_id))
            })?
            .ok_or_else(|| {
                GicV3BackendError::new(
                    "target assigned physical interrupt",
                    std::format!(
                        "GICv2 cannot route host INTID {} to affinity {affinity:?}: the host CPU \
                         route is not initialized",
                        binding.host().raw()
                    ),
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
            std::format!("the registered interrupt controller does not match {version:?}"),
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
            std::format!("the registered interrupt controller does not match {version:?}"),
        )
    })
}

fn instruction_sync_barrier() {
    // SAFETY: `isb` only synchronizes preceding GIC register operations on the
    // current CPU and does not access Rust memory.
    unsafe { std::arch::asm!("isb", options(nostack, preserves_flags)) };
}

pub(crate) fn backend() -> Result<Arc<AxvmVgicBackend>, GicV3BackendError> {
    AxvmVgicBackend::new().map(Arc::new)
}

pub(crate) fn host_irq_config() -> Result<arm_vcpu::ArmHostIrqConfig, GicV3BackendError> {
    cpu_interface::host_irq_config()
}

pub(crate) fn register_assigned_spi_routes(
    controller: &Arc<VgicCore>,
) -> Result<Arc<AssignedSpiRoutes>, GicV3BackendError> {
    AssignedSpiRoutes::register(controller)
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
        .map_err(|error| {
            GicV3BackendError::new("inspect host SPI capacity", std::format!("{error}"))
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
    let outcome = ax_std::os::arceos::modules::ax_hal::irq::dispatch_irq(irq);
    if !outcome.handled {
        if outcome.called == 0 {
            warn!("Unhandled acknowledged host IRQ {raw}");
        } else {
            debug!("Spurious acknowledged host IRQ {raw}");
        }
    }
    deactivate_host_irq(token);
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

pub(crate) fn enable_maintenance_interrupt() -> axvm_types::VmBackendResult {
    maintenance::enable_current_cpu()
}

pub(crate) fn disable_maintenance_interrupt() -> axvm_types::VmBackendResult {
    maintenance::disable_current_cpu()
}
