//! Guest firmware identity frozen from the same VGIC construction plan.

use arm_vgic::{ArmVgicConfig, VgicMmioRegion};

use crate::{config::*, machine::*, *};

pub(super) struct Aarch64FirmwarePlan {
    gic: GuestGicProfile,
    console: GuestSerialProfile,
    serials: std::vec::Vec<ResolvedSerialDevice>,
    serial_identity: Option<GuestSerialFdtIdentity>,
    devices: std::vec::Vec<crate::boot::fdt::device::ResolvedFdtDevice>,
    timer: GuestTimerProfile,
}

impl Aarch64FirmwarePlan {
    pub(super) fn new(
        config: &AxVMConfig,
        vgic: &ArmVgicConfig,
        graph: &axdevice::ResolvedDeviceGraph,
    ) -> AxVmResult<Self> {
        let mut gic = match config.gic_profile() {
            Some(profile) => profile.clone(),
            None => fallback_gic_profile(vgic)?,
        };
        let timer = config.timer_profile().cloned().ok_or_else(|| {
            AxVmError::invalid_config("AArch64 machine profile has no architectural timer")
        })?;
        let serials = resolved_serial_devices(graph)?;
        let firmware = crate::boot::fdt::device::resolve_fdt_firmware(graph)?;
        apply_gic_contribution(&firmware.specials, &serials, vgic, &mut gic)?;
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
            devices: firmware.devices,
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

    pub(super) fn devices(&self) -> &[crate::boot::fdt::device::ResolvedFdtDevice] {
        &self.devices
    }
}

fn apply_gic_contribution(
    specials: &[crate::boot::fdt::device::ResolvedFdtSpecial],
    serials: &[ResolvedSerialDevice],
    vgic: &ArmVgicConfig,
    profile: &mut GuestGicProfile,
) -> AxVmResult {
    use crate::boot::fdt::device::ResolvedFdtSpecialKind;

    let mut controllers = specials
        .iter()
        .filter(|special| matches!(special.kind, ResolvedFdtSpecialKind::InterruptController(_)));
    let controller = controllers.next().ok_or_else(|| {
        AxVmError::invalid_config("AArch64 FDT has no interrupt-controller contribution")
    })?;
    if controllers.next().is_some() {
        return Err(AxVmError::unsupported(
            "resolve AArch64 FDT topology",
            "multiple interrupt-controller contributions are not supported",
        ));
    }
    if controller.kind != ResolvedFdtSpecialKind::InterruptController(vgic.controller_id()) {
        return Err(AxVmError::invalid_config(
            "AArch64 FDT controller identity differs from the VGIC runtime",
        ));
    }
    let expected_registers = gic_registers(profile)?;
    if controller.node_name != "interrupt-controller"
        || controller.registers != expected_registers
        || !controller.interrupts.is_empty()
        || !controller.properties.is_empty()
    {
        return Err(AxVmError::invalid_config(
            "AArch64 FDT controller resources differ from the VGIC runtime plan",
        ));
    }
    let [compatible] = controller.compatible.as_slice() else {
        return Err(AxVmError::invalid_config(
            "AArch64 FDT controller must declare exactly one compatible string",
        ));
    };
    let expected_compatible = match vgic {
        ArmVgicConfig::V2(_) => "arm,gic-400",
        ArmVgicConfig::V3(_) => "arm,gic-v3",
    };
    if compatible != expected_compatible {
        return Err(AxVmError::invalid_config(std::format!(
            "AArch64 FDT controller compatible '{compatible}' differs from {expected_compatible}"
        )));
    }
    profile.compatible.clone_from(compatible);

    let consoles = specials
        .iter()
        .filter(|special| special.kind == ResolvedFdtSpecialKind::Console)
        .collect::<std::vec::Vec<_>>();
    if consoles.len() != serials.len()
        || serials
            .iter()
            .any(|serial| consoles.iter().all(|console| console.id != serial.id()))
        || consoles.iter().any(|console| {
            serials
                .iter()
                .find(|serial| serial.id() == console.id)
                .is_none_or(|serial| {
                    !crate::boot::fdt::device::fdt_console_matches_serial(
                        console,
                        serial,
                        vgic.controller_id(),
                    )
                })
        })
    {
        return Err(AxVmError::invalid_config(
            "AArch64 console contributions differ from resolved serial devices",
        ));
    }
    if specials.len() != 1 + consoles.len() {
        return Err(AxVmError::unsupported(
            "resolve AArch64 FDT topology",
            "the graph contains an unsupported special contribution",
        ));
    }
    Ok(())
}

fn gic_registers(profile: &GuestGicProfile) -> AxVmResult<std::vec::Vec<(u64, u64)>> {
    let mut registers = std::vec![gic_register(profile.distributor)?];
    match &profile.cpu_region {
        GuestGicCpuRegion::CpuInterface(region) => registers.push(gic_register(*region)?),
        GuestGicCpuRegion::Redistributors(redistributors) => {
            registers.extend(
                redistributors
                    .regions
                    .iter()
                    .copied()
                    .map(gic_register)
                    .collect::<AxVmResult<std::vec::Vec<_>>>()?,
            );
        }
    }
    registers.extend(
        profile
            .its
            .iter()
            .map(|its| gic_register(its.registers))
            .collect::<AxVmResult<std::vec::Vec<_>>>()?,
    );
    Ok(registers)
}

fn gic_register(region: GuestMmioRegion) -> AxVmResult<(u64, u64)> {
    Ok((
        u64::try_from(region.base)
            .map_err(|_| AxVmError::invalid_config("GIC base exceeds u64"))?,
        u64::try_from(region.length)
            .map_err(|_| AxVmError::invalid_config("GIC length exceeds u64"))?,
    ))
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
