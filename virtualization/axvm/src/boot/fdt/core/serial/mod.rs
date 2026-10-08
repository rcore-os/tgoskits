//! Machine-owned virtual serial description for guest device trees.

use std::{format, string::String, vec, vec::Vec};

use axdevice_base::AccessWidth;
use fdt_edit::{Fdt, Node, Property, RegFixed};
use fdt_raw::RegInfo;

use super::tree::{FdtTree, prop_string};
use crate::{
    AxVmResult, ax_err_type,
    machine::{
        GuestClockReference, GuestGicProfile, GuestMmioRegion, GuestPlicProfile,
        GuestSerialFdtIdentity, GuestSerialFdtInterrupt, GuestSerialFirmwareIdentity,
        GuestSerialModel, GuestSerialProfile, GuestSerialTransport, HostSerialSnapshot,
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
///
/// A malformed or conflicting `/chosen` console selector is a hard error rather
/// than being treated as "no console": callers must not silently overwrite the
/// firmware-selected UART when the selector cannot be parsed.
pub(crate) fn physical_serial_paths(fdt: &Fdt) -> AxVmResult<Vec<String>> {
    let console_path = console_path(fdt)?;
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
    Ok(paths)
}

/// Returns the firmware-selected UART that must remain owned by the host.
pub(crate) fn host_owned_serial_paths(fdt: &Fdt) -> AxVmResult<Vec<String>> {
    console_path(fdt).map(|path| path.into_iter().collect())
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
        console_selection(fdt)?,
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

/// Firmware source that supplied a resolved console contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConsoleSource {
    /// A developer-supplied guest DTB.
    Supplied,
    /// Host firmware.
    Host,
}

/// A resolved console contract together with the controller geometry behind it.
pub(crate) struct ResolvedConsole {
    /// Firmware that supplied this contract.
    pub(crate) source: ConsoleSource,
    /// Resolved serial profile and firmware identity.
    pub(crate) snapshot: HostSerialSnapshot,
    /// GIC description declared by a supplied DTB, retained for validation.
    pub(crate) supplied_gic: Option<GuestGicProfile>,
    /// PLIC description declared by a supplied DTB, retained for validation.
    pub(crate) supplied_plic: Option<GuestPlicProfile>,
}

/// Resolves which firmware source owns the virtual console.
///
/// `provided_dtb` is the developer-supplied guest DTB that was actually loaded;
/// `dtb_load_addr` alone must not be reported here. A supplied DTB whose
/// selected console contract is unusable fails closed instead of falling back.
/// When the supplied DTB declares no console selector at all, the host firmware
/// is consulted, and `None` leaves the machine profile untouched.
pub(crate) fn resolve_console_source(
    current: GuestSerialProfile,
    interrupt_encoding: GuestSerialFdtInterrupt,
    explicit_console: bool,
    provided_dtb: Option<&[u8]>,
    host_dtb: Option<&[u8]>,
) -> AxVmResult<Option<ResolvedConsole>> {
    if explicit_console {
        return Ok(None);
    }
    if let Some(provided) = provided_dtb {
        let fdt = parse_console_fdt(provided, "supplied guest DTB")?;
        if let Some(snapshot) = host_selected_serial(&fdt, current, interrupt_encoding)? {
            let (supplied_gic, supplied_plic) = match interrupt_encoding {
                GuestSerialFdtInterrupt::GicSpi => {
                    (super::interrupt::host_gic_profile(&fdt)?, None)
                }
                GuestSerialFdtInterrupt::PlicSource => {
                    (None, super::interrupt::host_plic_profile(&fdt)?)
                }
            };
            return Ok(Some(ResolvedConsole {
                source: ConsoleSource::Supplied,
                snapshot,
                supplied_gic,
                supplied_plic,
            }));
        }
    }
    let Some(host) = host_dtb else {
        return Ok(None);
    };
    let fdt = parse_console_fdt(host, "host firmware DTB")?;
    let Some(snapshot) = host_selected_serial(&fdt, current, interrupt_encoding)? else {
        return Ok(None);
    };
    Ok(Some(ResolvedConsole {
        source: ConsoleSource::Host,
        snapshot,
        supplied_gic: None,
        supplied_plic: None,
    }))
}

fn parse_console_fdt(bytes: &[u8], owner: &str) -> AxVmResult<Fdt> {
    Fdt::from_bytes(bytes).map_err(|err| {
        ax_err_type!(
            InvalidData,
            format!("Failed to parse {owner} while reading the console selector: {err:#?}")
        )
    })
}

/// Rewrites a selected console's firmware interrupt description to the encoding
/// the prepared per-VM controller actually installs.
///
/// Only the console contract is normalized here; other firmware nodes keep
/// their original interrupt properties.
pub(crate) fn normalize_console_identity(
    identity: &mut GuestSerialFirmwareIdentity,
    interrupt_encoding: GuestSerialFdtInterrupt,
    controller_phandle: Option<u32>,
) {
    let GuestSerialFirmwareIdentity::Fdt(identity) = identity else {
        return;
    };
    if let Some(phandle) = controller_phandle {
        identity.interrupt_parent = phandle;
    }
    let cells = match interrupt_encoding {
        GuestSerialFdtInterrupt::GicSpi => 3,
        GuestSerialFdtInterrupt::PlicSource => 1,
    };
    identity.interrupt_specifier.truncate(cells);
}

/// Keeps `/chosen` Zephyr console phandles pointing at the rebuilt serial node.
const ZEPHYR_CONSOLE_PROPERTIES: [&str; 2] = ["zephyr,console", "zephyr,shell-uart"];

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
    // Resolve the firmware UART layout before any node is replaced or removed.
    // `physical_serial_paths` re-reads the `/chosen` console selector, and the
    // install below deletes the firmware serial node; resolving afterwards would
    // let a still-valid Zephyr phandle selector dangle.
    let mut firmware_uart_paths: Vec<String> = if console {
        let mut paths = physical_serial_paths(tree.inner())?;
        paths.retain(|path| {
            !preserved_physical_selectors
                .iter()
                .any(|selector| super::device::selector_includes_path(selector, path))
        });
        paths.sort_by_key(|path| std::cmp::Reverse(path.matches('/').count()));
        paths
    } else {
        Vec::new()
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

    for path in firmware_uart_paths.drain(..) {
        tree.inner_mut().remove_by_path(&path);
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
    // A `/chosen` Zephyr phandle console names the serial node through a
    // phandle. When the rebuilt node carries no identity yet, mint a fresh one
    // so the surviving selector keeps resolving once the physical node is gone.
    let console_phandle = match node_phandle {
        Some(phandle) => Some(phandle),
        None if console && chosen_uses_zephyr_console_phandle(tree.inner()) => {
            let phandle = next_phandle(tree.inner());
            tree.set_property(serial_id, prop_u32("phandle", phandle))?;
            tree.set_property(serial_id, prop_u32("linux,phandle", phandle))?;
            Some(phandle)
        }
        None => None,
    };

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
    redirect_chosen_zephyr_console(tree, console_phandle, &serial_path)?;
    Ok(())
}

/// Returns whether `/chosen` selects a Zephyr console through a phandle.
fn chosen_uses_zephyr_console_phandle(fdt: &Fdt) -> bool {
    let Some(chosen) = fdt.get_by_path("/chosen") else {
        return false;
    };
    let chosen = chosen.as_node();
    ZEPHYR_CONSOLE_PROPERTIES.iter().copied().any(|name| {
        chosen
            .get_property(name)
            .and_then(Property::get_u32)
            .is_some()
    })
}

/// Redirects surviving `/chosen` Zephyr console selectors at the rebuilt node,
/// preserving each property's original phandle or path-string representation.
fn redirect_chosen_zephyr_console(
    tree: &mut FdtTree,
    phandle: Option<u32>,
    serial_path: &str,
) -> AxVmResult {
    let mut edits = Vec::new();
    if let Some(chosen) = tree.inner().get_by_path("/chosen") {
        let chosen = chosen.as_node();
        for name in ZEPHYR_CONSOLE_PROPERTIES {
            let Some(property) = chosen.get_property(name) else {
                continue;
            };
            if property.get_u32().is_some() {
                if let Some(phandle) = phandle {
                    edits.push((name, Some(phandle)));
                }
            } else if property.as_str().is_some() {
                edits.push((name, None));
            }
        }
    }
    if edits.is_empty() {
        return Ok(());
    }
    let chosen = tree.ensure_path("/chosen")?;
    for (name, phandle) in edits {
        let property = match phandle {
            Some(phandle) => prop_u32(name, phandle),
            None => prop_string(name, serial_path),
        };
        tree.set_property(chosen, property)?;
    }
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

/// One console selector declared in `/chosen` and the serial node it names.
struct SelectedConsole {
    /// Original selector text, or the resolved node path for a phandle property.
    raw: String,
    /// Absolute path of the selected serial node.
    path: String,
}

/// Resolves every console selector that `/chosen` declares.
///
/// Both the standard `stdout-path`/`linux,stdout-path` string selectors and the
/// Zephyr `zephyr,console`/`zephyr,shell-uart` phandle selectors are honored.
/// When several are present they must name the same serial node; a conflicting,
/// dangling, mistyped, or unparsable selector is rejected instead of being
/// silently ignored. Only a `/chosen` with no console selector at all falls
/// back to the legacy earlycon address.
fn console_selection(fdt: &Fdt) -> AxVmResult<Option<(String, String)>> {
    let Some(chosen) = fdt.get_by_path("/chosen") else {
        return Ok(None);
    };
    let chosen = chosen.as_node();
    let mut selected = Vec::new();
    for name in ["stdout-path", "linux,stdout-path"] {
        let Some(property) = chosen.get_property(name) else {
            continue;
        };
        let raw = property.as_str().ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                format!("/chosen {name} is present but is not a string selector")
            )
        })?;
        selected.push(resolve_stdout_selector(fdt, name, raw)?);
    }
    for name in ["zephyr,console", "zephyr,shell-uart"] {
        let Some(property) = chosen.get_property(name) else {
            continue;
        };
        if let Some(phandle) = property.get_u32() {
            selected.push(resolve_phandle_selector(fdt, name, phandle)?);
        } else if let Some(raw) = property.as_str() {
            selected.push(resolve_zephyr_path_selector(fdt, name, raw)?);
        } else {
            return Err(ax_err_type!(
                InvalidData,
                format!("/chosen {name} is present but is neither a phandle nor a path string")
            ));
        }
    }
    let Some(first) = selected.first() else {
        return Ok(earlycon_selection(fdt));
    };
    for candidate in &selected[1..] {
        if candidate.path != first.path {
            return Err(ax_err_type!(
                InvalidData,
                format!(
                    "guest firmware declares conflicting console nodes {} and {}",
                    first.path, candidate.path
                )
            ));
        }
    }
    Ok(Some((first.raw.clone(), first.path.clone())))
}

fn resolve_stdout_selector(fdt: &Fdt, name: &str, raw: &str) -> AxVmResult<SelectedConsole> {
    let selector = raw.split(':').next().unwrap_or(raw);
    let path = if selector.starts_with('/') {
        String::from(selector)
    } else {
        fdt.get_by_path("/aliases")
            .and_then(|aliases| {
                aliases
                    .as_node()
                    .get_property(selector)
                    .and_then(Property::as_str)
                    .map(String::from)
            })
            .ok_or_else(|| {
                ax_err_type!(
                    InvalidData,
                    format!("/chosen {name} selector {raw} does not resolve through /aliases")
                )
            })?
    };
    Ok(SelectedConsole {
        raw: raw.into(),
        path,
    })
}

fn resolve_phandle_selector(fdt: &Fdt, name: &str, phandle: u32) -> AxVmResult<SelectedConsole> {
    let path = phandle_target_path(fdt, phandle).ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            format!("/chosen {name} references missing phandle {phandle:#x}")
        )
    })?;
    Ok(SelectedConsole {
        raw: path.clone(),
        path,
    })
}

/// Resolves a phandle to the path of the live node that declares it.
///
/// `fdt_edit::Fdt::get_by_phandle` answers from a phandle cache populated when
/// the tree was parsed; adding or rewriting a `phandle` property afterwards does
/// not refresh that cache. Console selectors must follow the live properties so
/// a selector that firmware still declares keeps resolving while the tree is
/// patched, instead of dangling against the stale cache.
fn phandle_target_path(fdt: &Fdt, phandle: u32) -> Option<String> {
    fdt.iter_node_ids().find_map(|node_id| {
        let node = fdt.node(node_id)?;
        (node.get_property("phandle").and_then(Property::get_u32) == Some(phandle))
            .then(|| fdt.path_of(node_id))
    })
}

fn resolve_zephyr_path_selector(fdt: &Fdt, name: &str, raw: &str) -> AxVmResult<SelectedConsole> {
    if !raw.starts_with('/') {
        return Err(ax_err_type!(
            InvalidData,
            format!("/chosen {name} path {raw} is not absolute")
        ));
    }
    if fdt.get_by_path(raw).is_none() {
        return Err(ax_err_type!(
            InvalidData,
            format!("/chosen {name} path {raw} does not resolve to a node")
        ));
    }
    Ok(SelectedConsole {
        raw: raw.into(),
        path: raw.into(),
    })
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
    let path = phandle_target_path(fdt, phandle)?;
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

/// Resolves the firmware-selected console node path.
///
/// Malformed or conflicting `/chosen` selectors propagate as errors so the
/// protective callers fail closed instead of dropping the console selection.
fn console_path(fdt: &Fdt) -> AxVmResult<Option<String>> {
    console_selection(fdt).map(|selection| selection.map(|(_, path)| path))
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
