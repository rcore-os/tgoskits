//! LoongArch firmware-interface selection and shared special validation.

use super::{MmioRegion, SerialDevice};
use crate::{
    AxVmError, AxVmResult,
    boot::{
        acpi::{
            ResolvedAcpiProperty, ResolvedAcpiRegister, ResolvedAcpiSpecial,
            ResolvedAcpiSpecialKind,
        },
        fdt::device::{ResolvedFdtProperty, ResolvedFdtSpecial, ResolvedFdtSpecialKind},
    },
    config::{AxVMConfig, GuestBootPolicy, VMBootProtocol},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::arch::loongarch64) enum GuestFirmwareSelection {
    Uefi,
    DirectFdt,
}

impl GuestFirmwareSelection {
    pub(super) const fn uses_acpi(self) -> bool {
        matches!(self, Self::Uefi)
    }
}

pub(in crate::arch::loongarch64) fn select_guest_firmware(
    config: &AxVMConfig,
) -> AxVmResult<GuestFirmwareSelection> {
    match config.boot_policy() {
        GuestBootPolicy::KeepConfigured
        | GuestBootPolicy::AdjustKernelForBootProtocol {
            protocol: VMBootProtocol::Direct,
        } => Ok(GuestFirmwareSelection::DirectFdt),
        GuestBootPolicy::AdjustKernelForBootProtocol {
            protocol: VMBootProtocol::Uefi,
        } => Ok(GuestFirmwareSelection::Uefi),
        GuestBootPolicy::AdjustKernelForBootProtocol {
            protocol: VMBootProtocol::Multiboot,
        } => Err(AxVmError::unsupported(
            "select LoongArch guest firmware",
            "LoongArch Multiboot is unsupported",
        )),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LoongArchCommonFirmware {
    pub(super) controller: axdevice_base::InterruptControllerId,
    pub(super) pch_pic: MmioRegion,
    pub(super) fw_cfg: MmioRegion,
}

pub(super) fn resolve_fdt_common_firmware(
    fdt: &[ResolvedFdtSpecial],
    serial: SerialDevice,
) -> AxVmResult<LoongArchCommonFirmware> {
    if fdt.len() != 3 {
        return Err(AxVmError::unsupported(
            "resolve LoongArch FDT topology",
            std::format!(
                "expected interrupt-controller, console, and fw_cfg contributions in FDT; found {}",
                fdt.len()
            ),
        ));
    }

    let controller_special = single_fdt_special(
        fdt,
        |kind| matches!(kind, ResolvedFdtSpecialKind::InterruptController(_)),
        "interrupt controller",
    )?;
    let ResolvedFdtSpecialKind::InterruptController(controller) = controller_special.kind else {
        unreachable!("the special selector checked the contribution kind")
    };
    let [pch_pic] = controller_special.registers.as_slice() else {
        return Err(AxVmError::invalid_config(
            "LoongArch FDT PCH-PIC contribution must resolve one MMIO window",
        ));
    };
    if controller_special.node_name != "interrupt-controller"
        || controller_special.compatible.as_slice() != ["loongson,pch-pic-1.0"]
        || !controller_special.interrupts.is_empty()
        || !controller_special.properties.is_empty()
    {
        return Err(AxVmError::invalid_config(
            "LoongArch FDT PCH-PIC contribution has an invalid identity or shape",
        ));
    }

    let fw_cfg_special = single_fdt_special(
        fdt,
        |kind| kind == ResolvedFdtSpecialKind::FirmwareTransport,
        "firmware transport",
    )?;
    let [fw_cfg] = fw_cfg_special.registers.as_slice() else {
        return Err(AxVmError::invalid_config(
            "LoongArch FDT fw_cfg contribution must resolve one MMIO window",
        ));
    };
    if fw_cfg_special.node_name != "fw_cfg"
        || fw_cfg_special.compatible.as_slice() != ["qemu,fw-cfg-mmio"]
        || !fw_cfg_special.interrupts.is_empty()
        || !matches!(
            fw_cfg_special.properties.as_slice(),
            [ResolvedFdtProperty::Empty(name)] if name == "dma-coherent"
        )
    {
        return Err(AxVmError::invalid_config(
            "LoongArch FDT fw_cfg contribution has an invalid identity or shape",
        ));
    }

    validate_fdt_console(fdt, serial, controller)?;
    Ok(LoongArchCommonFirmware {
        controller,
        pch_pic: MmioRegion {
            base: pch_pic.0,
            size: pch_pic.1,
        },
        fw_cfg: MmioRegion {
            base: fw_cfg.0,
            size: fw_cfg.1,
        },
    })
}

pub(super) fn cross_check_acpi_common_firmware(
    common: LoongArchCommonFirmware,
    acpi: &[ResolvedAcpiSpecial],
    serial: SerialDevice,
) -> AxVmResult {
    let controller = single_acpi_special(
        acpi,
        |kind| matches!(kind, ResolvedAcpiSpecialKind::InterruptController(_)),
        "interrupt controller",
    )?;
    let [ResolvedAcpiRegister::Mmio { base, size }] = controller.registers.as_slice() else {
        return Err(AxVmError::invalid_config(
            "LoongArch ACPI PCH-PIC contribution must resolve one MMIO window",
        ));
    };
    if controller.kind != ResolvedAcpiSpecialKind::InterruptController(common.controller)
        || (*base, *size) != (common.pch_pic.base, common.pch_pic.size)
        || controller.name != "PCH0"
        || controller.hid.is_some()
        || !controller.interrupts.is_empty()
        || !controller.properties.is_empty()
    {
        return Err(AxVmError::invalid_config(
            "LoongArch PCH-PIC FDT and ACPI contributions disagree",
        ));
    }

    let fw_cfg = single_acpi_special(
        acpi,
        |kind| kind == ResolvedAcpiSpecialKind::FirmwareTransport,
        "firmware transport",
    )?;
    let [ResolvedAcpiRegister::Mmio { base, size }] = fw_cfg.registers.as_slice() else {
        return Err(AxVmError::invalid_config(
            "LoongArch ACPI fw_cfg contribution must resolve one MMIO window",
        ));
    };
    if (*base, *size) != (common.fw_cfg.base, common.fw_cfg.size)
        || fw_cfg.name != "FWCF"
        || fw_cfg.hid.as_deref() != Some("QEMU0002")
        || !fw_cfg.interrupts.is_empty()
        || !fw_cfg.properties.is_empty()
    {
        return Err(AxVmError::invalid_config(
            "LoongArch fw_cfg FDT and ACPI contributions disagree",
        ));
    }

    let console = single_acpi_special(
        acpi,
        |kind| kind == ResolvedAcpiSpecialKind::Console,
        "console",
    )?;
    let [ResolvedAcpiRegister::Mmio { base, size }] = console.registers.as_slice() else {
        return Err(AxVmError::invalid_config(
            "LoongArch ACPI console contribution must resolve one MMIO window",
        ));
    };
    let [irq] = console.interrupts.as_slice() else {
        return Err(AxVmError::invalid_config(
            "LoongArch ACPI console contribution must resolve one interrupt",
        ));
    };
    if (*base, *size) != (serial.mmio.base, serial.mmio.size)
        || irq.controller != common.controller
        || irq.input != serial.irq
        || console.name != "COM0"
        || console.hid.as_deref() != Some("PNP0501")
        || !matches!(
            console.properties.as_slice(),
            [ResolvedAcpiProperty::U32(name, clock_hz)]
                if name == "clock-frequency" && *clock_hz == serial.clock_hz
        )
    {
        return Err(AxVmError::invalid_config(
            "LoongArch console FDT, ACPI, and runtime resources disagree",
        ));
    }
    Ok(())
}

fn validate_fdt_console(
    fdt: &[ResolvedFdtSpecial],
    serial: SerialDevice,
    controller: axdevice_base::InterruptControllerId,
) -> AxVmResult {
    let console = single_fdt_special(
        fdt,
        |kind| kind == ResolvedFdtSpecialKind::Console,
        "console",
    )?;
    let [register] = console.registers.as_slice() else {
        return Err(AxVmError::invalid_config(
            "LoongArch FDT console contribution must resolve one MMIO window",
        ));
    };
    let [irq] = console.interrupts.as_slice() else {
        return Err(AxVmError::invalid_config(
            "LoongArch FDT console contribution must resolve one interrupt",
        ));
    };
    if *register != (serial.mmio.base, serial.mmio.size)
        || console.node_name != "serial"
        || console.compatible.as_slice() != ["ns16550a"]
        || irq.controller != controller
        || irq.input != serial.irq
        || !matches!(
            console.properties.as_slice(),
            [
                ResolvedFdtProperty::U32(clock_name, clock_hz),
                ResolvedFdtProperty::U32(shift_name, register_shift),
                ResolvedFdtProperty::U32(width_name, register_width),
            ] if clock_name == "clock-frequency"
                && *clock_hz == serial.clock_hz
                && shift_name == "reg-shift"
                && *register_shift == u32::from(serial.register_shift)
                && width_name == "reg-io-width"
                && *register_width == u32::try_from(serial.register_width.size())
                    .expect("a serial access width is at most eight bytes")
        )
    {
        return Err(AxVmError::invalid_config(
            "LoongArch FDT console and runtime resources disagree",
        ));
    }
    Ok(())
}

fn single_fdt_special<'a>(
    specials: &'a [ResolvedFdtSpecial],
    predicate: impl Fn(ResolvedFdtSpecialKind) -> bool,
    name: &'static str,
) -> AxVmResult<&'a ResolvedFdtSpecial> {
    let mut matches = specials.iter().filter(|special| predicate(special.kind));
    let special = matches.next().ok_or_else(|| {
        AxVmError::invalid_config(std::format!("LoongArch FDT has no {name} contribution"))
    })?;
    if matches.next().is_some() {
        return Err(AxVmError::unsupported(
            "resolve LoongArch FDT topology",
            std::format!("multiple {name} contributions are not supported"),
        ));
    }
    Ok(special)
}

fn single_acpi_special<'a>(
    specials: &'a [ResolvedAcpiSpecial],
    predicate: impl Fn(ResolvedAcpiSpecialKind) -> bool,
    name: &'static str,
) -> AxVmResult<&'a ResolvedAcpiSpecial> {
    let mut matches = specials.iter().filter(|special| predicate(special.kind));
    let special = matches.next().ok_or_else(|| {
        AxVmError::invalid_config(std::format!("LoongArch ACPI has no {name} contribution"))
    })?;
    if matches.next().is_some() {
        return Err(AxVmError::unsupported(
            "resolve LoongArch ACPI topology",
            std::format!("multiple {name} contributions are not supported"),
        ));
    }
    Ok(special)
}
