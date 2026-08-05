//! Mediation for physical MMIO providers shared with a passthrough guest.

use std::{format, string::String, sync::Arc, vec, vec::Vec};

use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceDeclaration, DeviceFactory, DeviceFactoryRegistry,
    DeviceManagerError, DeviceManagerResult, DeviceRegistration, DeviceRequirements,
    ResourceRequest, ResourceSlot,
};
use axdevice_base::{AccessWidth, DeviceError};
use axvm_types::{AddressSpacePolicy, EmulatedDeviceConfig, EmulatedDeviceType};
use rdif_clk::ClockMmioWriteProtection;

use super::shared_mmio::{MmioRegisterAccess, SharedMmioDevice};
use crate::{
    AxVmError, AxVmResult,
    config::AxVMConfig,
    machine::{GuestClockReference, GuestMmioRegion},
};

fn clock_references_for_plan(config: &AxVMConfig) -> Vec<GuestClockReference> {
    if config.address_space_policy() != AddressSpacePolicy::Passthrough {
        return Vec::new();
    }
    config
        .serial_fdt_identity()
        .map(|identity| identity.clock_references.clone())
        .unwrap_or_default()
}

/// Immutable shared-provider mediation selected during AArch64 VM planning.
pub(super) struct SharedProviderBootstrap {
    plans: Arc<[SharedProviderPlan]>,
    configs: Vec<EmulatedDeviceConfig>,
}

impl SharedProviderBootstrap {
    pub(super) fn from_config(config: &AxVMConfig) -> AxVmResult<Self> {
        let references = clock_references_for_plan(config);
        let plans = build_provider_plans(&references)?;
        let configs = plans
            .iter()
            .enumerate()
            .map(|(index, plan)| plan.device_config(index))
            .collect();
        Ok(Self {
            plans: plans.into(),
            configs,
        })
    }

    pub(super) fn configs(&self) -> &[EmulatedDeviceConfig] {
        &self.configs
    }

    pub(super) fn register_factory(&self, registry: &mut DeviceFactoryRegistry) -> AxVmResult {
        if self.plans.is_empty() {
            return Ok(());
        }
        registry.register(Arc::new(SharedProviderFactory {
            plans: self.plans.clone(),
        }))?;
        Ok(())
    }
}

fn build_provider_plans(references: &[GuestClockReference]) -> AxVmResult<Vec<SharedProviderPlan>> {
    let mut plans = Vec::new();
    for reference in references {
        let Some(region) = provider_region(reference)? else {
            continue;
        };
        let clock_id = provider_clock_id(reference)?;
        let protections = provider_protections(reference.provider_phandle, clock_id)?;
        if protections.is_empty() {
            continue;
        }
        for protection in &protections {
            validate_protection(region, *protection)?;
        }
        merge_provider_plan(&mut plans, reference.provider_phandle, region, protections)?;
    }
    Ok(plans)
}

fn provider_region(reference: &GuestClockReference) -> AxVmResult<Option<GuestMmioRegion>> {
    match reference.provider_regions.as_slice() {
        [] => Ok(None),
        [region] => Ok(Some(*region)),
        regions => Err(AxVmError::unsupported(
            "mediate shared clock provider",
            format!(
                "clock provider {:#x} exposes {} MMIO regions; exactly one is supported",
                reference.provider_phandle,
                regions.len()
            ),
        )),
    }
}

fn provider_clock_id(reference: &GuestClockReference) -> AxVmResult<rdif_clk::ClockId> {
    let [selector] = reference.specifier.as_slice() else {
        return Err(AxVmError::unsupported(
            "mediate shared clock provider",
            format!(
                "clock provider {:#x} uses {} selector cells; one is required",
                reference.provider_phandle,
                reference.specifier.len()
            ),
        ));
    };
    Ok(rdif_clk::ClockId::from(*selector as usize))
}

fn provider_protections(
    provider_phandle: u32,
    clock_id: rdif_clk::ClockId,
) -> AxVmResult<Vec<ClockMmioWriteProtection>> {
    let provider_id =
        rdrive::fdt_phandle_to_device_id(provider_phandle.into()).ok_or_else(|| {
            AxVmError::resource_unavailable(
                "clock provider",
                format!("FDT phandle {provider_phandle:#x} is not registered"),
            )
        })?;
    let provider = rdrive::get::<rdif_clk::Clk>(provider_id).map_err(|error| {
        AxVmError::resource_unavailable(
            "clock provider",
            format!("FDT phandle {provider_phandle:#x} has no rdif-clk capability: {error}"),
        )
    })?;
    let clock = provider.lock().map_err(|error| {
        AxVmError::resource_unavailable(
            "clock provider",
            format!("failed to lock FDT phandle {provider_phandle:#x}: {error}"),
        )
    })?;
    clock
        .assignment_mmio_write_protection(clock_id)
        .ok_or_else(|| {
            AxVmError::unsupported(
                "mediate shared clock provider",
                format!(
                    "clock {:#x} on provider {provider_phandle:#x} has no assignment protection",
                    clock_id.raw()
                ),
            )
        })
}

fn validate_protection(
    region: GuestMmioRegion,
    protection: ClockMmioWriteProtection,
) -> AxVmResult {
    let (offset, length) = match protection {
        ClockMmioWriteProtection::Deny { offset, length } => {
            if length == 0 {
                return Err(AxVmError::invalid_config(
                    "shared MMIO deny protection has an empty range",
                ));
            }
            (offset, length)
        }
        ClockMmioWriteProtection::MaskedWrite32 {
            offset,
            value_mask,
            write_enable_mask,
        } => {
            if !offset.is_multiple_of(4) {
                return Err(AxVmError::invalid_config(format!(
                    "shared MMIO masked-write protection offset {offset:#x} is unaligned"
                )));
            }
            if value_mask == 0 || write_enable_mask == 0 || value_mask & write_enable_mask != 0 {
                return Err(AxVmError::invalid_config(
                    "shared MMIO masked-write protection has invalid masks",
                ));
            }
            (offset, 4)
        }
    };
    let end = offset
        .checked_add(length)
        .filter(|end| *end <= region.length)
        .ok_or_else(|| {
            AxVmError::invalid_config(format!(
                "shared MMIO protection {offset:#x}..+{length:#x} exceeds provider range {:#x}",
                region.length
            ))
        })?;
    debug_assert!(end <= region.length);
    Ok(())
}

fn merge_provider_plan(
    plans: &mut Vec<SharedProviderPlan>,
    provider_phandle: u32,
    region: GuestMmioRegion,
    protections: Vec<ClockMmioWriteProtection>,
) -> AxVmResult {
    if let Some(plan) = plans
        .iter_mut()
        .find(|plan| plan.provider_phandle == provider_phandle)
    {
        if plan.region != region {
            return Err(AxVmError::invalid_config(format!(
                "clock provider {provider_phandle:#x} resolved to inconsistent MMIO regions"
            )));
        }
        for protection in protections {
            if !plan.protections.contains(&protection) {
                plan.protections.push(protection);
            }
        }
        return Ok(());
    }

    plans.push(SharedProviderPlan {
        provider_phandle,
        region,
        protections,
    });
    Ok(())
}

struct SharedProviderPlan {
    provider_phandle: u32,
    region: GuestMmioRegion,
    protections: Vec<ClockMmioWriteProtection>,
}

impl SharedProviderPlan {
    fn device_config(&self, index: usize) -> EmulatedDeviceConfig {
        EmulatedDeviceConfig {
            name: format!("shared-clock-provider@{:x}", self.region.base),
            base_gpa: self.region.base,
            length: self.region.length,
            irq_id: 0,
            emu_type: EmulatedDeviceType::SharedMmio,
            cfg_list: vec![index, self.provider_phandle as usize],
        }
    }
}

struct SharedProviderFactory {
    plans: Arc<[SharedProviderPlan]>,
}

impl DeviceFactory for SharedProviderFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::SharedMmio
    }

    fn declare(&self, config: &EmulatedDeviceConfig) -> DeviceManagerResult<DeviceDeclaration> {
        let [index, provider_phandle] = config.cfg_list.as_slice() else {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "declare shared MMIO provider",
                detail: String::from("internal provider fingerprint is malformed"),
            });
        };
        let plan = self
            .plans
            .get(*index)
            .ok_or_else(|| DeviceManagerError::InvalidConfig {
                operation: "declare shared MMIO provider",
                detail: format!("provider plan index {index} is out of range"),
            })?;
        let expected = plan.device_config(*index);
        if config != &expected || *provider_phandle != plan.provider_phandle as usize {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "declare shared MMIO provider",
                detail: format!(
                    "configuration does not match provider {:#x} plan",
                    plan.provider_phandle
                ),
            });
        }
        DeviceRequirements::new()
            .with_mmio(
                ResourceSlot::new("registers")?,
                plan.region.length as u64,
                1,
                ResourceRequest::Fixed(plan.region.base as u64),
            )
            .map(DeviceDeclaration::with_requirements)
    }

    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        context: &mut DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        let [index, provider_phandle] = config.cfg_list.as_slice() else {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build shared MMIO provider",
                detail: String::from("internal provider fingerprint is malformed"),
            });
        };
        let plan = self
            .plans
            .get(*index)
            .ok_or_else(|| DeviceManagerError::InvalidConfig {
                operation: "build shared MMIO provider",
                detail: format!("provider plan index {index} is out of range"),
            })?;
        let expected = plan.device_config(*index);
        if config.emu_type != EmulatedDeviceType::SharedMmio
            || config.name != expected.name
            || config.base_gpa != expected.base_gpa
            || config.length != expected.length
            || *provider_phandle != plan.provider_phandle as usize
        {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build shared MMIO provider",
                detail: format!(
                    "configuration does not match provider {:#x} plan",
                    plan.provider_phandle
                ),
            });
        }

        let (base, length) = context.mmio(&ResourceSlot::new("registers")?)?;
        if base != plan.region.base as u64 || length != plan.region.length as u64 {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build shared MMIO provider",
                detail: format!(
                    "planned range {base:#x}..+{length:#x} differs from provider {:#x}..+{:#x}",
                    plan.region.base, plan.region.length
                ),
            });
        }

        let mapped = axklib::mmio::ioremap(
            mmio_api::MmioAddr::from(plan.region.base),
            plan.region.length,
        )
        .map_err(|error| DeviceManagerError::ResourceNotFound {
            operation: "map shared MMIO provider",
            resource: format!("{:#x}/{:#x}: {error}", plan.region.base, plan.region.length),
        })?;
        let device = Arc::new(SharedMmioDevice::new(
            config.name.clone(),
            plan.region.base,
            plan.region.length,
            plan.protections.clone(),
            Arc::new(MappedMmio { mapped }),
        ));
        Ok(DeviceBundle::from_registration(DeviceRegistration::Device(
            device,
        )))
    }
}

struct MappedMmio {
    mapped: mmio_api::Mmio,
}

impl MmioRegisterAccess for MappedMmio {
    fn read(&self, offset: usize, width: AccessWidth) -> Result<u64, DeviceError> {
        Ok(match width {
            AccessWidth::Byte => u64::from(self.mapped.read::<u8>(offset)),
            AccessWidth::Word => u64::from(self.mapped.read::<u16>(offset)),
            AccessWidth::Dword => u64::from(self.mapped.read::<u32>(offset)),
            AccessWidth::Qword => self.mapped.read::<u64>(offset),
        })
    }

    fn write(&self, offset: usize, width: AccessWidth, value: u64) -> Result<(), DeviceError> {
        match width {
            AccessWidth::Byte => self.mapped.write(offset, value as u8),
            AccessWidth::Word => self.mapped.write(offset, value as u16),
            AccessWidth::Dword => self.mapped.write(offset, value as u32),
            AccessWidth::Qword => self.mapped.write(offset, value),
        }
        Ok(())
    }
}
