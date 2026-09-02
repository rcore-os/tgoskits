//! Immutable AArch64 device, VGIC, and firmware construction plan.

use core::ops::Range;
use std::vec::Vec;

use axdevice::{DeviceFirmwareBinding, DeviceNodeId, DeviceNodeSpec};

use super::{firmware_plan::*, pci_plan::*, shared_provider::*, vgic::*};
use crate::{config::*, machine::*, vm::prepare::device_plan::*, *};

/// Complete AArch64 plan created once before firmware and devices are finalized.
pub(crate) struct Aarch64VmPlan {
    devices: VmDevicePlan,
    firmware: Aarch64FirmwarePlan,
    pci: Option<Aarch64PciPlan>,
}

impl Aarch64VmPlan {
    pub(crate) fn new(config: &AxVMConfig) -> AxVmResult<Self> {
        let vgic = VgicConstructionPlan::new(config)?;
        let profile = config
            .gic_profile()
            .ok_or_else(|| AxVmError::invalid_config("AArch64 machine profile has no VGIC"))?;
        let controller_id = DeviceNodeId::new("vgic")?;
        let mut nodes = std::vec![
            DeviceNodeSpec::host_replacement(
                controller_id.clone(),
                super::vgic::model(config.id(), &vgic),
            )
            .with_firmware_binding(DeviceFirmwareBinding::FdtNode(profile.node_path.clone())),
        ];

        let shared_providers = SharedProviderBootstrap::from_config(config)?;
        nodes.extend(shared_providers.device_nodes()?);
        crate::configured::append_configured_devices(
            config,
            &mut nodes,
            &controller_id,
            vgic.config().controller_id(),
        )?;

        let mut replacement_ranges = gic_ranges(profile)?;
        replacement_ranges.extend(shared_providers.replacement_ranges()?);
        if config
            .serial_firmware_identity()
            .and_then(GuestSerialFirmwareIdentity::fdt)
            .is_some()
        {
            replacement_ranges.push(serial_range(config.serial_profile())?);
        }

        let devices = VmDevicePlan::with_optional_pci_host_for_vm(
            config,
            nodes,
            &replacement_ranges,
            super::resource_pools::create(vgic.config())?,
            provider(&controller_id)?,
        )?;
        let pci = Aarch64PciPlan::resolve(config, devices.graph())?;
        let firmware = Aarch64FirmwarePlan::new(config, vgic.config(), devices.graph())?;
        Ok(Self {
            devices,
            firmware,
            pci,
        })
    }

    pub(crate) const fn gic_profile(&self) -> &GuestGicProfile {
        self.firmware.gic()
    }

    pub(crate) const fn serial_profile(&self) -> GuestSerialProfile {
        self.firmware.serial()
    }

    pub(crate) const fn serial_fdt_identity(&self) -> Option<&GuestSerialFdtIdentity> {
        self.firmware.serial_identity()
    }

    pub(crate) fn serial_devices(&self) -> &[ResolvedSerialDevice] {
        self.firmware.serials()
    }

    pub(crate) fn firmware_devices(&self) -> &[crate::boot::fdt::device::ResolvedFdtDevice] {
        self.firmware.devices()
    }

    pub(crate) const fn timer_profile(&self) -> &GuestTimerProfile {
        self.firmware.timer()
    }

    pub(crate) fn pci_firmware(&self) -> Option<crate::boot::fdt::core::pci::GuestPciHost> {
        self.pci.as_ref().map(Aarch64PciPlan::firmware)
    }
}

impl ArchitectureVmPlan for Aarch64VmPlan {
    fn devices(&self) -> &VmDevicePlan {
        &self.devices
    }
}

fn gic_ranges(profile: &GuestGicProfile) -> AxVmResult<Vec<Range<u64>>> {
    let mut ranges = std::vec![checked_range(profile.distributor, "GIC Distributor")?];
    match &profile.cpu_region {
        GuestGicCpuRegion::CpuInterface(region) => {
            ranges.push(checked_range(*region, "GIC CPU interface")?);
        }
        GuestGicCpuRegion::Redistributors(redistributors) => {
            for region in &redistributors.regions {
                ranges.push(checked_range(*region, "GIC Redistributor")?);
            }
        }
    }
    for its in &profile.its {
        ranges.push(checked_range(its.registers, "GIC ITS")?);
    }
    Ok(ranges)
}

fn serial_range(profile: GuestSerialProfile) -> AxVmResult<Range<u64>> {
    match profile.transport {
        GuestSerialTransport::Mmio { base, length, .. } => {
            checked_range(GuestMmioRegion { base, length }, "serial")
        }
        GuestSerialTransport::Port { .. } => Err(AxVmError::invalid_config(
            "AArch64 serial replacement must use MMIO",
        )),
    }
}

fn checked_range(region: GuestMmioRegion, owner: &'static str) -> AxVmResult<Range<u64>> {
    let base = region.base as u64;
    let length = region.length as u64;
    let end = base
        .checked_add(length)
        .filter(|_| length != 0)
        .ok_or_else(|| AxVmError::invalid_config(std::format!("{owner} range is invalid")))?;
    Ok(base..end)
}
