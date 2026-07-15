//! Unified construction of one VM's GICv3 controller and legacy config views.

use alloc::{sync::Arc, vec::Vec};

use arm_vgic::{
    GicAffinity, GicV3Config, GicV3Controller, GicV3MmioRegion, GicV3Mode, GicVcpuId, GuestMemory,
    GuestMemoryError, SpiId,
};
use axdevice::{
    AxVmDevices, ControllerRole, GicV3DeviceSet, InterruptControllerId, InterruptTopology,
};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType, GuestPhysAddr, VMInterruptMode};

use super::{
    HostSpiForwarding, VcpuRoute, backend, list_register_count, physical_spi_count,
    resolve_physical_irq,
};
use crate::{AxVmError, AxVmResult, config::AxVMConfig, vm::prepare::vcpus::VcpuPlacement};

const PRIMARY_GIC: InterruptControllerId = InterruptControllerId::new(0);
const GICR_FRAME_SIZE: usize = 0x2_0000;

/// GICv3 resources prepared before ordinary interrupt-producing devices.
pub(crate) struct PreparedGicV3 {
    controller: Arc<GicV3Controller>,
    backend: Arc<super::AxvmGicV3Backend>,
    device_set: GicV3DeviceSet,
    ordinary_devices: Vec<EmulatedDeviceConfig>,
}

impl PreparedGicV3 {
    /// Parses legacy GIC frame rows into one validated controller configuration.
    pub(crate) fn from_vm_config(
        vm_id: usize,
        config: &AxVMConfig,
        placements: &[VcpuPlacement],
    ) -> AxVmResult<Option<Self>> {
        let frames = LegacyGicFrames::parse(config.emu_devices())?;
        let Some(mut parsed) = frames.into_config(config.interrupt_mode(), placements)? else {
            return Ok(None);
        };
        if parsed.config.mode() == GicV3Mode::Passthrough {
            let roles = config.arch().interrupt_roles().ok_or_else(|| {
                AxVmError::invalid_config(
                    "AArch64 passthrough interrupt roles were not prepared before GIC creation",
                )
            })?;
            let spi_count = physical_spi_count()
                .map_err(|error| AxVmError::interrupt("inspect physical GICv3", error))?;
            parsed.config = parsed
                .config
                .with_spi_count(spi_count)
                .and_then(|config| {
                    config.with_passthrough_private_interrupts(roles.guest_private_interrupts())
                })
                .map_err(|error| {
                    AxVmError::interrupt("apply physical GICv3 capabilities", error)
                })?;
            debug!(
                "VM[{vm_id}] GICv3 passthrough roles: host-reserved={:?}, guest-timers={:?}, \
                 SPIs={spi_count}",
                roles.host_reserved(),
                roles.guest_timers()
            );
        }
        let passthrough = parsed.config.mode() == GicV3Mode::Passthrough;
        let routes = placements
            .iter()
            .map(|placement| {
                let host_cpu = if passthrough {
                    fixed_host_cpu(placement)?
                } else {
                    placement
                        .phys_cpu_set
                        .and_then(single_host_cpu)
                        .unwrap_or(placement.phys_cpu_id)
                };
                let affinity = GicAffinity::from_mpidr(placement.phys_cpu_id as u64);
                Ok(VcpuRoute::new(placement.id, host_cpu, affinity))
            })
            .collect::<AxVmResult<Vec<_>>>()?;
        let backend = backend(vm_id, routes);
        let controller = match parsed.config.mode() {
            GicV3Mode::Emulated => GicV3Controller::new_with_guest_memory(
                parsed.config,
                backend.clone(),
                Some(Arc::new(VmGuestMemory { vm_id })),
            ),
            GicV3Mode::Passthrough => GicV3Controller::new(parsed.config, backend.clone()),
        }
        .map_err(|error| AxVmError::interrupt("create GICv3 controller", error))?;
        let controller = Arc::new(controller);
        Ok(Some(Self {
            device_set: GicV3DeviceSet::new(controller.clone(), PRIMARY_GIC),
            controller,
            backend,
            ordinary_devices: parsed.ordinary_devices,
        }))
    }

    /// Registers controller capabilities and all configured MMIO frames atomically.
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

    /// Connects every discovered physical SPI according to the controller mode.
    pub(crate) fn connect_physical_spis(
        &self,
        config: &AxVMConfig,
        topology: &InterruptTopology,
    ) -> AxVmResult<Option<HostSpiForwarding>> {
        match self.controller.config().mode() {
            GicV3Mode::Emulated => HostSpiForwarding::connect(
                topology,
                PRIMARY_GIC,
                config.pass_through_spis(),
                self.backend.clone(),
            )
            .map(Some),
            GicV3Mode::Passthrough => {
                self.bind_passthrough_spis(config)?;
                Ok(None)
            }
        }
    }

    fn bind_passthrough_spis(&self, config: &AxVMConfig) -> AxVmResult {
        let target = GicVcpuId::new(0);
        for spi in config.pass_through_spis() {
            let intid = spi
                .checked_add(32)
                .ok_or_else(|| AxVmError::invalid_config("passthrough SPI INTID overflows u32"))?;
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

    pub(crate) fn emulates_interrupts(&self) -> bool {
        self.controller.config().mode() == GicV3Mode::Emulated
    }

    pub(crate) fn ordinary_devices(&self) -> &[EmulatedDeviceConfig] {
        &self.ordinary_devices
    }
}

struct ParsedGicV3Config {
    config: GicV3Config,
    ordinary_devices: Vec<EmulatedDeviceConfig>,
}

#[derive(Default)]
struct LegacyGicFrames {
    distributor: Option<EmulatedDeviceConfig>,
    redistributors: Option<EmulatedDeviceConfig>,
    its: Option<EmulatedDeviceConfig>,
    ordinary_devices: Vec<EmulatedDeviceConfig>,
}

impl LegacyGicFrames {
    fn parse(configs: &[EmulatedDeviceConfig]) -> AxVmResult<Self> {
        let mut frames = Self::default();
        for config in configs {
            match config.emu_type {
                EmulatedDeviceType::InterruptController => {
                    return Err(AxVmError::unsupported(
                        "configure AArch64 interrupt controller",
                        alloc::format!(
                            "device '{}' requests the removed GICv2 interface",
                            config.name
                        ),
                    ));
                }
                EmulatedDeviceType::GPPTDistributor => {
                    set_unique_frame(&mut frames.distributor, config, "Distributor")?;
                }
                EmulatedDeviceType::GPPTRedistributor => {
                    set_unique_frame(&mut frames.redistributors, config, "Redistributor")?;
                }
                EmulatedDeviceType::GPPTITS => {
                    set_unique_frame(&mut frames.its, config, "ITS")?;
                }
                _ => frames.ordinary_devices.push(config.clone()),
            }
        }
        Ok(frames)
    }

    fn into_config(
        self,
        mode: VMInterruptMode,
        placements: &[VcpuPlacement],
    ) -> AxVmResult<Option<ParsedGicV3Config>> {
        let has_any_frame =
            self.distributor.is_some() || self.redistributors.is_some() || self.its.is_some();
        if !has_any_frame {
            if mode == VMInterruptMode::Emulated {
                return Err(AxVmError::invalid_config(
                    "an emulated AArch64 VM requires GICv3 Distributor and Redistributor rows",
                ));
            }
            return Ok(None);
        }
        if mode == VMInterruptMode::NoIrq {
            return Err(AxVmError::unsupported(
                "configure GICv3",
                "interrupt_mode=no_irq cannot register an interrupt controller",
            ));
        }
        let distributor = required_frame(self.distributor, "Distributor")?;
        let redistributors = required_frame(self.redistributors, "Redistributor")?;
        let (redistributor_region, stride) = redistributor_region(&redistributors, placements)?;
        let mut config = GicV3Config::new(
            gic_mode(mode)?,
            mmio_region(&distributor, "Distributor")?,
            redistributor_region,
            stride,
            placements.len(),
        )
        .and_then(|config| config.with_list_register_count(list_register_count()))
        .map_err(|error| AxVmError::interrupt("validate GICv3 configuration", error))?;
        if let Some(its) = &self.its {
            validate_legacy_its_arguments(its)?;
            if mode == VMInterruptMode::Passthrough {
                return Err(AxVmError::unsupported(
                    "configure passthrough GICv3 ITS",
                    "no isolated physical ITS command capability is registered; the guest must \
                     not access the host GITS frame",
                ));
            }
            config = config
                .with_its(mmio_region(its, "ITS")?)
                .map_err(|error| AxVmError::interrupt("validate GICv3 ITS", error))?;
        }
        Ok(Some(ParsedGicV3Config {
            config,
            ordinary_devices: self.ordinary_devices,
        }))
    }
}

fn set_unique_frame(
    slot: &mut Option<EmulatedDeviceConfig>,
    config: &EmulatedDeviceConfig,
    frame: &'static str,
) -> AxVmResult {
    if slot.is_some() {
        return Err(AxVmError::invalid_config(alloc::format!(
            "multiple GICv3 {frame} rows are configured"
        )));
    }
    *slot = Some(config.clone());
    Ok(())
}

fn required_frame(
    frame: Option<EmulatedDeviceConfig>,
    name: &'static str,
) -> AxVmResult<EmulatedDeviceConfig> {
    frame.ok_or_else(|| AxVmError::invalid_config(alloc::format!("GICv3 {name} row is missing")))
}

fn mmio_region(config: &EmulatedDeviceConfig, frame: &'static str) -> AxVmResult<GicV3MmioRegion> {
    GicV3MmioRegion::new(config.base_gpa as u64, config.length as u64).map_err(|error| {
        AxVmError::invalid_config(alloc::format!(
            "GICv3 {frame} device '{}': {error}",
            config.name
        ))
    })
}

fn redistributor_region(
    config: &EmulatedDeviceConfig,
    placements: &[VcpuPlacement],
) -> AxVmResult<(GicV3MmioRegion, u64)> {
    let configured_vcpus = required_argument(config, 0, "vCPU count")?;
    let stride = required_argument(config, 1, "Redistributor stride")?;
    let first_host_cpu = required_argument(config, 2, "first physical CPU")?;
    if configured_vcpus != placements.len() {
        return Err(AxVmError::invalid_config(alloc::format!(
            "GICv3 Redistributor row describes {configured_vcpus} vCPUs, but the VM has {}",
            placements.len()
        )));
    }
    if config.length < GICR_FRAME_SIZE || config.length > stride {
        return Err(AxVmError::invalid_config(alloc::format!(
            "GICv3 Redistributor frame length {:#x} must be in {GICR_FRAME_SIZE:#x}..={stride:#x}",
            config.length
        )));
    }
    for (index, placement) in placements.iter().enumerate() {
        let expected = first_host_cpu.checked_add(index).ok_or_else(|| {
            AxVmError::invalid_config("GICv3 Redistributor physical CPU range overflows")
        })?;
        if placement.phys_cpu_id != expected {
            return Err(AxVmError::invalid_config(alloc::format!(
                "GICv3 Redistributor vCPU {} expects physical CPU {expected}, but placement uses \
                 {}",
                placement.id,
                placement.phys_cpu_id
            )));
        }
    }
    let region_size = stride
        .checked_mul(placements.len())
        .ok_or_else(|| AxVmError::invalid_config("GICv3 Redistributor region size overflows"))?;
    let region = GicV3MmioRegion::new(config.base_gpa as u64, region_size as u64)
        .map_err(AxVmError::invalid_config)?;
    Ok((region, stride as u64))
}

fn fixed_host_cpu(placement: &VcpuPlacement) -> AxVmResult<usize> {
    let mask = placement.phys_cpu_set.ok_or_else(|| {
        AxVmError::invalid_config(alloc::format!(
            "AArch64 passthrough vCPU {} has no fixed physical CPU mask",
            placement.id
        ))
    })?;
    single_host_cpu(mask).ok_or_else(|| {
        AxVmError::invalid_config(alloc::format!(
            "AArch64 passthrough vCPU {} requires one fixed physical CPU, but mask {mask:#x} does \
             not select exactly one CPU",
            placement.id
        ))
    })
}

fn single_host_cpu(mask: usize) -> Option<usize> {
    (mask.count_ones() == 1).then(|| mask.trailing_zeros() as usize)
}

fn required_argument(
    config: &EmulatedDeviceConfig,
    index: usize,
    name: &'static str,
) -> AxVmResult<usize> {
    config.cfg_list.get(index).copied().ok_or_else(|| {
        AxVmError::invalid_config(alloc::format!(
            "GICv3 device '{}' requires {name} in cfg_list[{index}]",
            config.name
        ))
    })
}

fn validate_legacy_its_arguments(config: &EmulatedDeviceConfig) -> AxVmResult {
    if config.cfg_list.len() > 1 {
        return Err(AxVmError::invalid_config(alloc::format!(
            "GICv3 ITS device '{}' accepts at most the legacy host base argument",
            config.name
        )));
    }
    Ok(())
}

fn gic_mode(mode: VMInterruptMode) -> AxVmResult<GicV3Mode> {
    match mode {
        VMInterruptMode::Emulated => Ok(GicV3Mode::Emulated),
        VMInterruptMode::Passthrough => Ok(GicV3Mode::Passthrough),
        VMInterruptMode::NoIrq => Err(AxVmError::unsupported(
            "configure GICv3",
            "interrupt_mode=no_irq has no GICv3 mode",
        )),
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
