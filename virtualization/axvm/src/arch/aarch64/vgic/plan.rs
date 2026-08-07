//! Immutable AArch64 VGIC construction and resource requirements.

use std::{sync::Arc, vec::Vec};

use arm_vgic::*;
use axdevice::*;
use axdevice_base::{HostIrqId, InterruptControllerId};

use super::super::gic;
use crate::{config::*, machine::*, *};

/// Immutable controller construction shared by resource planning and runtime build.
pub(crate) struct VgicConstructionPlan {
    config: ArmVgicConfig,
    backend: Arc<gic::AxvmVgicBackend>,
    host_virtual_timer_intid: u32,
}

impl VgicConstructionPlan {
    pub(crate) fn new(config: &AxVMConfig) -> AxVmResult<Arc<Self>> {
        let profile = config
            .gic_profile()
            .ok_or_else(|| AxVmError::invalid_config("AArch64 machine profile has no VGIC"))?;
        let backend = gic::backend()
            .map_err(|error| AxVmError::interrupt("create host GIC backend", error))?;
        let vgic_config = build_vgic_config(config, profile, backend.clone())?;
        let host_virtual_timer_intid = config
            .timer_profile()
            .ok_or_else(|| {
                AxVmError::invalid_config("AArch64 machine profile has no architectural timer")
            })?
            .virtual_intid;
        Ok(Arc::new(Self {
            config: vgic_config,
            backend,
            host_virtual_timer_intid,
        }))
    }

    pub(crate) const fn config(&self) -> &ArmVgicConfig {
        &self.config
    }

    pub(super) fn backend(&self) -> Arc<gic::AxvmVgicBackend> {
        self.backend.clone()
    }

    pub(super) const fn host_virtual_timer_intid(&self) -> u32 {
        self.host_virtual_timer_intid
    }

    pub(super) fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        let mut requirements = DeviceRequirements::new();
        match self.config() {
            ArmVgicConfig::V2(config) => {
                requirements = add_region(requirements, registers_slot()?, config.distributor())?;
                requirements =
                    add_region(requirements, cpu_region_slot(0)?, config.cpu_interface())?;
            }
            ArmVgicConfig::V3(config) => {
                requirements = add_region(requirements, registers_slot()?, config.distributor())?;
                for (index, region) in config.redistributors().iter().copied().enumerate() {
                    requirements = add_region(requirements, cpu_region_slot(index)?, region)?;
                }
                for its in config.its() {
                    requirements = add_region(requirements, its_slot(its.id())?, its.registers())?;
                }
            }
        }
        Ok(requirements)
    }

    pub(super) fn validate_and_consume(
        &self,
        context: &mut DeviceBuildContext<'_>,
    ) -> DeviceManagerResult {
        match self.config() {
            ArmVgicConfig::V2(config) => {
                consume_region(context, &registers_slot()?, config.distributor())?;
                consume_region(context, &cpu_region_slot(0)?, config.cpu_interface())?;
            }
            ArmVgicConfig::V3(config) => {
                consume_region(context, &registers_slot()?, config.distributor())?;
                for (index, region) in config.redistributors().iter().copied().enumerate() {
                    consume_region(context, &cpu_region_slot(index)?, region)?;
                }
                for its in config.its() {
                    consume_region(context, &its_slot(its.id())?, its.registers())?;
                }
            }
        }
        Ok(())
    }
}

fn build_vgic_config(
    config: &AxVMConfig,
    profile: &GuestGicProfile,
    backend: Arc<gic::AxvmVgicBackend>,
) -> AxVmResult<ArmVgicConfig> {
    let capabilities = backend.capabilities();
    let guest_version = match &profile.cpu_region {
        GuestGicCpuRegion::CpuInterface(_) => HostGicVersion::V2,
        GuestGicCpuRegion::Redistributors(_) => HostGicVersion::V3,
    };
    if capabilities.host_version() != guest_version {
        return Err(AxVmError::unsupported(
            "create AArch64 virtual GIC",
            std::format!(
                "guest firmware requires {guest_version:?}, but the host CPU interface is {:?}",
                capabilities.host_version()
            ),
        ));
    }

    let affinities = config
        .phys_cpu_ls
        .get_vcpu_affinities_pcpu_ids()
        .iter()
        .map(|(_, _, physical_id)| GicAffinity::from_mpidr(*physical_id as u64))
        .collect();
    let distributor = vgic_region(profile.distributor, "validate GIC Distributor range")?;
    let assigned_spis = assigned_spis(config.pass_through_irqs())?;
    let spi_count = gic::host_spi_count()
        .map_err(|error| AxVmError::interrupt("inspect host GIC SPI capacity", error))?;
    let controller = InterruptControllerId::new(0);

    match &profile.cpu_region {
        GuestGicCpuRegion::CpuInterface(region) => VgicV2Config::new(
            controller,
            distributor,
            vgic_region(*region, "validate GIC CPU-interface range")?,
            affinities,
        )
        .and_then(|value| value.with_spi_count(spi_count))
        .and_then(|value| value.with_list_register_count(capabilities.list_register_count()))
        .and_then(|value| value.with_priority_bits(capabilities.priority_bits()))
        .and_then(|value| value.with_assigned_spis(assigned_spis))
        .map(ArmVgicConfig::V2)
        .map_err(|error| AxVmError::interrupt("construct AArch64 virtual GICv2", error)),
        GuestGicCpuRegion::Redistributors(redistributors) => {
            let regions = redistributors
                .regions
                .iter()
                .copied()
                .map(|region| vgic_region(region, "validate GIC Redistributor range"))
                .collect::<AxVmResult<Vec<_>>>()?;
            let its = profile
                .its
                .iter()
                .map(|profile| {
                    vgic_region(profile.registers, "validate ITS range")
                        .map(|registers| ItsConfig::new(profile.id, registers))
                })
                .collect::<AxVmResult<Vec<_>>>()?;
            VgicV3Config::new(
                controller,
                distributor,
                regions,
                redistributors.stride as u64,
                affinities,
            )
            .and_then(|value| value.with_spi_count(spi_count))
            .and_then(|value| value.with_list_register_count(capabilities.list_register_count()))
            .and_then(|value| value.with_priority_bits(capabilities.priority_bits()))
            .and_then(|value| value.with_its(its))
            .and_then(|value| value.with_assigned_spis(assigned_spis))
            .map(ArmVgicConfig::V3)
            .map_err(|error| AxVmError::interrupt("construct AArch64 virtual GICv3", error))
        }
    }
}

fn add_region(
    requirements: DeviceRequirements,
    slot: ResourceSlot,
    region: VgicMmioRegion,
) -> DeviceManagerResult<DeviceRequirements> {
    requirements.with_mmio(
        slot,
        region.size(),
        1,
        ResourceRequest::Fixed(region.base()),
    )
}

fn consume_region(
    context: &mut DeviceBuildContext<'_>,
    slot: &ResourceSlot,
    expected: VgicMmioRegion,
) -> DeviceManagerResult {
    let (base, size) = context.mmio(slot)?;
    if (base, size) != (expected.base(), expected.size()) {
        return Err(DeviceManagerError::InvalidConfig {
            operation: "build AArch64 virtual GIC",
            detail: std::format!(
                "planned MMIO range {base:#x}..+{size:#x} differs from {:#x}..+{:#x}",
                expected.base(),
                expected.size()
            ),
        });
    }
    Ok(())
}

fn vgic_region(region: GuestMmioRegion, operation: &'static str) -> AxVmResult<VgicMmioRegion> {
    VgicMmioRegion::new(region.base as u64, region.length as u64)
        .map_err(|error| AxVmError::interrupt(operation, error))
}

fn assigned_spis(
    configured: &[crate::config::PassthroughInterrupt],
) -> AxVmResult<Vec<AssignedSpiConfig>> {
    configured
        .iter()
        .map(|route| {
            let intid = route.source.checked_add(32).ok_or_else(|| {
                AxVmError::invalid_config("AArch64 passthrough SPI number overflows")
            })?;
            AssignedSpiConfig::new(
                SpiId::new(intid)
                    .map_err(|error| AxVmError::interrupt("validate assigned SPI", error))?,
                HostIrqId::new(intid as usize),
                0,
                route.trigger,
            )
            .map_err(|error| AxVmError::interrupt("plan assigned physical SPI", error))
        })
        .collect()
}

fn registers_slot() -> DeviceManagerResult<ResourceSlot> {
    ResourceSlot::new("distributor")
}

fn cpu_region_slot(index: usize) -> DeviceManagerResult<ResourceSlot> {
    ResourceSlot::new(std::format!("cpu-region-{index}"))
}

fn its_slot(id: axdevice_base::ItsId) -> DeviceManagerResult<ResourceSlot> {
    ResourceSlot::new(std::format!("its-{}", id.value()))
}
