//! Construction of one VM's GICv3 controller from its immutable machine plan.

use alloc::{sync::Arc, vec::Vec};

use arm_vgic::{
    GicAffinity, GicV3Config, GicV3Controller, GicV3MmioRegion, GicV3Mode, GicVcpuId, GuestMemory,
    GuestMemoryError, SpiId,
};
use axdevice::{
    AxVmDevices, ControllerRole, GicV3DeviceSet, InterruptControllerId, InterruptTopology,
};
use axvm_types::{GuestPhysAddr, InterruptDelivery, VmMachineMode};

use super::{
    HostSpiForwarding, VcpuRoute, backend, list_register_count, physical_capabilities,
    resolve_physical_irq,
};
use crate::{
    AxVmError, AxVmResult,
    config::AxVMConfig,
    machine::{Aarch64GicV3Plan, HostInterruptResource, InterruptControllerPlan},
    vm::prepare::vcpus::VcpuPlacement,
};

const PRIMARY_GIC: InterruptControllerId = InterruptControllerId::new(0);

/// GICv3 resources prepared before interrupt-producing devices.
pub(crate) struct PreparedGicV3 {
    controller: Arc<GicV3Controller>,
    backend: Arc<super::AxvmGicV3Backend>,
    device_set: GicV3DeviceSet,
    assigned_interrupts: Vec<HostInterruptResource>,
}

impl PreparedGicV3 {
    /// Creates the mandatory controller from the architecture profile selected
    /// by [`crate::machine::VmMachinePlanner`].
    pub(crate) fn from_vm_config(
        vm_id: usize,
        config: &AxVMConfig,
        placements: &[VcpuPlacement],
    ) -> AxVmResult<Self> {
        let layout = aarch64_layout(config)?;
        let mode = gic_mode(config.interrupt_delivery());
        let mut gic_config = GicV3Config::new(
            mode,
            mmio_region(layout.distributor(), "Distributor")?,
            mmio_region(layout.redistributors(), "Redistributor")?,
            layout.redistributor_stride(),
            placements.len(),
        )
        .and_then(|config| config.with_list_register_count(list_register_count()))
        .and_then(|config| config.with_spi_count(layout.spi_count() as usize))
        .map_err(|error| AxVmError::interrupt("validate GICv3 configuration", error))?;

        if let Some(its) = layout.its() {
            if mode == GicV3Mode::Passthrough {
                return Err(AxVmError::unsupported(
                    "configure passthrough GICv3 ITS",
                    "no isolated physical ITS command capability is registered",
                ));
            }
            gic_config = gic_config
                .with_its(mmio_region(its, "ITS")?)
                .map_err(|error| AxVmError::interrupt("validate GICv3 ITS", error))?;
        }

        if config.machine_mode() == VmMachineMode::Passthrough {
            let capabilities = physical_capabilities()
                .map_err(|error| AxVmError::interrupt("inspect physical GICv3", error))?;
            gic_config = gic_config
                .with_hardware_capabilities(capabilities)
                .map_err(|error| {
                    AxVmError::interrupt("apply physical GICv3 SPI capability", error)
                })?;
        }
        if mode == GicV3Mode::Passthrough {
            let roles = config.arch().interrupt_roles().ok_or_else(|| {
                AxVmError::invalid_config(
                    "AArch64 passthrough interrupt roles were not prepared before GIC creation",
                )
            })?;
            debug!(
                "VM[{vm_id}] GICv3 passthrough roles: host-reserved={:?}, guest-timer={:?}, \
                 SPIs={}",
                roles.host_reserved(),
                roles.guest_physical_timer(),
                gic_config.spi_count()
            );
        }

        let passthrough = mode == GicV3Mode::Passthrough;
        let routes = placements
            .iter()
            .map(|placement| {
                let host_cpu = if passthrough {
                    placement.fixed_host_cpu()?
                } else {
                    placement
                        .phys_cpu_set
                        .and_then(single_host_cpu)
                        .or_else(|| {
                            super::super::capabilities::logical_cpu_id(placement.phys_cpu_id)
                        })
                        .unwrap_or(placement.id)
                };
                let affinity = GicAffinity::from_mpidr(placement.phys_cpu_id as u64);
                Ok(VcpuRoute::new(placement.id, host_cpu, affinity))
            })
            .collect::<AxVmResult<Vec<_>>>()?;
        let backend = backend(vm_id, routes);
        let controller = match mode {
            GicV3Mode::Emulated => GicV3Controller::new_with_guest_memory(
                gic_config,
                backend.clone(),
                Some(Arc::new(VmGuestMemory { vm_id })),
            ),
            GicV3Mode::Passthrough => GicV3Controller::new(gic_config, backend.clone()),
        }
        .map_err(|error| AxVmError::interrupt("create GICv3 controller", error))?;
        let controller = Arc::new(controller);
        let assigned_interrupts = config
            .machine_plan()
            .assigned_host_interrupts()
            .iter()
            .filter(|interrupt| interrupt.input_u32() >= 32)
            .cloned()
            .collect();
        Ok(Self {
            device_set: GicV3DeviceSet::new(controller.clone(), PRIMARY_GIC),
            controller,
            backend,
            assigned_interrupts,
        })
    }

    /// Registers controller capabilities and all MMIO frames atomically.
    pub(crate) fn register(
        &self,
        devices: &mut AxVmDevices,
        topology: &InterruptTopology,
    ) -> AxVmResult {
        devices
            .register_bundle_with_topology(
                self.device_set.bundle(ControllerRole::Default),
                topology,
            )
            .map_err(Into::into)
    }

    /// Connects every assigned physical SPI according to the controller mode.
    pub(crate) fn connect_physical_spis(
        &self,
        topology: &InterruptTopology,
    ) -> AxVmResult<Option<HostSpiForwarding>> {
        match self.controller.config().mode() {
            GicV3Mode::Emulated => HostSpiForwarding::connect_mediated(
                topology,
                PRIMARY_GIC,
                &self.assigned_interrupts,
                self.backend.clone(),
            )
            .map(Some),
            GicV3Mode::Passthrough => {
                self.bind_passthrough_spis()?;
                HostSpiForwarding::connect_direct(
                    self.controller.clone(),
                    GicVcpuId::new(0),
                    &self.assigned_interrupts,
                    self.backend.clone(),
                )
                .map(Some)
            }
        }
    }

    fn bind_passthrough_spis(&self) -> AxVmResult {
        let target = GicVcpuId::new(0);
        for interrupt in &self.assigned_interrupts {
            let intid = interrupt.input_u32();
            let spi = SpiId::new(intid)
                .map_err(|error| AxVmError::interrupt("validate passthrough SPI", error))?;
            let host = resolve_physical_irq(intid)
                .map_err(|error| AxVmError::interrupt("resolve passthrough SPI", error))?;
            self.controller
                .bind_physical_spi(spi, host, target)
                .map_err(|error| AxVmError::interrupt("bind passthrough SPI", error))?;
        }
        Ok(())
    }

    pub(crate) fn controller(&self) -> Arc<GicV3Controller> {
        self.controller.clone()
    }

    pub(crate) const fn device_set(&self) -> &GicV3DeviceSet {
        &self.device_set
    }
}

fn aarch64_layout(config: &AxVMConfig) -> AxVmResult<&Aarch64GicV3Plan> {
    match config.machine_plan().interrupt_controller() {
        Some(InterruptControllerPlan::Aarch64GicV3(layout)) => Ok(layout),
        Some(_) => Err(AxVmError::invalid_config(
            "AArch64 VM machine plan contains a controller for another architecture",
        )),
        None => Err(AxVmError::invalid_config(
            "AArch64 VM machine plan has no mandatory GICv3 controller",
        )),
    }
}

fn mmio_region(
    range: crate::machine::AddressRange,
    frame: &'static str,
) -> AxVmResult<GicV3MmioRegion> {
    GicV3MmioRegion::new(range.base(), range.size()).map_err(|error| {
        AxVmError::invalid_config(alloc::format!("GICv3 {frame} range is invalid: {error}"))
    })
}

fn single_host_cpu(mask: usize) -> Option<usize> {
    (mask.count_ones() == 1).then(|| mask.trailing_zeros() as usize)
}

fn gic_mode(mode: InterruptDelivery) -> GicV3Mode {
    match mode {
        InterruptDelivery::Mediated => GicV3Mode::Emulated,
        InterruptDelivery::Direct => GicV3Mode::Passthrough,
    }
}

struct VmGuestMemory {
    vm_id: usize,
}

impl GuestMemory for VmGuestMemory {
    fn read(&self, address: u64, destination: &mut [u8]) -> Result<(), GuestMemoryError> {
        let address = usize::try_from(address).map_err(|_| {
            GuestMemoryError::new(
                "read guest memory",
                alloc::format!("guest address {address:#x} does not fit usize"),
            )
        })?;
        let vm = crate::get_vm_by_id(self.vm_id).ok_or_else(|| {
            GuestMemoryError::new(
                "read guest memory",
                alloc::format!("VM {} is not registered", self.vm_id),
            )
        })?;
        vm.read_from_guest(GuestPhysAddr::from_usize(address), destination)
            .map_err(|error| GuestMemoryError::new("read guest memory", alloc::format!("{error}")))
    }
}
