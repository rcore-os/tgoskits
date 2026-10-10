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
    preserved_physical_selectors: &[String],
) -> AxVmResult {
    let machine = crate::machine::current_machine_profile(1);
    let GuestSerialTransport::Mmio { .. } = profile.transport else {
        return Ok(());
    };
    let Some(interrupt_encoding) = machine.serial_fdt_interrupt else {
        return Ok(());
    };
    install_mmio_serial_preserving(
        tree,
        profile,
        interrupt_encoding,
        identity,
        true,
        preserved_physical_selectors,
    )
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

/// Returns every physical UART node described by the firmware.
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

/// Returns the firmware-selected UART that must remain owned by the host.
pub(crate) fn host_owned_serial_paths(fdt: &Fdt) -> Vec<String> {
    console_path(fdt).into_iter().collect()
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
    resolve_selected_serial(
        fdt,
        fallback,
        interrupt_encoding,
        console_selection(fdt),
        "host",
    )
    .map(|resolved| {
        resolved.map(|resolved| HostSerialSnapshot {
            profile: resolved.profile,
            identity: GuestSerialFirmwareIdentity::Fdt(resolved.identity),
        })
    })
}

/// Selects the UART resources compiled into explicit guest firmware.
pub(super) fn select_guest_serial(
    fallback: GuestSerialProfile,
    provided_dtb: Option<&[u8]>,
    passthrough: bool,
    interrupt_encoding: GuestSerialFdtInterrupt,
) -> AxVmResult<Option<SelectedFdtSerial>> {
    let Some(bytes) = provided_dtb.filter(|_| !passthrough) else {
        return Ok(None);
    };
    let fdt = Fdt::from_bytes(bytes).map_err(|error| {
        ax_err_type!(
            InvalidData,
            format!("Invalid explicit guest DTB while resolving its UART: {error:?}")
        )
    })?;
    resolve_selected_serial(
        &fdt,
        fallback,
        interrupt_encoding,
        guest_console_selection(&fdt),
        "explicit guest",
    )
}

pub(super) struct SelectedFdtSerial {
    pub(super) profile: GuestSerialProfile,
    pub(super) identity: GuestSerialFdtIdentity,
}

fn resolve_selected_serial(
    fdt: &Fdt,
    fallback: GuestSerialProfile,
    interrupt_encoding: GuestSerialFdtInterrupt,
    selection: Option<(String, String)>,
    firmware: &'static str,
) -> AxVmResult<Option<SelectedFdtSerial>> {
    let Some((stdout_selector, path)) = selection else {
        return Ok(None);
    };
    let serial = fdt.get_by_path(&path).ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            format!("{firmware} console UART node {path} is missing")
        )
    })?;
    let node = serial.as_node();
    let compatibles = node.compatibles().collect::<Vec<_>>();
    let model = serial_model(node).ok_or_else(|| {
        ax_err_type!(
            Unsupported,
            format!(
                "{firmware} console UART node {path} has no supported virtual register model: \
                 {compatibles:?}"
            )
        )
    })?;

    let reg = serial.regs().into_iter().next().ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            format!("{firmware} console UART node {path} has no register range")
        )
    })?;
    let base = usize::try_from(reg.address).map_err(|_| {
        ax_err_type!(
            InvalidData,
            format!(
                "{firmware} console UART address does not fit usize: {:#x}",
                reg.address
            )
        )
    })?;
    let length = reg
        .size
        .ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                format!("{firmware} console UART node {path} has no register range size")
            )
        })
        .and_then(|length| {
            usize::try_from(length).map_err(|_| {
                ax_err_type!(
                    InvalidData,
                    format!("{firmware} console UART range size does not fit usize: {length:#x}")
                )
            })
        })?;
    if length == 0 {
        return Err(ax_err_type!(
            InvalidData,
            format!("{firmware} console UART node {path} has an empty register range")
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
                    format!("{firmware} console UART reg-shift {shift} is too large")
                ));
            }
            let register_width = node
                .get_property("reg-io-width")
                .and_then(Property::get_u32)
                .map_or(Ok(AccessWidth::Byte), |width| {
                    AccessWidth::try_from(width as usize).map_err(|_| {
                        ax_err_type!(
                            InvalidData,
                            format!("{firmware} console UART reg-io-width {width} is unsupported")
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
            format!("{firmware} console UART node {path} has no interrupt")
        )
    })?;
    validate_interrupt_parent(
        fdt,
        &path,
        interrupt.interrupt_parent.raw(),
        interrupt_encoding,
        firmware,
    )?;
    let irq = decode_interrupt_id(&path, interrupt_encoding, &interrupt.specifier, firmware)?;
    let node_phandle = node
        .get_property("phandle")
        .or_else(|| node.get_property("linux,phandle"))
        .and_then(Property::get_u32);
    let clock_references = serial_clock_references(fdt, node, &path, firmware)?;

    Ok(Some(SelectedFdtSerial {
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
        identity: GuestSerialFdtIdentity {
            node_path: path,
            node_phandle,
            interrupt_parent: interrupt.interrupt_parent.raw(),
            interrupt_specifier: interrupt.specifier,
            stdout_path: stdout_selector,
            clock_references,
        },
    }))
}

fn validate_interrupt_parent(
    fdt: &Fdt,
    path: &str,
    parent_phandle: u32,
    encoding: GuestSerialFdtInterrupt,
    firmware: &'static str,
) -> AxVmResult {
    let parent = fdt.get_by_phandle(parent_phandle.into()).ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            format!(
                "{firmware} console UART node {path} references missing interrupt controller \
                 {parent_phandle:#x}"
            )
        )
    })?;
    let controller = match encoding {
        GuestSerialFdtInterrupt::GicSpi => "GIC",
        GuestSerialFdtInterrupt::PlicSource => "PLIC",
    };
    if !is_supported_interrupt_controller(parent.as_node(), encoding) {
        return Err(ax_err_type!(
            Unsupported,
            format!(
                "{firmware} console UART node {path} is not directly connected to a supported \
                 {controller}"
            )
        ));
    }

    let selected = fdt
        .iter_node_ids()
        .find(|node_id| {
            fdt.node(*node_id)
                .is_some_and(|node| is_supported_interrupt_controller(node, encoding))
        })
        .and_then(|node_id| fdt.node(node_id))
        .ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                format!("{firmware} firmware has no supported {controller}")
            )
        })?;
    let selected_phandle = selected
        .get_property("phandle")
        .or_else(|| selected.get_property("linux,phandle"))
        .and_then(Property::get_u32)
        .ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                format!("selected {firmware} {controller} has no phandle")
            )
        })?;
    if parent_phandle != selected_phandle {
        return Err(ax_err_type!(
            Unsupported,
            format!(
                "{firmware} console UART node {path} is not directly connected to the selected \
                 {controller}"
            )
        ));
    }
    Ok(())
}

fn is_supported_interrupt_controller(node: &Node, encoding: GuestSerialFdtInterrupt) -> bool {
    node.get_property("interrupt-controller").is_some()
        && node.compatibles().any(|compatible| match encoding {
            GuestSerialFdtInterrupt::GicSpi => matches!(
                compatible,
                "arm,gic-v3" | "arm,cortex-a15-gic" | "arm,gic-400"
            ),
            GuestSerialFdtInterrupt::PlicSource => compatible.contains("plic"),
        })
}

fn serial_clock_references(
    fdt: &Fdt,
    serial: &Node,
    serial_path: &str,
    firmware: &'static str,
) -> AxVmResult<Vec<GuestClockReference>> {
    let Some(clocks) = serial.get_property("clocks") else {
        return Ok(Vec::new());
    };
    if clocks.data.is_empty() || !clocks.data.len().is_multiple_of(4) {
        return Err(ax_err_type!(
            InvalidData,
            format!("{firmware} console UART node {serial_path} has a malformed clocks property")
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
                    "{firmware} console UART node {serial_path} references missing clock provider \
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
                        "{firmware} console UART node {serial_path} has a truncated clock \
                         specifier for provider {provider_path}"
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
    firmware: &'static str,
) -> AxVmResult<usize> {
    let raw = match encoding {
        GuestSerialFdtInterrupt::GicSpi => {
            if specifier.first().copied() != Some(0) {
                return Err(ax_err_type!(
                    Unsupported,
                    format!("{firmware} console UART node {path} is not connected to a GIC SPI")
                ));
            }
            specifier
                .get(1)
                .copied()
                .and_then(|source| source.checked_add(32))
                .ok_or_else(|| {
                    ax_err_type!(
                        InvalidData,
                        format!("{firmware} console UART node {path} has an invalid GIC interrupt")
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
                    format!("{firmware} console UART node {path} has an invalid PLIC interrupt")
                )
            })?,
    };
    usize::try_from(raw).map_err(|_| {
        ax_err_type!(
            InvalidData,
            format!("{firmware} console UART interrupt does not fit usize: {raw}")
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
    install_mmio_serial_preserving(tree, profile, interrupt_encoding, identity, console, &[])
}

fn install_mmio_serial_preserving(
    tree: &mut FdtTree,
    profile: GuestSerialProfile,
    interrupt_encoding: GuestSerialFdtInterrupt,
    identity: Option<&GuestSerialFdtIdentity>,
    console: bool,
    preserved_physical_selectors: &[String],
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
    // Firmware phandles are local to each tree; the host identity cannot
    // select the controller after explicit guest firmware replaces it.
    let interrupt_parent = interrupt_controller_phandle(tree, interrupt_encoding)?;

    let serial_path = match identity {
        Some(identity) => identity.node_path.clone(),
        None => match profile.model {
            GuestSerialModel::Pl011 => format!("/pl011@{base:x}"),
            GuestSerialModel::Uart16550 => format!("/serial@{base:x}"),
        },
    };
    let node_phandle = tree.replacement_phandle(
        &serial_path,
        identity.and_then(|identity| identity.node_phandle),
    )?;

    if console {
        let mut old_paths = physical_serial_paths(tree.inner());
        old_paths.retain(|path| {
            !preserved_physical_selectors
                .iter()
                .any(|selector| super::device::selector_includes_path(selector, path))
        });
        old_paths.sort_by_key(|path| std::cmp::Reverse(path.matches('/').count()));
        for path in old_paths {
            tree.inner_mut().remove_by_path(&path);
        }
    }

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
    if let Some(phandle) = node_phandle {
        tree.set_property(serial_id, prop_u32("phandle", phandle))?;
        tree.set_property(serial_id, prop_u32("linux,phandle", phandle))?;
    }
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
    let interrupts = serial_interrupt_specifier(
        tree,
        interrupt_parent,
        profile,
        interrupt_encoding,
        identity,
    )?;
    tree.set_property(serial_id, prop_u32_list("interrupts", &interrupts))?;

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

fn serial_interrupt_specifier(
    tree: &FdtTree,
    interrupt_parent: u32,
    profile: GuestSerialProfile,
    interrupt_encoding: GuestSerialFdtInterrupt,
    identity: Option<&GuestSerialFdtIdentity>,
) -> AxVmResult<Vec<u32>> {
    let expected_cells = tree.interrupt_cells(interrupt_parent)?;
    let mut specifier = match identity {
        Some(identity) => identity.interrupt_specifier.clone(),
        None => match interrupt_encoding {
            GuestSerialFdtInterrupt::GicSpi => {
                let spi = profile.irq.checked_sub(32).ok_or_else(|| {
                    ax_err_type!(InvalidData, "virtual UART interrupt ID is not a GIC SPI")
                })?;
                vec![0, spi as u32, 4]
            }
            GuestSerialFdtInterrupt::PlicSource => vec![profile.irq as u32],
        },
    };
    if interrupt_encoding == GuestSerialFdtInterrupt::GicSpi
        && let Some(flags) = specifier.get_mut(2)
    {
        // The emulated UART owns a level-high wired input regardless of how
        // the source firmware described the physical UART signal.
        *flags = 4;
    }
    match (interrupt_encoding, specifier.len(), expected_cells) {
        (_, actual, expected) if actual == expected => Ok(specifier),
        (GuestSerialFdtInterrupt::GicSpi, 3, 4) => {
            // Zephyr's fourth GIC cell is the interrupt priority. Use its
            // default value when adapting a three-cell source.
            specifier.push(0);
            Ok(specifier)
        }
        (GuestSerialFdtInterrupt::GicSpi, 4, 3) => {
            specifier.truncate(3);
            Ok(specifier)
        }
        _ => Err(ax_err_type!(
            InvalidData,
            format!(
                "virtual UART interrupt has {} cells but its controller requires {expected_cells}",
                specifier.len()
            )
        )),
    }
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
    let phandle = match tree.replacement_phandle(&clock_path, None)? {
        Some(phandle) => phandle,
        None => tree.allocate_phandle()?,
    };
    tree.inner_mut().remove_by_path(&clock_path);
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
    let controller = super::interrupt::controller_node(
        tree,
        |node| {
            node.get_property("interrupt-controller").is_some()
                && node.compatibles().any(|compatible| match encoding {
                    GuestSerialFdtInterrupt::GicSpi => matches!(
                        compatible,
                        "arm,gic-v3" | "arm,gic-400" | "arm,cortex-a15-gic"
                    ),
                    GuestSerialFdtInterrupt::PlicSource => {
                        matches!(compatible, "riscv,plic0" | "sifive,plic-1.0.0")
                    }
                })
        },
        "machine interrupt",
    )?;

    if let Some(phandle) = tree.node_phandle(controller)? {
        return Ok(phandle);
    }

    let phandle = tree.allocate_phandle()?;
    tree.set_property(controller, prop_u32("phandle", phandle))?;
    tree.set_property(controller, prop_u32("linux,phandle", phandle))?;
    Ok(phandle)
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

fn guest_console_selection(fdt: &Fdt) -> Option<(String, String)> {
    stdout_selection(fdt)
        .or_else(|| zephyr_console_selection(fdt))
        .or_else(|| earlycon_selection(fdt))
}

fn zephyr_console_selection(fdt: &Fdt) -> Option<(String, String)> {
    let chosen = fdt.get_by_path("/chosen")?;
    let phandle = ["zephyr,console", "zephyr,shell-uart"]
        .into_iter()
        .find_map(|name| chosen.as_node().get_property(name)?.get_u32())?;
    let path = fdt.get_by_phandle(phandle.into())?.path();
    Some((path.clone(), path))
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
