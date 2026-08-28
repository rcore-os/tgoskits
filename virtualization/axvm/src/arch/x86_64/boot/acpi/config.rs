//! Small immutable x86 plans consumed by both direct and firmware ACPI paths.

use std::{
    format,
    string::{String, ToString},
    vec::Vec,
};

use axdevice::*;
use axdevice_base::InterruptControllerId;

use super::serial::*;
use crate::arch::x86_64::pci_config::{PCI_HOST_NODE, host_key as x86_pci_host_key};

/// Guest processor/APIC identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct X86CpuPlan {
    pub(super) apic_ids: Vec<u8>,
}

/// Local and I/O APIC firmware description.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct X86InterruptPlan {
    pub(super) controller: InterruptControllerId,
    pub(super) local_apic_base: u32,
    pub(super) io_apic_base: u32,
    pub(super) io_apic_id: u8,
    pub(super) gsi_base: u32,
}

/// PCI INTx routing exposed by the current virtual IOAPIC policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct X86PciPlan {
    pub(super) bus_range: (u16, u16),
    pub(super) io_windows: [(u16, u16); 2],
    pub(super) memory_windows: [X86PciMemoryWindow; 2],
    pub(super) intx_gsis: [u32; 4],
}

/// One firmware-visible PCI host bridge memory aperture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct X86PciMemoryWindow {
    pub(super) start: u32,
    pub(super) end: u32,
    pub(super) cacheable: bool,
}

/// ACPI power-management resources backed by modeled virtual hardware.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct X86PowerPlan {
    pub(super) sci_irq: u16,
    pub(super) pm1_event: X86AcpiIoRegisterPlan,
    pub(super) pm1_control: X86AcpiIoRegisterPlan,
    pub(super) pm_timer: X86AcpiIoRegisterPlan,
}

/// One System-I/O register block published through the FADT.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct X86AcpiIoRegisterPlan {
    pub(super) port: u16,
    pub(super) length: u8,
}

/// Firmware-visible serial and fw_cfg resources resolved by the planner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct X86FirmwareResources {
    pub(super) serials: Vec<X86SerialPlan>,
    pub(super) fw_cfg_selector_base: u16,
    pub(super) fw_cfg_selector_size: u16,
    pub(super) fw_cfg_dma_base: u16,
    pub(super) fw_cfg_dma_size: u16,
}

/// Complete x86 firmware input assembled from smaller architecture plans.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct X86FirmwarePlan {
    pub(super) cpus: X86CpuPlan,
    pub(super) interrupts: X86InterruptPlan,
    pub(super) pci: X86PciPlan,
    pub(super) power: X86PowerPlan,
    pub(super) resources: X86FirmwareResources,
    pub(super) configured_devices: Vec<crate::boot::acpi::ResolvedAcpiDevice>,
}

impl X86FirmwarePlan {
    pub(crate) fn from_graph(
        graph: &ResolvedDeviceGraph,
        cpu_count: usize,
    ) -> Result<Self, X86FirmwarePlanError> {
        let firmware = crate::boot::acpi::resolve_acpi_firmware(graph).map_err(|error| {
            DeviceManagerError::InvalidConfig {
                operation: "resolve x86 ACPI firmware contributions",
                detail: format!("{error}"),
            }
        })?;
        let apic_ids = (0..cpu_count)
            .map(|id| {
                u8::try_from(id).map_err(|_| X86FirmwarePlanError::InvalidValue {
                    field: "x86 APIC ID",
                    value: id.to_string(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if apic_ids.is_empty() {
            return Err(X86FirmwarePlanError::InvalidValue {
                field: "x86 vCPU count",
                value: "0".into(),
            });
        }

        let serials = x86_serial_plans(graph)?;
        let specials = resolve_x86_specials(&firmware.specials, &serials)?;
        let pci_topology =
            graph
                .pci_topology(&x86_pci_host_key())
                .ok_or(X86FirmwarePlanError::MissingDevice {
                    node_id: PCI_HOST_NODE,
                })?;
        let pci_aperture = pci_topology.memory_aperture();
        let pci_size = pci_aperture
            .end
            .checked_sub(pci_aperture.start)
            .ok_or_else(|| X86FirmwarePlanError::InvalidValue {
                field: "PCI memory aperture",
                value: "empty or reversed".into(),
            })?;
        if specials.pci_memory != (pci_aperture.start, pci_size) {
            return Err(X86FirmwarePlanError::InvalidValue {
                field: "PCI firmware/runtime aperture",
                value: "ACPI contribution differs from resolved PCI topology".into(),
            });
        }
        let pci_memory_end =
            pci_aperture
                .end
                .checked_sub(1)
                .ok_or_else(|| X86FirmwarePlanError::InvalidValue {
                    field: "PCI memory aperture",
                    value: "cannot encode inclusive end".into(),
                })?;
        let pci_memory_start =
            u32::try_from(pci_aperture.start).map_err(|_| X86FirmwarePlanError::InvalidValue {
                field: "PCI memory aperture start",
                value: format!("{:#x}", pci_aperture.start),
            })?;
        let pci_memory_end =
            u32::try_from(pci_memory_end).map_err(|_| X86FirmwarePlanError::InvalidValue {
                field: "PCI memory aperture end",
                value: format!("{pci_memory_end:#x}"),
            })?;
        let (io_apic_base, io_apic_size) = specials.ioapic;
        if io_apic_size == 0 {
            return Err(X86FirmwarePlanError::InvalidValue {
                field: "I/O APIC window size",
                value: "0".into(),
            });
        }
        let (fw_cfg_selector_base, fw_cfg_selector_size) = specials.fw_cfg[0];
        let (fw_cfg_dma_base, fw_cfg_dma_size) = specials.fw_cfg[1];
        let (pm_timer_base, pm_timer_window_size) = specials.pm_timer;
        let sci = specials.sci;

        Ok(Self {
            cpus: X86CpuPlan { apic_ids },
            interrupts: X86InterruptPlan {
                controller: specials.controller,
                local_apic_base: u32::try_from(x86_vcpu::X86_LOCAL_APIC_GPA).map_err(|_| {
                    X86FirmwarePlanError::InvalidValue {
                        field: "local APIC address",
                        value: format!("{:#x}", x86_vcpu::X86_LOCAL_APIC_GPA),
                    }
                })?,
                io_apic_base: u32::try_from(io_apic_base).map_err(|_| {
                    X86FirmwarePlanError::InvalidValue {
                        field: "I/O APIC address",
                        value: format!("{io_apic_base:#x}"),
                    }
                })?,
                io_apic_id: 1,
                gsi_base: 0,
            },
            pci: X86PciPlan {
                bus_range: (0, 0xff),
                io_windows: [(0, 0x0cf7), (0x0d00, u16::MAX)],
                memory_windows: [
                    X86PciMemoryWindow {
                        start: 0x000a_0000,
                        end: 0x000b_ffff,
                        cacheable: true,
                    },
                    X86PciMemoryWindow {
                        start: pci_memory_start,
                        end: pci_memory_end,
                        cacheable: false,
                    },
                ],
                intx_gsis: [16, 17, 18, 19],
            },
            power: X86PowerPlan {
                sci_irq: u16::try_from(sci.input).map_err(|_| {
                    X86FirmwarePlanError::InvalidValue {
                        field: "ACPI SCI",
                        value: sci.input.to_string(),
                    }
                })?,
                pm1_event: resolved_pm_register(
                    pm_timer_base,
                    pm_timer_window_size,
                    axdevice::X86AcpiPmTimerDevice::EVENT_PORT_OFFSET,
                    axdevice::X86AcpiPmTimerDevice::EVENT_REGISTER_SIZE,
                    "ACPI PM1 event block",
                )?,
                pm1_control: resolved_pm_register(
                    pm_timer_base,
                    pm_timer_window_size,
                    axdevice::X86AcpiPmTimerDevice::CONTROL_PORT_OFFSET,
                    axdevice::X86AcpiPmTimerDevice::CONTROL_REGISTER_SIZE,
                    "ACPI PM1 control block",
                )?,
                pm_timer: resolved_pm_register(
                    pm_timer_base,
                    pm_timer_window_size,
                    axdevice::X86AcpiPmTimerDevice::TIMER_PORT_OFFSET,
                    axdevice::X86AcpiPmTimerDevice::TIMER_REGISTER_SIZE,
                    "ACPI PM timer block",
                )?,
            },
            resources: X86FirmwareResources {
                serials,
                fw_cfg_selector_base,
                fw_cfg_selector_size,
                fw_cfg_dma_base,
                fw_cfg_dma_size,
            },
            configured_devices: firmware.devices,
        })
    }

    pub(crate) fn apic_ids(&self) -> &[u8] {
        &self.cpus.apic_ids
    }

    pub(crate) const fn local_apic_base(&self) -> u32 {
        self.interrupts.local_apic_base
    }

    pub(crate) const fn io_apic_base(&self) -> u32 {
        self.interrupts.io_apic_base
    }

    pub(crate) fn fw_cfg_range(&self) -> Result<(usize, usize), X86FirmwarePlanError> {
        let base = usize::from(self.resources.fw_cfg_selector_base);
        let end = usize::from(self.resources.fw_cfg_dma_base)
            .checked_add(usize::from(self.resources.fw_cfg_dma_size))
            .ok_or_else(|| X86FirmwarePlanError::InvalidValue {
                field: "fw_cfg PIO range",
                value: "address overflow".into(),
            })?;
        let size = end
            .checked_sub(base)
            .ok_or_else(|| X86FirmwarePlanError::InvalidValue {
                field: "fw_cfg PIO range",
                value: std::format!("DMA end {end:#x} precedes selector base {base:#x}"),
            })?;
        Ok((base, size))
    }
}

struct ResolvedX86Specials {
    controller: InterruptControllerId,
    ioapic: (u64, u64),
    fw_cfg: [(u16, u16); 2],
    pm_timer: (u16, u16),
    sci: crate::boot::acpi::ResolvedAcpiInterrupt,
    pci_memory: (u64, u64),
}

fn resolve_x86_specials(
    specials: &[crate::boot::acpi::ResolvedAcpiSpecial],
    serials: &[X86SerialPlan],
) -> Result<ResolvedX86Specials, X86FirmwarePlanError> {
    use crate::boot::acpi::{ResolvedAcpiRegister, ResolvedAcpiSpecialKind};

    let ioapic = named_special(specials, "IOAP", "I/O APIC")?;
    let ResolvedAcpiSpecialKind::InterruptController(controller) = ioapic.kind else {
        return invalid_special_kind(ioapic, "interrupt controller");
    };
    let [
        ResolvedAcpiRegister::Mmio {
            base: ioapic_base,
            size: ioapic_size,
        },
    ] = ioapic.registers.as_slice()
    else {
        return invalid_special_shape(ioapic, "exactly one MMIO register window");
    };
    if ioapic.hid.is_some() || !ioapic.interrupts.is_empty() || !ioapic.properties.is_empty() {
        return invalid_special_shape(ioapic, "an I/O APIC table contribution without properties");
    }

    let pic = named_special(specials, "PIC0", "legacy PIC")?;
    if !matches!(
        pic.kind,
        ResolvedAcpiSpecialKind::InterruptController(id) if id == controller
    ) || pic.hid.as_deref() != Some("PNP0000")
        || !all_pio_registers(pic, 2)
        || !pic.interrupts.is_empty()
        || !pic.properties.is_empty()
    {
        return invalid_special_shape(pic, "PNP0000 interrupt controller with two PIO windows");
    }

    let pit = named_special(specials, "PIT0", "legacy PIT")?;
    if pit.kind != ResolvedAcpiSpecialKind::Timer
        || pit.hid.as_deref() != Some("PNP0100")
        || !all_pio_registers(pit, 2)
        || !pit.interrupts.is_empty()
        || !pit.properties.is_empty()
    {
        return invalid_special_shape(pit, "PNP0100 timer with two PIO windows");
    }

    let pm_timer = named_special(specials, "PMTM", "ACPI PM timer")?;
    let [
        ResolvedAcpiRegister::Pio {
            base: pm_timer_base,
            size: pm_timer_size,
        },
    ] = pm_timer.registers.as_slice()
    else {
        return invalid_special_shape(pm_timer, "exactly one PIO register window");
    };
    let [sci] = pm_timer.interrupts.as_slice() else {
        return invalid_special_shape(pm_timer, "exactly one SCI interrupt");
    };
    if pm_timer.kind != ResolvedAcpiSpecialKind::Timer
        || pm_timer.hid.as_deref() != Some("ACPI0008")
        || sci.controller != controller
        || !pm_timer.properties.is_empty()
    {
        return invalid_special_shape(
            pm_timer,
            "ACPI0008 timer connected to the I/O APIC controller",
        );
    }

    let pci = named_special(specials, "PCI0", "PCI host bridge")?;
    let [
        ResolvedAcpiRegister::Pio { .. },
        ResolvedAcpiRegister::Mmio {
            base: pci_memory_base,
            size: pci_memory_size,
        },
    ] = pci.registers.as_slice()
    else {
        return invalid_special_shape(pci, "one CF8/CFC PIO window and one memory aperture");
    };
    if pci.kind != ResolvedAcpiSpecialKind::PciHostBridge
        || pci.hid.as_deref() != Some("PNP0A03")
        || !pci.interrupts.is_empty()
        || !pci.properties.is_empty()
    {
        return invalid_special_shape(pci, "PNP0A03 bridge with CF8/CFC and memory aperture");
    }

    let fw_cfg = named_special(specials, "FWCF", "fw_cfg transport")?;
    let [
        ResolvedAcpiRegister::Pio {
            base: selector_base,
            size: selector_size,
        },
        ResolvedAcpiRegister::Pio {
            base: dma_base,
            size: dma_size,
        },
    ] = fw_cfg.registers.as_slice()
    else {
        return invalid_special_shape(fw_cfg, "selector/data and DMA PIO windows");
    };
    if fw_cfg.kind != ResolvedAcpiSpecialKind::FirmwareTransport
        || fw_cfg.hid.as_deref() != Some("QEMU0002")
        || !fw_cfg.interrupts.is_empty()
        || !fw_cfg.properties.is_empty()
    {
        return invalid_special_shape(fw_cfg, "QEMU0002 firmware transport");
    }

    validate_console_specials(specials, serials, controller)?;
    let expected_specials =
        6usize
            .checked_add(serials.len())
            .ok_or_else(|| X86FirmwarePlanError::InvalidValue {
                field: "x86 ACPI special contribution count",
                value: "overflow".into(),
            })?;
    if specials.len() != expected_specials {
        return Err(X86FirmwarePlanError::InvalidValue {
            field: "x86 ACPI special contributions",
            value: format!(
                "expected {expected_specials} consumed contributions, found {}",
                specials.len()
            ),
        });
    }

    Ok(ResolvedX86Specials {
        controller,
        ioapic: (*ioapic_base, *ioapic_size),
        fw_cfg: [(*selector_base, *selector_size), (*dma_base, *dma_size)],
        pm_timer: (*pm_timer_base, *pm_timer_size),
        sci: *sci,
        pci_memory: (*pci_memory_base, *pci_memory_size),
    })
}

fn validate_console_specials(
    specials: &[crate::boot::acpi::ResolvedAcpiSpecial],
    serials: &[X86SerialPlan],
    controller: InterruptControllerId,
) -> Result<(), X86FirmwarePlanError> {
    use crate::boot::acpi::{ResolvedAcpiProperty, ResolvedAcpiRegister, ResolvedAcpiSpecialKind};

    for serial in serials {
        let console = specials
            .iter()
            .find(|special| {
                special.id == serial.id && special.kind == ResolvedAcpiSpecialKind::Console
            })
            .ok_or(X86FirmwarePlanError::MissingContribution {
                contribution: "console",
            })?;
        let register_matches = match (console.registers.as_slice(), serial.registers) {
            (
                [ResolvedAcpiRegister::Pio { base, size }],
                X86SerialRegisters::Port {
                    base: expected_base,
                    size: expected_size,
                },
            ) => (*base, *size) == (expected_base, expected_size),
            (
                [ResolvedAcpiRegister::Mmio { base, size }],
                X86SerialRegisters::Mmio {
                    base: expected_base,
                    size: expected_size,
                },
            ) => (*base, *size) == (u64::from(expected_base), u64::from(expected_size)),
            _ => false,
        };
        let [interrupt] = console.interrupts.as_slice() else {
            return invalid_special_shape(console, "exactly one console interrupt");
        };
        if !register_matches
            || interrupt.controller != controller
            || interrupt.input != serial.irq
            || console.hid.as_deref() != Some(serial.hid.as_str())
            || !matches!(
                console.properties.as_slice(),
                [ResolvedAcpiProperty::U32(name, clock_hz)]
                    if name == "clock-frequency" && *clock_hz == serial.clock_hz
            )
        {
            return invalid_special_shape(
                console,
                "registers and interrupt matching the resolved serial runtime",
            );
        }
    }
    Ok(())
}

fn named_special<'a>(
    specials: &'a [crate::boot::acpi::ResolvedAcpiSpecial],
    name: &str,
    contribution: &'static str,
) -> Result<&'a crate::boot::acpi::ResolvedAcpiSpecial, X86FirmwarePlanError> {
    let mut matches = specials.iter().filter(|special| special.name == name);
    let special = matches
        .next()
        .ok_or(X86FirmwarePlanError::MissingContribution { contribution })?;
    if matches.next().is_some() {
        return Err(X86FirmwarePlanError::InvalidValue {
            field: "x86 ACPI contribution name",
            value: format!("duplicate '{name}'"),
        });
    }
    Ok(special)
}

fn all_pio_registers(special: &crate::boot::acpi::ResolvedAcpiSpecial, count: usize) -> bool {
    special.registers.len() == count
        && special.registers.iter().all(|register| {
            matches!(
                register,
                crate::boot::acpi::ResolvedAcpiRegister::Pio { .. }
            )
        })
}

fn invalid_special_kind<T>(
    special: &crate::boot::acpi::ResolvedAcpiSpecial,
    expected: &'static str,
) -> Result<T, X86FirmwarePlanError> {
    invalid_special_shape(special, expected)
}

fn invalid_special_shape<T>(
    special: &crate::boot::acpi::ResolvedAcpiSpecial,
    expected: &'static str,
) -> Result<T, X86FirmwarePlanError> {
    Err(X86FirmwarePlanError::InvalidValue {
        field: "x86 ACPI special contribution",
        value: format!("{} ({}) must be {expected}", special.id, special.name),
    })
}

fn resolved_pm_register(
    base: u16,
    window_size: u16,
    offset: u16,
    register_size: u16,
    field: &'static str,
) -> Result<X86AcpiIoRegisterPlan, X86FirmwarePlanError> {
    let required_size =
        offset
            .checked_add(register_size)
            .ok_or_else(|| X86FirmwarePlanError::InvalidValue {
                field,
                value: "register range overflow".into(),
            })?;
    if window_size < required_size {
        return Err(X86FirmwarePlanError::InvalidValue {
            field,
            value: format!(
                "window size {window_size:#x} does not contain range ending at {required_size:#x}"
            ),
        });
    }
    let port = base
        .checked_add(offset)
        .ok_or_else(|| X86FirmwarePlanError::InvalidValue {
            field,
            value: "port address overflow".into(),
        })?;
    let length = u8::try_from(register_size).map_err(|_| X86FirmwarePlanError::InvalidValue {
        field,
        value: format!("register size {register_size:#x} exceeds the FADT field"),
    })?;
    Ok(X86AcpiIoRegisterPlan { port, length })
}

/// Failure to derive firmware facts from the sealed device graph.
#[derive(Debug, thiserror::Error)]
pub(crate) enum X86FirmwarePlanError {
    #[error("resolved x86 graph has no '{node_id}' node")]
    MissingDevice { node_id: &'static str },
    #[error("resolved x86 graph has no {contribution} firmware contribution")]
    MissingContribution { contribution: &'static str },
    #[error("invalid {field}: {value}")]
    InvalidValue { field: &'static str, value: String },
    #[error(transparent)]
    Device(#[from] DeviceManagerError),
}

#[cfg(test)]
pub(super) fn test_plan(cpu_count: u8) -> X86FirmwarePlan {
    X86FirmwarePlan {
        cpus: X86CpuPlan {
            apic_ids: (0..cpu_count).collect(),
        },
        interrupts: X86InterruptPlan {
            controller: InterruptControllerId::new(0),
            local_apic_base: 0xfee0_0000,
            io_apic_base: 0xfec0_0000,
            io_apic_id: 1,
            gsi_base: 0,
        },
        pci: X86PciPlan {
            bus_range: (0, 0xff),
            io_windows: [(0, 0x0cf7), (0x0d00, u16::MAX)],
            memory_windows: [
                X86PciMemoryWindow {
                    start: 0x000a_0000,
                    end: 0x000b_ffff,
                    cacheable: true,
                },
                X86PciMemoryWindow {
                    start: 0xc000_0000,
                    end: 0xfebf_ffff,
                    cacheable: false,
                },
            ],
            intx_gsis: [16, 17, 18, 19],
        },
        power: X86PowerPlan {
            sci_irq: 9,
            pm1_event: X86AcpiIoRegisterPlan {
                port: 0x600,
                length: 4,
            },
            pm1_control: X86AcpiIoRegisterPlan {
                port: 0x604,
                length: 2,
            },
            pm_timer: X86AcpiIoRegisterPlan {
                port: 0x608,
                length: 4,
            },
        },
        resources: X86FirmwareResources {
            serials: std::vec![X86SerialPlan {
                id: "console0".into(),
                name: "COM1".into(),
                namespace_path: None,
                hid: "PNP0501".into(),
                interface_type: 0,
                registers: X86SerialRegisters::Port {
                    base: 0x3f8,
                    size: 8,
                },
                irq: 4,
                clock_hz: 1_843_200,
            }],
            fw_cfg_selector_base: 0x510,
            fw_cfg_selector_size: 2,
            fw_cfg_dma_base: 0x514,
            fw_cfg_dma_size: 8,
        },
        configured_devices: Vec::new(),
    }
}
