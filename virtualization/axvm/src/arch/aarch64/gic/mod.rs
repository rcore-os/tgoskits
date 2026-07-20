//! AArch64 platform backend for the VM-local GICv3 model.

use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};

use arm_vgic::{
    CpuInterfaceState, GicAffinity, GicV3Backend, GicV3BackendError, GicVcpuId, IntId,
    PhysicalInterruptBinding, PhysicalIrqId, PhysicalMsiBinding, SpiId,
};
use ax_kspin::SpinRaw;

mod cpu_interface;
mod forwarding;
mod maintenance;
mod physical_gic;
mod physical_its;
mod physical_spi;
mod registration;
mod roles;

pub(crate) use forwarding::HostSpiForwarding;
pub(crate) use maintenance::{
    HostMaintenanceInterrupt, register as register_maintenance_interrupt,
};
pub(crate) use registration::PreparedGicV3;
pub(crate) use roles::{Aarch64InterruptDiscovery, Aarch64InterruptRoles};

/// Fixed host placement of one VM-local vCPU.
#[derive(Clone, Copy, Debug)]
pub(crate) struct VcpuRoute {
    vcpu: GicVcpuId,
    host_cpu: usize,
    affinity: GicAffinity,
}

impl VcpuRoute {
    pub(crate) const fn new(vcpu: usize, host_cpu: usize, affinity: GicAffinity) -> Self {
        Self {
            vcpu: GicVcpuId::new(vcpu),
            host_cpu,
            affinity,
        }
    }
}

/// Checked bridge from `arm_vgic` capabilities to the current Arm host.
pub(crate) struct AxvmGicV3Backend {
    vm_id: usize,
    routes: BTreeMap<GicVcpuId, VcpuRoute>,
    emulated_spis: SpinRaw<BTreeMap<SpiId, Weak<forwarding::ForwardedSpi>>>,
    hardware_backed_spis: SpinRaw<BTreeMap<SpiId, Weak<forwarding::ForwardedSpi>>>,
}

impl AxvmGicV3Backend {
    pub(crate) fn new(vm_id: usize, routes: impl IntoIterator<Item = VcpuRoute>) -> Self {
        Self {
            vm_id,
            routes: routes
                .into_iter()
                .map(|route| (route.vcpu, route))
                .collect(),
            emulated_spis: SpinRaw::new(BTreeMap::new()),
            hardware_backed_spis: SpinRaw::new(BTreeMap::new()),
        }
    }

    fn route(&self, vcpu: GicVcpuId) -> Result<VcpuRoute, GicV3BackendError> {
        self.routes.get(&vcpu).copied().ok_or_else(|| {
            GicV3BackendError::new(
                "resolve vCPU route",
                alloc::format!("vCPU {} has no fixed host route", vcpu.raw()),
            )
        })
    }

    fn boot_vcpu(&self) -> Result<GicVcpuId, GicV3BackendError> {
        self.routes.keys().next().copied().ok_or_else(|| {
            GicV3BackendError::new(
                "resolve boot vCPU route",
                "the GICv3 backend has no vCPU routes",
            )
        })
    }

    fn register_emulated_spi(
        &self,
        spi: SpiId,
        forwarding: Weak<forwarding::ForwardedSpi>,
    ) -> Result<(), GicV3BackendError> {
        let mut spis = self.emulated_spis.lock();
        if spis.get(&spi).and_then(Weak::upgrade).is_some() {
            return Err(GicV3BackendError::new(
                "register emulated SPI forwarding",
                alloc::format!("guest SPI {} is already forwarded", spi.raw()),
            ));
        }
        spis.insert(spi, forwarding);
        Ok(())
    }

    fn unregister_emulated_spi(&self, spi: SpiId) {
        self.emulated_spis.lock().remove(&spi);
    }

    fn register_hardware_backed_spi(
        &self,
        spi: SpiId,
        forwarding: Weak<forwarding::ForwardedSpi>,
    ) -> Result<(), GicV3BackendError> {
        let mut spis = self.hardware_backed_spis.lock();
        if spis.get(&spi).and_then(Weak::upgrade).is_some() {
            return Err(GicV3BackendError::new(
                "register hardware-backed SPI forwarding",
                alloc::format!("guest SPI {} is already forwarded", spi.raw()),
            ));
        }
        spis.insert(spi, forwarding);
        Ok(())
    }

    fn hardware_backed_spi(&self, spi: SpiId) -> Option<Arc<forwarding::ForwardedSpi>> {
        self.hardware_backed_spis
            .lock()
            .get(&spi)
            .and_then(Weak::upgrade)
    }

    fn unregister_hardware_backed_spi(&self, spi: SpiId) {
        self.hardware_backed_spis.lock().remove(&spi);
    }
}

impl GicV3Backend for AxvmGicV3Backend {
    fn load_cpu_interface(
        &self,
        vcpu: GicVcpuId,
        state: &CpuInterfaceState,
    ) -> Result<(), GicV3BackendError> {
        cpu_interface::load(vcpu, state)
    }

    fn save_cpu_interface(
        &self,
        vcpu: GicVcpuId,
        state: &mut CpuInterfaceState,
    ) -> Result<(), GicV3BackendError> {
        cpu_interface::save(vcpu, state)
    }

    fn retire_emulated_interrupt(
        &self,
        _vcpu: GicVcpuId,
        intid: IntId,
    ) -> Result<(), GicV3BackendError> {
        let IntId::Spi(spi) = intid else {
            return Ok(());
        };
        let forwarding = {
            let spis = self.emulated_spis.lock();
            spis.get(&spi).and_then(Weak::upgrade)
        };
        if let Some(forwarding) = forwarding {
            forwarding.retire_guest_interrupt()?;
        }
        Ok(())
    }

    fn deactivate_physical_interrupt(
        &self,
        vcpu: GicVcpuId,
        binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        physical_spi::deactivate(self, vcpu, binding)
    }

    fn bind_physical_interrupt(
        &self,
        binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        physical_spi::bind(self, binding)
    }

    fn set_physical_interrupt_enabled(
        &self,
        binding: PhysicalInterruptBinding,
        enabled: bool,
    ) -> Result<(), GicV3BackendError> {
        physical_spi::prepare_enabled(self, binding, enabled)?;
        let IntId::Spi(spi) = binding.guest() else {
            return Err(GicV3BackendError::new(
                "set hardware-backed physical interrupt enable state",
                alloc::format!("guest interrupt {:?} is not an SPI", binding.guest()),
            ));
        };
        let forwarding = self.hardware_backed_spi(spi).ok_or_else(|| {
            GicV3BackendError::new(
                "set hardware-backed physical interrupt enable state",
                alloc::format!("guest SPI {} has no host forwarding action", spi.raw()),
            )
        })?;
        forwarding.set_hardware_backed_enabled(enabled)
    }

    fn bind_physical_msi(&self, binding: PhysicalMsiBinding) -> Result<(), GicV3BackendError> {
        physical_its::bind_msi(self, binding)
    }

    fn signal_physical_msi(&self, binding: PhysicalMsiBinding) -> Result<(), GicV3BackendError> {
        physical_its::signal_msi(self, binding)
    }

    fn unbind_physical_interrupt(
        &self,
        binding: PhysicalInterruptBinding,
    ) -> Result<(), GicV3BackendError> {
        physical_spi::unbind(self, binding)
    }

    fn unbind_physical_msi(&self, binding: PhysicalMsiBinding) -> Result<(), GicV3BackendError> {
        physical_its::unbind_msi(self, binding)
    }
}

pub(crate) fn backend(
    vm_id: usize,
    routes: impl IntoIterator<Item = VcpuRoute>,
) -> Arc<AxvmGicV3Backend> {
    Arc::new(AxvmGicV3Backend::new(vm_id, routes))
}

pub(crate) fn list_register_count() -> usize {
    cpu_interface::hardware_list_register_count()
}

pub(crate) fn require_deactivation_trap() -> Result<(), GicV3BackendError> {
    cpu_interface::require_deactivation_trap()
}

pub(crate) fn resolve_physical_irq(intid: u32) -> Result<PhysicalIrqId, GicV3BackendError> {
    physical_spi::resolve(intid)
}

pub(crate) fn physical_capabilities()
-> Result<arm_vgic::GicV3HardwareCapabilities, GicV3BackendError> {
    physical_gic::physical_capabilities()
}

pub(crate) fn handle_current_irq() -> bool {
    ax_std::os::arceos::modules::ax_hal::irq::handle_irq(0)
}
