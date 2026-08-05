//! Immutable AArch64 VGIC construction and resource requirements.

use alloc::{sync::Arc, vec::Vec};

use arm_vgic::{
    ArmVgicConfig, AssignedSpiConfig, GicAffinity, GicV3Backend, HostGicVersion, ItsConfig, SpiId,
    VgicMmioRegion, VgicV2Config, VgicV3Config,
};
use axdevice::{
    DeviceBuildContext, DeviceManagerError, DeviceManagerResult, DeviceModel, DeviceModelRegistry,
    DeviceRequirements, ResourceRequest, ResourceSlot,
};
use axdevice_base::{HostIrqId, InterruptControllerId};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use super::super::gic;
use crate::{
    AxVmError, AxVmResult,
    config::AxVMConfig,
    machine::{GuestGicCpuRegion, GuestGicProfile, GuestMmioRegion},
};

const DEFAULT_REDISTRIBUTOR_STRIDE: u64 = 0x2_0000;

/// Immutable controller construction shared by planning and the runtime factory.
pub(crate) struct VgicConstructionPlan {
    config: ArmVgicConfig,
    backend: Arc<gic::AxvmVgicBackend>,
    distributor: EmulatedDeviceConfig,
    host_virtual_timer_intid: u32,
}

impl VgicConstructionPlan {
    pub(crate) fn new(config: &AxVMConfig) -> AxVmResult<Arc<Self>> {
        let configs = config.emu_devices();
        let distributor = unique_config(
            configs,
            EmulatedDeviceType::InterruptController,
            "AArch64 virtual GIC Distributor",
        )?
        .clone();
        let per_cpu = configs
            .iter()
            .filter(|config| config.emu_type == EmulatedDeviceType::GicCpuRegion)
            .collect::<Vec<_>>();
        if per_cpu.is_empty() {
            return Err(AxVmError::resource_unavailable(
                "machine device",
                "AArch64 virtual GIC per-CPU regions",
            ));
        }

        let backend = gic::backend()
            .map_err(|error| AxVmError::interrupt("create host GIC backend", error))?;
        let vgic_config = build_vgic_config(config, &distributor, &per_cpu, backend.clone())?;
        let host_virtual_timer_intid = config
            .timer_profile()
            .ok_or_else(|| {
                AxVmError::invalid_config("AArch64 machine profile has no architectural timer")
            })?
            .virtual_intid;
        Ok(Arc::new(Self {
            config: vgic_config,
            backend,
            distributor,
            host_virtual_timer_intid,
        }))
    }

    pub(crate) fn register_model(self: &Arc<Self>, models: &mut DeviceModelRegistry) -> AxVmResult {
        models.register(Arc::new(Aarch64VgicDeviceModel { plan: self.clone() }))?;
        Ok(())
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

    pub(super) fn validate_and_consume(
        &self,
        config: &EmulatedDeviceConfig,
        context: &mut DeviceBuildContext<'_>,
    ) -> DeviceManagerResult {
        axdevice::validate_device_config(&self.distributor, config, "build AArch64 virtual GIC")?;
        consume_mmio(
            context,
            &registers_slot()?,
            self.distributor.base_gpa as u64,
            self.distributor.length as u64,
            "build AArch64 virtual GIC",
        )?;
        if let ArmVgicConfig::V3(v3) = &self.config {
            for its in v3.its() {
                let region = its.registers();
                consume_mmio(
                    context,
                    &its_slot(its.id())?,
                    region.base(),
                    region.size(),
                    "build AArch64 virtual ITS",
                )?;
            }
        }
        Ok(())
    }
}

struct Aarch64VgicDeviceModel {
    plan: Arc<VgicConstructionPlan>,
}

impl DeviceModel for Aarch64VgicDeviceModel {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::InterruptController
    }

    fn requirements(
        &self,
        config: &EmulatedDeviceConfig,
    ) -> DeviceManagerResult<DeviceRequirements> {
        axdevice::validate_device_config(
            &self.plan.distributor,
            config,
            "declare AArch64 virtual GIC resources",
        )?;
        let mut requirements = DeviceRequirements::new().with_mmio(
            registers_slot()?,
            config.length as u64,
            1,
            ResourceRequest::Fixed(config.base_gpa as u64),
        )?;
        if let ArmVgicConfig::V3(v3) = self.plan.config() {
            for its in v3.its() {
                let region = its.registers();
                requirements = requirements.with_mmio(
                    its_slot(its.id())?,
                    region.size(),
                    1,
                    ResourceRequest::Fixed(region.base()),
                )?;
            }
        }
        Ok(requirements)
    }
}

fn build_vgic_config(
    config: &AxVMConfig,
    distributor: &EmulatedDeviceConfig,
    per_cpu: &[&EmulatedDeviceConfig],
    backend: Arc<gic::AxvmVgicBackend>,
) -> AxVmResult<ArmVgicConfig> {
    let capabilities = backend.capabilities();
    let profile = config.gic_profile();
    let guest_version = match profile.map(|profile| &profile.cpu_region) {
        Some(GuestGicCpuRegion::CpuInterface(_)) => HostGicVersion::V2,
        Some(GuestGicCpuRegion::Redistributors(_)) | None => HostGicVersion::V3,
    };
    if capabilities.host_version() != guest_version {
        return Err(AxVmError::unsupported(
            "create AArch64 virtual GIC",
            alloc::format!(
                "machine profile requires {guest_version:?}, but the host CPU interface is {:?}",
                capabilities.host_version()
            ),
        ));
    }

    let placements = config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids();
    let affinities = placements
        .iter()
        .map(|(_, _, physical_id)| GicAffinity::from_mpidr(*physical_id as u64))
        .collect();
    let assigned_spis = assigned_spis(config.pass_through_irqs())?;
    let distributor_region = vgic_region(
        GuestMmioRegion {
            base: distributor.base_gpa,
            length: distributor.length,
        },
        "validate GIC Distributor range",
    )?;
    let spi_count = gic::host_spi_count()
        .map_err(|error| AxVmError::interrupt("inspect host GIC SPI capacity", error))?;
    let controller_id = InterruptControllerId::new(0);

    match guest_version {
        HostGicVersion::V2 => {
            let cpu_interface = cpu_interface_region(profile, per_cpu)?;
            VgicV2Config::new(controller_id, distributor_region, cpu_interface, affinities)
                .and_then(|config| config.with_spi_count(spi_count))
                .and_then(|config| {
                    config.with_list_register_count(capabilities.list_register_count())
                })
                .and_then(|config| config.with_priority_bits(capabilities.priority_bits()))
                .and_then(|config| config.with_assigned_spis(assigned_spis))
                .map(ArmVgicConfig::V2)
                .map_err(|error| AxVmError::interrupt("construct AArch64 virtual GICv2", error))
        }
        HostGicVersion::V3 => {
            let (redistributors, stride, its) = redistributor_regions(profile, per_cpu)?;
            VgicV3Config::new(
                controller_id,
                distributor_region,
                redistributors,
                stride,
                affinities,
            )
            .and_then(|config| config.with_spi_count(spi_count))
            .and_then(|config| config.with_list_register_count(capabilities.list_register_count()))
            .and_then(|config| config.with_priority_bits(capabilities.priority_bits()))
            .and_then(|config| config.with_its(its))
            .and_then(|config| config.with_assigned_spis(assigned_spis))
            .map(ArmVgicConfig::V3)
            .map_err(|error| AxVmError::interrupt("construct AArch64 virtual GICv3", error))
        }
    }
}

fn cpu_interface_region(
    profile: Option<&GuestGicProfile>,
    descriptors: &[&EmulatedDeviceConfig],
) -> AxVmResult<VgicMmioRegion> {
    let [descriptor] = descriptors else {
        return Err(AxVmError::invalid_config(
            "AArch64 GICv2 requires exactly one CPU-interface descriptor",
        ));
    };
    if !descriptor.cfg_list.is_empty() {
        return Err(AxVmError::invalid_config(
            "AArch64 GICv2 CPU-interface descriptor must not carry Redistributor metadata",
        ));
    }
    let region = match profile.map(|profile| &profile.cpu_region) {
        Some(GuestGicCpuRegion::CpuInterface(region)) => *region,
        _ => GuestMmioRegion {
            base: descriptor.base_gpa,
            length: descriptor.length,
        },
    };
    ensure_descriptor_matches(descriptor, region)?;
    vgic_region(region, "validate GIC CPU-interface range")
}

fn redistributor_regions(
    profile: Option<&GuestGicProfile>,
    descriptors: &[&EmulatedDeviceConfig],
) -> AxVmResult<(Vec<VgicMmioRegion>, u64, Vec<ItsConfig>)> {
    let (regions, stride, its_profiles) = match profile.map(|profile| &profile.cpu_region) {
        Some(GuestGicCpuRegion::Redistributors(redistributors)) => (
            redistributors.regions.as_slice(),
            redistributors.stride as u64,
            profile.map_or(&[][..], |profile| profile.its.as_slice()),
        ),
        _ => {
            let fallback = descriptors
                .iter()
                .map(|descriptor| GuestMmioRegion {
                    base: descriptor.base_gpa,
                    length: descriptor.length,
                })
                .collect::<Vec<_>>();
            return build_redistributor_result(
                &fallback,
                DEFAULT_REDISTRIBUTOR_STRIDE,
                &[],
                descriptors,
            );
        }
    };
    build_redistributor_result(regions, stride, its_profiles, descriptors)
}

fn build_redistributor_result(
    regions: &[GuestMmioRegion],
    stride: u64,
    its_profiles: &[crate::machine::GuestItsProfile],
    descriptors: &[&EmulatedDeviceConfig],
) -> AxVmResult<(Vec<VgicMmioRegion>, u64, Vec<ItsConfig>)> {
    if regions.len() != descriptors.len() {
        return Err(AxVmError::invalid_config(alloc::format!(
            "AArch64 GIC profile has {} Redistributor regions but {} descriptors",
            regions.len(),
            descriptors.len()
        )));
    }
    let mut resolved = Vec::with_capacity(regions.len());
    for (descriptor, region) in descriptors.iter().zip(regions) {
        ensure_descriptor_matches(descriptor, *region)?;
        resolved.push(vgic_region(*region, "validate GIC Redistributor range")?);
    }
    let its = its_profiles
        .iter()
        .map(|profile| {
            vgic_region(profile.registers, "validate ITS range")
                .map(|registers| ItsConfig::new(profile.id, registers))
        })
        .collect::<AxVmResult<Vec<_>>>()?;
    Ok((resolved, stride, its))
}

fn ensure_descriptor_matches(
    descriptor: &EmulatedDeviceConfig,
    region: GuestMmioRegion,
) -> AxVmResult {
    if descriptor.base_gpa != region.base || descriptor.length != region.length {
        return Err(AxVmError::invalid_config(alloc::format!(
            "GIC descriptor {} at {:#x}..+{:#x} differs from firmware plan {:#x}..+{:#x}",
            descriptor.name,
            descriptor.base_gpa,
            descriptor.length,
            region.base,
            region.length
        )));
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

fn unique_config<'a>(
    configs: &'a [EmulatedDeviceConfig],
    device_type: EmulatedDeviceType,
    resource: &'static str,
) -> AxVmResult<&'a EmulatedDeviceConfig> {
    let mut matches = configs
        .iter()
        .filter(|config| config.emu_type == device_type);
    let config = matches
        .next()
        .ok_or_else(|| AxVmError::resource_unavailable("machine device", resource))?;
    if matches.next().is_some() {
        return Err(AxVmError::resource_conflict(
            "machine device",
            alloc::format!("more than one {resource} descriptor is configured"),
        ));
    }
    Ok(config)
}

fn registers_slot() -> DeviceManagerResult<ResourceSlot> {
    ResourceSlot::new("registers")
}

fn its_slot(id: axdevice_base::ItsId) -> DeviceManagerResult<ResourceSlot> {
    ResourceSlot::new(alloc::format!("its-{}", id.value()))
}

fn consume_mmio(
    context: &mut DeviceBuildContext<'_>,
    slot: &ResourceSlot,
    expected_base: u64,
    expected_length: u64,
    operation: &'static str,
) -> DeviceManagerResult {
    let (base, length) = context.mmio(slot)?;
    if (base, length) != (expected_base, expected_length) {
        return Err(DeviceManagerError::InvalidConfig {
            operation,
            detail: alloc::format!(
                "planned MMIO range {base:#x}..+{length:#x} differs from \
                 {expected_base:#x}..+{expected_length:#x}"
            ),
        });
    }
    Ok(())
}
