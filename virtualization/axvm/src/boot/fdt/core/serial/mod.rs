//! Machine-owned virtual serial description for guest device trees.

use std::{format, string::String, vec, vec::Vec};

use axdevice_base::AccessWidth;
use fdt_edit::{Fdt, Node, Property, RegFixed};
use fdt_raw::RegInfo;

use super::tree::{FdtTree, prop_string};
use crate::{
    AxVmResult, ax_err_type,
    machine::{
        GuestClockReference, GuestMmioRegion, GuestSerialFdtIdentity, GuestSerialFdtInterrupt,
        GuestSerialFirmwareIdentity, GuestSerialModel, GuestSerialProfile, GuestSerialTransport,
        HostSerialSnapshot,
    },
};

/// Replaces firmware-provided UARTs with the current machine's virtual UART.
pub(crate) fn install_machine_serial(
    tree: &mut FdtTree,
    profile: GuestSerialProfile,
    identity: Option<&GuestSerialFdtIdentity>,
) -> AxVmResult {
    let machine = crate::machine::current_machine_profile(1);
    let GuestSerialTransport::Mmio { .. } = profile.transport else {
        return Ok(());
    };
    let Some(interrupt_encoding) = machine.serial_fdt_interrupt else {
        return Ok(());
    };
    install_mmio_serial(tree, profile, interrupt_encoding, identity, true)
}

/// Adds a non-console virtual UART without changing aliases or stdout-path.
pub(crate) fn install_additional_serial(
    tree: &mut FdtTree,
    profile: GuestSerialProfile,
) -> AxVmResult {
    let machine = crate::machine::current_machine_profile(1);
    let GuestSerialTransport::Mmio { .. } = profile.transport else {
        return Ok(());
    };
    let Some(interrupt_encoding) = machine.serial_fdt_interrupt else {
        return Ok(());
    };
    install_mmio_serial(tree, profile, interrupt_encoding, None, false)
}

/// Returns physical UART nodes that must remain owned by the host.
pub(crate) fn physical_serial_paths(fdt: &Fdt) -> Vec<String> {
    let console_path = console_path(fdt);
    let mut paths = fdt
        .iter_node_ids()
        .filter_map(|node_id| {
            let node = fdt.node(node_id)?;
            let path = fdt.path_of(node_id);
            let serial_name = node.name().starts_with("serial@")
                || node.name().starts_with("uart@")
                || node.name().starts_with("pl011@");
            let serial_compatible = node.compatibles().any(|compatible| {
                compatible.contains("uart")
                    || compatible.contains("serial")
                    || compatible == "arm,pl011"
                    || compatible == "ns16550"
                    || compatible == "ns16550a"
            });
            (serial_name || serial_compatible || console_path.as_deref() == Some(path.as_str()))
                .then_some(path)
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

/// Resolves the guest virtual UART identity from the firmware-selected host UART.
///
/// Firmware-backed machines retain the selected host UART's register model and
/// bus layout while replacing the physical device with an emulated UART.
pub(crate) fn host_selected_serial(
    fdt: &Fdt,
    fallback: GuestSerialProfile,
    interrupt_encoding: GuestSerialFdtInterrupt,
) -> AxVmResult<Option<HostSerialSnapshot>> {
    let Some((stdout_selector, path)) = console_selection(fdt) else {
        return Ok(None);
    };
    let serial = fdt.get_by_path(&path).ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            format!("host console UART node {path} is missing")
        )
    })?;
    let node = serial.as_node();
    let compatibles = node.compatibles().collect::<Vec<_>>();
    let model = serial_model(node).ok_or_else(|| {
        ax_err_type!(
            Unsupported,
            format!(
                "host console UART node {path} has no supported virtual register model: \
                 {compatibles:?}"
            )
        )
    })?;

    let reg = serial.regs().into_iter().next().ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            format!("host console UART node {path} has no register range")
        )
    })?;
    let base = usize::try_from(reg.address).map_err(|_| {
        ax_err_type!(
            InvalidData,
            format!(
                "host console UART address does not fit usize: {:#x}",
                reg.address
            )
        )
    })?;
    let length = reg
        .size
        .ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                format!("host console UART node {path} has no register range size")
            )
        })
        .and_then(|length| {
            usize::try_from(length).map_err(|_| {
                ax_err_type!(
                    InvalidData,
                    format!("host console UART range size does not fit usize: {length:#x}")
                )
            })
        })?;
    if length == 0 {
        return Err(ax_err_type!(
            InvalidData,
            format!("host console UART node {path} has an empty register range")
        ));
    }

    let GuestSerialTransport::Mmio { .. } = fallback.transport else {
        return Err(ax_err_type!(
            InvalidData,
            "FDT-backed machine serial profile is not MMIO"
        ));
    };
    let (register_shift, register_width, clock_hz) = match model {
        GuestSerialModel::Pl011 => (0, AccessWidth::Dword, fallback.clock_hz),
        GuestSerialModel::Uart16550 => {
            let shift = node
                .get_property("reg-shift")
                .and_then(Property::get_u32)
                .unwrap_or(0);
            if shift >= usize::BITS {
                return Err(ax_err_type!(
                    InvalidData,
                    format!("host console UART reg-shift {shift} is too large")
                ));
            }
            let register_width = node
                .get_property("reg-io-width")
                .and_then(Property::get_u32)
                .map_or(Ok(AccessWidth::Byte), |width| {
                    AccessWidth::try_from(width as usize).map_err(|_| {
                        ax_err_type!(
                            InvalidData,
                            format!("host console UART reg-io-width {width} is unsupported")
                        )
                    })
                })?;
            let clock_hz = node
                .get_property("clock-frequency")
                .and_then(Property::get_u32)
                .filter(|clock| *clock != 0)
                .unwrap_or(fallback.clock_hz);
            (shift as u8, register_width, clock_hz)
        }
    };
    let interrupt = serial.interrupts().into_iter().next().ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            format!("host console UART node {path} has no interrupt")
        )
    })?;
    let irq = decode_interrupt_id(&path, interrupt_encoding, &interrupt.specifier)?;
    let node_phandle = node
        .get_property("phandle")
        .or_else(|| node.get_property("linux,phandle"))
        .and_then(Property::get_u32);
    let clock_references = serial_clock_references(fdt, node, &path)?;

    Ok(Some(HostSerialSnapshot {
        profile: GuestSerialProfile {
            model,
            transport: GuestSerialTransport::Mmio {
                base,
                length,
                register_shift,
                register_width,
            },
            irq,
            clock_hz,
        },
        identity: GuestSerialFirmwareIdentity::Fdt(GuestSerialFdtIdentity {
            node_path: path,
            node_phandle,
            interrupt_parent: interrupt.interrupt_parent.raw(),
            interrupt_specifier: interrupt.specifier,
            stdout_path: stdout_selector,
            clock_references,
        }),
    }))
}

fn serial_clock_references(
    fdt: &Fdt,
    serial: &Node,
    serial_path: &str,
) -> AxVmResult<Vec<GuestClockReference>> {
    let Some(clocks) = serial.get_property("clocks") else {
        return Ok(Vec::new());
    };
    if clocks.data.is_empty() || !clocks.data.len().is_multiple_of(4) {
        return Err(ax_err_type!(
            InvalidData,
            format!("host console UART node {serial_path} has a malformed clocks property")
        ));
    }

    let cells = clocks.get_u32_iter().collect::<Vec<_>>();
    let mut references = Vec::new();
    let mut index = 0;
    while index < cells.len() {
        let provider_phandle = cells[index];
        let provider = fdt.get_by_phandle(provider_phandle.into()).ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                format!(
                    "host console UART node {serial_path} references missing clock provider \
                     {provider_phandle:#x}"
                )
            )
        })?;
        let provider_path = provider.path();
        let clock_cells = provider
            .as_node()
            .get_property("#clock-cells")
            .and_then(Property::get_u32)
            .ok_or_else(|| {
                ax_err_type!(
                    InvalidData,
                    format!("clock provider {provider_path} has no valid #clock-cells")
                )
            })? as usize;
        let end = index
            .checked_add(1)
            .and_then(|start| start.checked_add(clock_cells))
            .filter(|end| *end <= cells.len())
            .ok_or_else(|| {
                ax_err_type!(
                    InvalidData,
                    format!(
                        "host console UART node {serial_path} has a truncated clock specifier for \
                         provider {provider_path}"
                    )
                )
            })?;
        let provider_regions = provider
            .regs()
            .into_iter()
            .map(|reg| guest_clock_provider_region(&provider_path, reg))
            .collect::<AxVmResult<Vec<_>>>()?;
        references.push(GuestClockReference {
            provider_phandle,
            specifier: cells[index + 1..end].to_vec(),
            provider_regions,
        });
        index = end;
    }
    Ok(references)
}

fn guest_clock_provider_region(provider_path: &str, reg: RegFixed) -> AxVmResult<GuestMmioRegion> {
    let base = usize::try_from(reg.address).map_err(|_| {
        ax_err_type!(
            InvalidData,
            format!("clock provider {provider_path} address does not fit usize")
        )
    })?;
    let length = reg
        .size
        .ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                format!("clock provider {provider_path} register range has no size")
            )
        })
        .and_then(|length| {
            usize::try_from(length).map_err(|_| {
                ax_err_type!(
                    InvalidData,
                    format!("clock provider {provider_path} range size does not fit usize")
                )
            })
        })?;
    if length == 0 {
        return Err(ax_err_type!(
            InvalidData,
            format!("clock provider {provider_path} register range is empty")
        ));
    }
    Ok(GuestMmioRegion { base, length })
}

fn decode_interrupt_id(
    path: &str,
    encoding: GuestSerialFdtInterrupt,
    specifier: &[u32],
) -> AxVmResult<usize> {
    let raw = match encoding {
        GuestSerialFdtInterrupt::GicSpi => {
            if specifier.first().copied() != Some(0) {
                return Err(ax_err_type!(
                    Unsupported,
                    format!("host console UART node {path} is not connected to a GIC SPI")
                ));
            }
            specifier
                .get(1)
                .copied()
                .and_then(|source| source.checked_add(32))
                .ok_or_else(|| {
                    ax_err_type!(
                        InvalidData,
                        format!("host console UART node {path} has an invalid GIC interrupt")
                    )
                })?
        }
        GuestSerialFdtInterrupt::PlicSource => specifier
            .first()
            .copied()
            .filter(|source| *source != 0)
            .ok_or_else(|| {
                ax_err_type!(
                    InvalidData,
                    format!("host console UART node {path} has an invalid PLIC interrupt")
                )
            })?,
    };
    usize::try_from(raw).map_err(|_| {
        ax_err_type!(
            InvalidData,
            format!("host console UART interrupt does not fit usize: {raw}")
        )
    })
}

fn install_mmio_serial(
    tree: &mut FdtTree,
    profile: GuestSerialProfile,
    interrupt_encoding: GuestSerialFdtInterrupt,
    identity: Option<&GuestSerialFdtIdentity>,
    console: bool,
) -> AxVmResult {
    let GuestSerialTransport::Mmio {
        base,
        length,
        register_shift,
        register_width,
    } = profile.transport
    else {
        return Err(ax_err_type!(
            InvalidData,
            "device-tree serial profile is not MMIO"
        ));
    };
    let interrupt_parent = match identity {
        Some(identity) => identity.interrupt_parent,
        None => interrupt_controller_phandle(tree, interrupt_encoding)?,
    };

    if console {
        let mut old_paths = physical_serial_paths(tree.inner());
        old_paths.sort_by_key(|path| std::cmp::Reverse(path.matches('/').count()));
        for path in old_paths {
            tree.inner_mut().remove_by_path(&path);
        }
    }

    let serial_path = match identity {
        Some(identity) => identity.node_path.clone(),
        None => match profile.model {
            GuestSerialModel::Pl011 => format!("/pl011@{base:x}"),
            GuestSerialModel::Uart16550 => format!("/serial@{base:x}"),
        },
    };
    let (parent_path, node_name) = serial_path.rsplit_once('/').ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            format!("virtual serial node path is not absolute: {serial_path}")
        )
    })?;
    let parent = if parent_path.is_empty() {
        tree.inner().root_id()
    } else {
        tree.ensure_path(parent_path)?
    };
    let serial_id = tree.add_node(parent, Node::new(node_name));
    tree.inner_mut()
        .view_typed_mut(serial_id)
        .ok_or_else(|| ax_err_type!(InvalidData, "new serial FDT node is missing"))?
        .set_regs(&[RegInfo::new(base as u64, Some(length as u64))]);

    match profile.model {
        GuestSerialModel::Pl011 => {
            let clock = install_pl011_clock(tree, profile.clock_hz, base, console)?;
            tree.set_property(
                serial_id,
                prop_string_list("compatible", &["arm,pl011", "arm,primecell"]),
            )?;
            tree.set_property(serial_id, prop_u32_list("clocks", &[clock, clock]))?;
            tree.set_property(
                serial_id,
                prop_string_list("clock-names", &["uartclk", "apb_pclk"]),
            )?;
        }
        GuestSerialModel::Uart16550 => {
            tree.set_property(serial_id, prop_string("compatible", "ns16550a"))?;
            tree.set_property(serial_id, prop_u32("reg-shift", u32::from(register_shift)))?;
            tree.set_property(
                serial_id,
                prop_u32("reg-io-width", register_width.size() as u32),
            )?;
        }
    }
    tree.set_property(serial_id, prop_u32("clock-frequency", profile.clock_hz))?;
    tree.set_property(serial_id, prop_u32("current-speed", 115_200))?;
    tree.set_property(serial_id, prop_u32("interrupt-parent", interrupt_parent))?;
    let interrupts = match identity {
        Some(identity) => prop_u32_list("interrupts", &identity.interrupt_specifier),
        None => match interrupt_encoding {
            GuestSerialFdtInterrupt::GicSpi => {
                let spi = profile.irq.checked_sub(32).ok_or_else(|| {
                    ax_err_type!(InvalidData, "PL011 interrupt ID is not a GIC SPI")
                })?;
                prop_u32_list("interrupts", &[0, spi as u32, 4])
            }
            GuestSerialFdtInterrupt::PlicSource => {
                prop_u32_list("interrupts", &[profile.irq as u32])
            }
        },
    };
    tree.set_property(serial_id, interrupts)?;
    if let Some(phandle) = identity.and_then(|identity| identity.node_phandle) {
        tree.set_property(serial_id, prop_u32("phandle", phandle))?;
        tree.set_property(serial_id, prop_u32("linux,phandle", phandle))?;
    }

    if console && identity.is_none() {
        let aliases = tree.ensure_path("/aliases")?;
        tree.set_property(aliases, prop_string("serial0", &serial_path))?;
    }
    if !console {
        return Ok(());
    }
    let chosen = tree.ensure_path("/chosen")?;
    let stdout_path = identity
        .map(|identity| identity.stdout_path.as_str())
        .unwrap_or(&serial_path);
    let stdout_selector = stdout_path.split(':').next().unwrap_or(stdout_path);
    if !stdout_selector.starts_with('/') {
        let aliases = tree.ensure_path("/aliases")?;
        tree.set_property(aliases, prop_string(stdout_selector, &serial_path))?;
    }
    tree.set_property(chosen, prop_string("stdout-path", stdout_path))?;
    Ok(())
}

fn install_pl011_clock(
    tree: &mut FdtTree,
    clock_hz: u32,
    serial_base: usize,
    console: bool,
) -> AxVmResult<u32> {
    let node_name = if console {
        "vuart-clock".into()
    } else {
        format!("vuart-clock@{serial_base:x}")
    };
    let clock_path = format!("/{node_name}");
    tree.inner_mut().remove_by_path(&clock_path);
    let phandle = next_phandle(tree.inner());
    let clock = tree.add_node(tree.inner().root_id(), Node::new(&node_name));

    tree.set_property(clock, prop_string("compatible", "fixed-clock"))?;
    tree.set_property(clock, prop_u32("#clock-cells", 0))?;
    tree.set_property(clock, prop_u32("clock-frequency", clock_hz))?;
    tree.set_property(
        clock,
        prop_string("clock-output-names", "virtual-uart-clock"),
    )?;
    tree.set_property(clock, prop_u32("phandle", phandle))?;
    tree.set_property(clock, prop_u32("linux,phandle", phandle))?;
    Ok(phandle)
}

pub(super) fn interrupt_controller_phandle(
    tree: &mut FdtTree,
    encoding: GuestSerialFdtInterrupt,
) -> AxVmResult<u32> {
    let controller = tree
        .inner()
        .iter_node_ids()
        .find(|node_id| {
            let Some(node) = tree.inner().node(*node_id) else {
                return false;
            };
            if node.get_property("interrupt-controller").is_none() {
                return false;
            }
            node.compatibles().any(|compatible| match encoding {
                GuestSerialFdtInterrupt::GicSpi => compatible.contains("gic"),
                GuestSerialFdtInterrupt::PlicSource => compatible.contains("plic"),
            })
        })
        .ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                "guest FDT has no interrupt controller for the machine serial port"
            )
        })?;

    if let Some(phandle) = tree
        .inner()
        .node(controller)
        .and_then(|node| {
            node.get_property("phandle")
                .or_else(|| node.get_property("linux,phandle"))
        })
        .and_then(Property::get_u32)
    {
        return Ok(phandle);
    }

    let phandle = next_phandle(tree.inner());
    tree.set_property(controller, prop_u32("phandle", phandle))?;
    tree.set_property(controller, prop_u32("linux,phandle", phandle))?;
    Ok(phandle)
}

fn next_phandle(fdt: &Fdt) -> u32 {
    fdt.iter_node_ids()
        .filter_map(|node_id| {
            let node = fdt.node(node_id)?;
            node.get_property("phandle")
                .or_else(|| node.get_property("linux,phandle"))
                .and_then(Property::get_u32)
        })
        .max()
        .unwrap_or(0)
        .saturating_add(1)
        .max(1)
}

fn stdout_selection(fdt: &Fdt) -> Option<(String, String)> {
    let chosen = fdt.get_by_path("/chosen")?;
    let raw = ["stdout-path", "linux,stdout-path"]
        .into_iter()
        .find_map(|name| chosen.as_node().get_property(name)?.as_str())?;
    let selector = raw.split(':').next().unwrap_or(raw);
    let path = if selector.starts_with('/') {
        selector
    } else {
        fdt.get_by_path("/aliases")?
            .as_node()
            .get_property(selector)?
            .as_str()?
    };
    Some((raw.into(), path.into()))
}

fn console_selection(fdt: &Fdt) -> Option<(String, String)> {
    stdout_selection(fdt).or_else(|| earlycon_selection(fdt))
}

fn earlycon_selection(fdt: &Fdt) -> Option<(String, String)> {
    let bootargs = fdt
        .get_by_path("/chosen")?
        .as_node()
        .get_property("bootargs")?
        .as_str()?;
    let address = bootargs
        .split_ascii_whitespace()
        .filter_map(|argument| argument.strip_prefix("earlycon="))
        .find_map(|configuration| {
            configuration
                .split(',')
                .find_map(parse_earlycon_mmio_address)
        })?;
    let path = fdt.iter_node_ids().find_map(|node_id| {
        let node = fdt.node(node_id)?;
        serial_model(node)?;
        fdt.view_typed(node_id)?
            .regs()
            .into_iter()
            .any(|reg| reg.address == address)
            .then(|| fdt.path_of(node_id))
    })?;
    Some((path.clone(), path))
}

fn parse_earlycon_mmio_address(component: &str) -> Option<u64> {
    let digits = component
        .strip_prefix("0x")
        .or_else(|| component.strip_prefix("0X"))?;
    u64::from_str_radix(digits, 16).ok()
}

fn serial_model(node: &Node) -> Option<GuestSerialModel> {
    let mut uart_16550 = false;
    for compatible in node.compatibles() {
        if compatible == "arm,pl011" {
            return Some(GuestSerialModel::Pl011);
        }
        uart_16550 |= matches!(compatible, "ns16550" | "ns16550a" | "snps,dw-apb-uart");
    }
    uart_16550.then_some(GuestSerialModel::Uart16550)
}

fn console_path(fdt: &Fdt) -> Option<String> {
    console_selection(fdt).map(|(_, path)| path)
}

fn prop_u32(name: &str, value: u32) -> Property {
    prop_u32_list(name, &[value])
}

fn prop_u32_list(name: &str, values: &[u32]) -> Property {
    let mut prop = Property::new(name, vec![]);
    prop.set_u32_ls(values);
    prop
}

fn prop_string_list(name: &str, values: &[&str]) -> Property {
    let mut prop = Property::new(name, vec![]);
    prop.set_string_ls(values);
    prop
}

#[cfg(test)]
mod tests;
