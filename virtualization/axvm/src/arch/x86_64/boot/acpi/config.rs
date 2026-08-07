//! Small immutable x86 plans consumed by both direct and firmware ACPI paths.

use std::{
    format,
    string::{String, ToString},
    vec::Vec,
};

use axdevice::*;

use super::serial::*;

/// Guest processor/APIC identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct X86CpuPlan {
    pub(super) apic_ids: Vec<u8>,
}

/// Local and I/O APIC firmware description.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct X86InterruptPlan {
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
}

impl X86FirmwarePlan {
    pub(crate) fn from_graph(
        graph: &ResolvedDeviceGraph,
        cpu_count: usize,
    ) -> Result<Self, X86FirmwarePlanError> {
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

        let ioapic = resources_for_node(graph, "ioapic")?;
        let fw_cfg = resources_for_node(graph, "fw-cfg")?;
        let pm_timer = resources_for_node(graph, "acpi-pm-timer")?;
        let (io_apic_base, io_apic_size) = ioapic.mmio(&ResourceSlot::new("registers")?)?;
        if io_apic_size == 0 {
            return Err(X86FirmwarePlanError::InvalidValue {
                field: "I/O APIC window size",
                value: "0".into(),
            });
        }
        let serials = x86_serial_plans(graph)?;
        let (fw_cfg_selector_base, fw_cfg_selector_size) =
            fw_cfg.pio(&ResourceSlot::new("selector-data")?)?;
        let (fw_cfg_dma_base, fw_cfg_dma_size) = fw_cfg.pio(&ResourceSlot::new("dma")?)?;
        let (pm_timer_base, pm_timer_window_size) =
            pm_timer.pio(&ResourceSlot::new("registers")?)?;
        let sci = pm_timer.wired_irq(&ResourceSlot::new("sci")?)?;

        Ok(Self {
            cpus: X86CpuPlan { apic_ids },
            interrupts: X86InterruptPlan {
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
                        start: 0xc000_0000,
                        end: 0xfebf_ffff,
                        cacheable: false,
                    },
                ],
                intx_gsis: [16, 17, 18, 19],
            },
            power: X86PowerPlan {
                sci_irq: u16::try_from(sci.input().value()).map_err(|_| {
                    X86FirmwarePlanError::InvalidValue {
                        field: "ACPI SCI",
                        value: sci.input().value().to_string(),
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

fn resources_for_node<'a>(
    graph: &'a ResolvedDeviceGraph,
    node_id: &'static str,
) -> Result<&'a ResolvedDeviceResources, X86FirmwarePlanError> {
    let node = graph
        .nodes()
        .find(|node| node.id().as_str() == node_id)
        .ok_or(X86FirmwarePlanError::MissingDevice { node_id })?;
    graph.resources_for(node.id()).map_err(Into::into)
}

/// Failure to derive firmware facts from the sealed device graph.
#[derive(Debug, thiserror::Error)]
pub(crate) enum X86FirmwarePlanError {
    #[error("resolved x86 graph has no '{node_id}' node")]
    MissingDevice { node_id: &'static str },
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
    }
}
