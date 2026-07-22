//! Dynamically allocated virtual-device nodes for host-derived FDT guests.

use alloc::{format, string::String, vec, vec::Vec};

use fdt_edit::{Fdt, Node, Property};
use fdt_raw::RegInfo;
use virtual_ns16550::Ns16550RegisterLayout;

use super::fixed_clock::{add_fixed_clock, next_phandle};
use crate::machine::{
    InterruptControllerPlan, MachinePlanError, MachinePlanResult, ResolvedVirtualDevice,
    VmMachinePlan,
};

pub(super) fn sanitize_virtual_device_templates(
    source: &mut Fdt,
    plan: &VmMachinePlan,
) -> MachinePlanResult<()> {
    for device in plan.virtual_devices() {
        let Some(template) = device.host_template() else {
            continue;
        };
        let node_id = source.get_by_path_id(template.as_str()).ok_or_else(|| {
            MachinePlanError::InvalidFirmware {
                detail: format!(
                    "virtual-device template '{}' is absent from the host FDT",
                    template
                ),
            }
        })?;
        let node = source
            .node_mut(node_id)
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: format!("virtual-device template '{}' cannot be sanitized", template),
            })?;
        remove_physical_capabilities(node);
    }
    Ok(())
}

pub(super) fn materialize_virtual_devices(
    guest: &mut Fdt,
    plan: &VmMachinePlan,
) -> MachinePlanResult<()> {
    for (console_index, device) in plan.virtual_devices().iter().enumerate() {
        let kind = UartKind::from_model(device.model_id().as_str())?;
        if let Some(template) = device.host_template() {
            patch_template_uart(guest, plan, device, kind, template.as_str(), console_index)?;
        } else {
            add_dynamic_uart(guest, plan, device, kind, console_index)?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum UartKind {
    Pl011,
    Ns16550,
    DwApb,
}

impl UartKind {
    fn from_model(model: &str) -> MachinePlanResult<Self> {
        match model {
            "arm-pl011" => Ok(Self::Pl011),
            "ns16550a" => Ok(Self::Ns16550),
            "snps-dw-apb-uart" => Ok(Self::DwApb),
            _ => Err(MachinePlanError::InvalidFirmware {
                detail: format!("host-derived FDT cannot describe virtual model '{model}'"),
            }),
        }
    }

    const fn compatible(self) -> &'static [&'static str] {
        match self {
            Self::Pl011 => &["arm,pl011", "arm,primecell"],
            Self::Ns16550 => &["ns16550a"],
            Self::DwApb => &["snps,dw-apb-uart", "ns16550a"],
        }
    }

    const fn clock_hz(self) -> u32 {
        match self {
            Self::Pl011 => 24_000_000,
            Self::Ns16550 | Self::DwApb => 100_000_000,
        }
    }

    fn write_register_layout(self, uart: &mut Node) {
        match self {
            Self::Pl011 => {
                uart.remove_property("reg-shift");
                uart.remove_property("reg-io-width");
            }
            Self::Ns16550 => {
                write_ns16550_register_layout(uart, Ns16550RegisterLayout::Packed);
            }
            Self::DwApb => {
                write_ns16550_register_layout(uart, Ns16550RegisterLayout::DwApb);
            }
        }
    }
}

fn write_ns16550_register_layout(uart: &mut Node, layout: Ns16550RegisterLayout) {
    uart.set_property(u32_list_property("reg-shift", &[layout.register_shift()]));
    uart.set_property(u32_list_property(
        "reg-io-width",
        &[layout.register_io_width()],
    ));
}

fn patch_template_uart(
    guest: &mut Fdt,
    plan: &VmMachinePlan,
    device: &ResolvedVirtualDevice,
    kind: UartKind,
    template_path: &str,
    console_index: usize,
) -> MachinePlanResult<()> {
    let registers = device_registers(device)?;
    let interrupt = device_interrupt(device)?;
    let interrupt_cells = interrupt_specifier(plan, interrupt)?;
    let interrupt_parent = interrupt_parent_phandle(guest, plan)?;
    let clock = matches!(kind, UartKind::Pl011)
        .then(|| add_fixed_clock(guest, kind.clock_hz()))
        .transpose()?;
    let node_id =
        guest
            .get_by_path_id(template_path)
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: format!("virtual-device template '{template_path}' was not copied"),
            })?;
    let uart = guest
        .node_mut(node_id)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: format!("virtual-device template '{template_path}' cannot be updated"),
        })?;
    remove_physical_capabilities(uart);
    uart.set_property(string_list_property("compatible", kind.compatible()));
    uart.set_property(u32_list_property("clock-frequency", &[kind.clock_hz()]));
    uart.set_property(u32_list_property("interrupt-parent", &[interrupt_parent]));
    uart.set_property(u32_list_property("interrupts", &interrupt_cells));
    uart.set_property(string_property("status", "okay"));
    kind.write_register_layout(uart);
    if let Some(clock) = clock {
        uart.set_property(u32_list_property("clocks", &[clock, clock]));
        uart.set_property(string_list_property(
            "clock-names",
            &["uartclk", "apb_pclk"],
        ));
    }
    guest
        .view_typed_mut(node_id)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: format!("virtual-device template '{template_path}' has invalid reg encoding"),
        })?
        .set_regs(&[RegInfo::new(registers.base(), Some(registers.size()))]);
    register_console_alias(guest, template_path, console_index)
}

fn remove_physical_capabilities(node: &mut fdt_edit::Node) {
    let properties = node
        .properties()
        .iter()
        .map(|property| String::from(property.name()))
        .filter(|name| is_physical_capability_property(name))
        .collect::<Vec<_>>();
    for property in properties {
        node.remove_property(&property);
    }
}

fn is_physical_capability_property(name: &str) -> bool {
    matches!(
        name,
        "assigned-clock-parents"
            | "assigned-clock-rates"
            | "assigned-clocks"
            | "clock-names"
            | "clocks"
            | "dma-coherent"
            | "dma-names"
            | "dmas"
            | "interconnect-names"
            | "interconnects"
            | "interrupts-extended"
            | "iommu-map"
            | "iommu-map-mask"
            | "iommus"
            | "mbox-names"
            | "mboxes"
            | "memory-region"
            | "memory-region-names"
            | "msi-map"
            | "msi-map-mask"
            | "msi-parent"
            | "nvmem-cell-names"
            | "nvmem-cells"
            | "phy-names"
            | "phys"
            | "pinctrl-names"
            | "power-domains"
            | "reset-names"
            | "resets"
    ) || name.starts_with("pinctrl-")
        || name.ends_with("-supply")
        || name.ends_with("-gpio")
        || name.ends_with("-gpios")
}

fn add_dynamic_uart(
    guest: &mut Fdt,
    plan: &VmMachinePlan,
    device: &ResolvedVirtualDevice,
    kind: UartKind,
    console_index: usize,
) -> MachinePlanResult<()> {
    let registers = device_registers(device)?;
    let interrupt = device_interrupt(device)?;
    let interrupt_cells = interrupt_specifier(plan, interrupt)?;
    let interrupt_parent = interrupt_parent_phandle(guest, plan)?;
    let clock = if matches!(kind, UartKind::Pl011) {
        Some(add_fixed_clock(guest, kind.clock_hz())?)
    } else {
        None
    };
    let root = guest.root_id();
    let node_name = format!("serial@{:x}", registers.base());
    let node = guest.add_node(root, Node::new(&node_name));

    let uart = guest
        .node_mut(node)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: format!("new virtual UART node '{node_name}' cannot be updated"),
        })?;
    uart.set_property(string_list_property("compatible", kind.compatible()));
    uart.set_property(u32_list_property("clock-frequency", &[kind.clock_hz()]));
    uart.set_property(u32_list_property("interrupt-parent", &[interrupt_parent]));
    uart.set_property(u32_list_property("interrupts", &interrupt_cells));
    kind.write_register_layout(uart);
    if let Some(clock) = clock {
        uart.set_property(u32_list_property("clocks", &[clock, clock]));
        uart.set_property(string_list_property(
            "clock-names",
            &["uartclk", "apb_pclk"],
        ));
    }
    guest
        .view_typed_mut(node)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: "new virtual UART cannot be represented".into(),
        })?
        .set_regs(&[RegInfo::new(registers.base(), Some(registers.size()))]);
    register_console_alias(guest, &format!("/{node_name}"), console_index)?;
    Ok(())
}

fn interrupt_parent_phandle(guest: &mut Fdt, plan: &VmMachinePlan) -> MachinePlanResult<u32> {
    let controller = guest
        .iter_node_ids()
        .find(|node_id| {
            let Some(node) = guest.node(*node_id) else {
                return false;
            };
            match plan.interrupt_controller() {
                Some(InterruptControllerPlan::Aarch64GicV3(_)) => node
                    .compatibles()
                    .any(|compatible| compatible == "arm,gic-v3"),
                Some(InterruptControllerPlan::RiscvPlic(_)) => node
                    .compatibles()
                    .any(|compatible| compatible.starts_with("riscv,plic")),
                _ => false,
            }
        })
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: "host-derived FDT has no matching interrupt-controller node".into(),
        })?;
    if let Some(phandle) = guest
        .node(controller)
        .and_then(|node| {
            node.get_property("phandle")
                .or_else(|| node.get_property("linux,phandle"))
        })
        .and_then(Property::get_u32)
    {
        return Ok(phandle);
    }

    let phandle = next_phandle(guest);
    guest
        .node_mut(controller)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: "host-derived interrupt controller cannot receive a phandle".into(),
        })?
        .set_property(u32_list_property("phandle", &[phandle]));
    Ok(phandle)
}

fn device_registers(
    device: &ResolvedVirtualDevice,
) -> MachinePlanResult<crate::machine::AddressRange> {
    device
        .mmio()
        .iter()
        .find(|resource| resource.slot().as_str() == "registers")
        .map(|resource| resource.range())
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: format!("virtual UART '{}' has no registers", device.instance_id()),
        })
}

fn device_interrupt(device: &ResolvedVirtualDevice) -> MachinePlanResult<u32> {
    device
        .interrupts()
        .iter()
        .find(|resource| resource.slot().as_str() == "irq")
        .map(|resource| resource.id())
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: format!("virtual UART '{}' has no IRQ", device.instance_id()),
        })
}

fn interrupt_specifier(plan: &VmMachinePlan, intid: u32) -> MachinePlanResult<Vec<u32>> {
    match plan.interrupt_controller() {
        Some(InterruptControllerPlan::Aarch64GicV3(_)) => intid
            .checked_sub(32)
            .map(|spi| vec![0, spi, 4])
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: format!("dynamic AArch64 UART INTID {intid} is not an SPI"),
            }),
        Some(InterruptControllerPlan::RiscvPlic(_)) => Ok(vec![intid]),
        Some(other) => Err(MachinePlanError::InvalidFirmware {
            detail: format!("host-derived FDT cannot encode UART IRQ for {other:?}"),
        }),
        None => Err(MachinePlanError::InvalidFirmware {
            detail: "host-derived FDT has no interrupt controller plan".into(),
        }),
    }
}

fn register_console_alias(
    guest: &mut Fdt,
    node_path: &str,
    console_index: usize,
) -> MachinePlanResult<()> {
    if console_index == 0 && selected_console_targets_node(guest, node_path) {
        return Ok(());
    }
    let aliases = guest
        .get_by_path_id("/aliases")
        .unwrap_or_else(|| guest.add_node(guest.root_id(), Node::new("aliases")));
    let alias = format!("serial{console_index}");
    guest
        .node_mut(aliases)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: "guest /aliases node cannot be updated".into(),
        })?
        .set_property(string_property(&alias, node_path));

    if console_index == 0 {
        let chosen = guest
            .get_by_path_id("/chosen")
            .unwrap_or_else(|| guest.add_node(guest.root_id(), Node::new("chosen")));
        guest
            .node_mut(chosen)
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: "guest /chosen node cannot be updated".into(),
            })?
            .set_property(string_property("stdout-path", "serial0:115200n8"));
    }
    Ok(())
}

fn selected_console_targets_node(guest: &Fdt, node_path: &str) -> bool {
    let Some(reference) = guest
        .get_by_path("/chosen")
        .and_then(|chosen| chosen.as_node().get_property("stdout-path"))
        .and_then(Property::as_str)
        .and_then(|value| value.split(':').next())
    else {
        return false;
    };
    if reference.starts_with('/') {
        return reference == node_path;
    }
    guest
        .get_by_path("/aliases")
        .and_then(|aliases| aliases.as_node().get_property(reference))
        .and_then(Property::as_str)
        == Some(node_path)
}

fn string_property(name: &str, value: &str) -> Property {
    let mut property = Property::new(name, Vec::new());
    property.set_string(value);
    property
}

fn string_list_property(name: &str, values: &[&str]) -> Property {
    let mut bytes = Vec::new();
    for value in values {
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(0);
    }
    Property::new(name, bytes)
}

fn u32_list_property(name: &str, values: &[u32]) -> Property {
    let mut property = Property::new(name, Vec::new());
    property.set_u32_ls(values);
    property
}
