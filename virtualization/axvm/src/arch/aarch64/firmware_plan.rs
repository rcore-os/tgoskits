//! Guest firmware identity frozen from the same VGIC construction plan.

use arm_vgic::{ArmVgicConfig, VgicMmioRegion};

use crate::{config::*, machine::*, *};

pub(super) struct Aarch64FirmwarePlan {
    gic: GuestGicProfile,
    console: GuestSerialProfile,
    serials: std::vec::Vec<ResolvedSerialDevice>,
    serial_identity: Option<GuestSerialFdtIdentity>,
    ivc_channels: std::vec::Vec<GuestIvcChannel>,
    timer: GuestTimerProfile,
}

impl Aarch64FirmwarePlan {
    pub(super) fn new(
        config: &AxVMConfig,
        vgic: &ArmVgicConfig,
        graph: &axdevice::ResolvedDeviceGraph,
    ) -> AxVmResult<Self> {
        let gic = match config.gic_profile() {
            Some(profile) => profile.clone(),
            None => fallback_gic_profile(vgic)?,
        };
        let timer = config.timer_profile().cloned().ok_or_else(|| {
            AxVmError::invalid_config("AArch64 machine profile has no architectural timer")
        })?;
        let serials = resolved_serial_devices(graph)?;
        let ivc_channels = resolved_ivc_channels(graph)?;
        let console = serials
            .iter()
            .find(|serial| serial.id() == "console0")
            .ok_or_else(|| AxVmError::invalid_config("AArch64 plan has no console0 serial node"))?;
        let console_profile = console.profile();
        let serial_identity = match console.firmware_binding() {
            axdevice::DeviceFirmwareBinding::FdtNode(path) => config
                .serial_firmware_identity()
                .and_then(GuestSerialFirmwareIdentity::fdt)
                .filter(|identity| identity.node_path == *path)
                .cloned(),
            _ => None,
        };
        Ok(Self {
            gic,
            console: console_profile,
            serials,
            serial_identity,
            ivc_channels,
            timer,
        })
    }

    pub(super) const fn gic(&self) -> &GuestGicProfile {
        &self.gic
    }

    pub(super) const fn serial(&self) -> GuestSerialProfile {
        self.console
    }

    pub(super) const fn serial_identity(&self) -> Option<&GuestSerialFdtIdentity> {
        self.serial_identity.as_ref()
    }

    pub(super) const fn timer(&self) -> &GuestTimerProfile {
        &self.timer
    }

    pub(super) fn serials(&self) -> &[ResolvedSerialDevice] {
        &self.serials
    }

    pub(super) fn ivc_channels(&self) -> &[GuestIvcChannel] {
        &self.ivc_channels
    }
}

fn fallback_gic_profile(config: &ArmVgicConfig) -> AxVmResult<GuestGicProfile> {
    match config {
        ArmVgicConfig::V2(v2) => Ok(GuestGicProfile {
            compatible: "arm,cortex-a15-gic".into(),
            node_path: std::string::String::new(),
            node_phandle: None,
            distributor: guest_region(v2.distributor())?,
            cpu_region: GuestGicCpuRegion::CpuInterface(guest_region(v2.cpu_interface())?),
            its: std::vec![],
        }),
        ArmVgicConfig::V3(v3) => Ok(GuestGicProfile {
            compatible: "arm,gic-v3".into(),
            node_path: std::string::String::new(),
            node_phandle: None,
            distributor: guest_region(v3.distributor())?,
            cpu_region: GuestGicCpuRegion::Redistributors(GuestGicRedistributorProfile {
                regions: v3
                    .redistributors()
                    .iter()
                    .copied()
                    .map(guest_region)
                    .collect::<AxVmResult<_>>()?,
                stride: usize::try_from(v3.redistributor_stride()).map_err(|_| {
                    AxVmError::invalid_config("VGIC Redistributor stride does not fit usize")
                })?,
            }),
            its: v3
                .its()
                .iter()
                .map(|its| {
                    let registers = guest_region(its.registers())?;
                    Ok(GuestItsProfile {
                        id: its.id(),
                        node_path: std::format!("/its@{:x}", registers.base),
                        node_phandle: None,
                        registers,
                    })
                })
                .collect::<AxVmResult<_>>()?,
        }),
    }
}

fn guest_region(region: VgicMmioRegion) -> AxVmResult<GuestMmioRegion> {
    Ok(GuestMmioRegion {
        base: usize::try_from(region.base())
            .map_err(|_| AxVmError::invalid_config("VGIC MMIO base does not fit usize"))?,
        length: usize::try_from(region.size())
            .map_err(|_| AxVmError::invalid_config("VGIC MMIO size does not fit usize"))?,
    })
}
