use some_serial::ns16550;

use crate::console::EarlyconInput;

/// Sets up the early console and reports whether an explicit command-line
/// console was selected.
pub(crate) fn setup_earlycon_with_selection() -> bool {
    let _ = super::set_cmdline();

    // RISC-V can use an explicit MMIO console in place of the SBI default.
    #[cfg(target_arch = "riscv64")]
    if crate::console::set_earlycon_by_cmdline().is_ok() {
        return true;
    }

    if <<crate::arch::Arch as crate::ArchTrait>::Console as crate::console::ArchConsoleOps>::init()
    {
        return false;
    }

    if crate::console::set_earlycon_by_cmdline().is_ok() {
        return true;
    }

    let _ = set_by_stdout();
    false
}

fn stdout_registers<'a>(fdt: &fdt_raw::Fdt<'a>) -> Option<(fdt_raw::Node<'a>, usize, Option<u64>)> {
    let chosen = fdt.chosen()?;
    let stdout = split_stdout_options(chosen.stdout_path()?);
    let node = fdt.find_by_path(stdout)?;
    let reg = node.reg()?.next()?;
    let address = <crate::arch::Arch as crate::ArchTrait>::canonicalize_paddr(
        usize::try_from(fdt.translate_address(stdout, reg.address)).ok()?,
    );

    Some((node, address, reg.size))
}

fn set_by_stdout() -> Option<()> {
    let fdt = crate::fdt::fdt_base()?;
    let (node, address, size) = stdout_registers(&fdt)?;

    let clock = node
        .find_property("clock-frequency")
        .and_then(|prop| prop.as_u32())
        .unwrap_or(0);
    for com in node.compatibles() {
        match com {
            "arm,pl011" | "arm,primecell" => {
                crate::console::bind_earlycon_pl011(address, clock, EarlyconInput::ArchFirst)
                    .ok()?;
                return Some(());
            }
            "snps,dw-apb-uart" | "spacemit,k1-uart" | "spacemit,k3-uart" | "intel,xscale-uart"
            | "ns16550a" | "ns16550" => {
                // A generic fallback compatible may precede the XScale one.
                let xscale = node.compatibles().any(|compatible| {
                    matches!(
                        compatible,
                        "spacemit,k1-uart" | "spacemit,k3-uart" | "intel,xscale-uart"
                    )
                });
                let variant = if xscale {
                    ns16550::UartVariant::XScale
                } else {
                    ns16550::UartVariant::Standard
                };
                let layout = ns16550_register_layout(&node, variant)?;
                let size = usize::try_from(size?).ok()?;
                let (address, size) = if com == "snps,dw-apb-uart" {
                    (address, size)
                } else {
                    ns16550_register_range(&node, address, size)?
                };
                // SAFETY: the firmware stdout node supplies the UART range
                // and the compatible register layout for this boot stage.
                unsafe {
                    crate::console::bind_earlycon_ns16550(
                        address,
                        Some(size),
                        clock,
                        layout,
                        variant,
                        EarlyconInput::ArchFirst,
                    )
                }
                .ok()?;
                return Some(());
            }
            _ => {
                continue;
            }
        }
    }
    None
}

fn ns16550_register_range(
    node: &fdt_raw::Node<'_>,
    address: usize,
    size: usize,
) -> Option<(usize, usize)> {
    let offset = match node.find_property("reg-offset") {
        Some(prop) => {
            let bytes = prop.as_slice().try_into().ok()?;
            usize::try_from(u32::from_be_bytes(bytes)).ok()?
        }
        None => 0,
    };
    let size = size.checked_sub(offset).filter(|size| *size != 0)?;
    Some((address.checked_add(offset)?, size))
}

fn ns16550_register_layout(
    node: &fdt_raw::Node<'_>,
    variant: ns16550::UartVariant,
) -> Option<ns16550::MmioLayout> {
    let read_property = |name| match node.find_property(name) {
        Some(prop) => {
            let bytes = prop.as_slice().try_into().ok()?;
            Some(Some(u32::from_be_bytes(bytes)))
        }
        None => Some(None),
    };
    variant
        .mmio_layout(read_property("reg-shift")?, read_property("reg-io-width")?)
        .ok()
}

fn split_stdout_options(path: &str) -> &str {
    path.split_once(':').map_or(path, |(path, _)| path)
}

#[cfg(test)]
mod tests {
    use fdt_edit::{Fdt, Node, Property};

    use super::*;

    #[test]
    fn fdt_uart_register_range_applies_offset_within_resource() {
        let mut fdt = Fdt::new();
        let serial = fdt.add_node(fdt.root_id(), Node::new("serial@1000"));
        let register_range = |fdt: &Fdt| {
            let bytes = fdt.encode();
            let fdt = fdt_raw::Fdt::from_bytes(bytes.as_ref()).unwrap();
            let node = fdt.find_by_path("/serial@1000").unwrap();
            ns16550_register_range(&node, 0x1000, 0x100)
        };

        assert_eq!(register_range(&fdt), Some((0x1000, 0x100)));

        fdt.node_mut(serial)
            .unwrap()
            .set_property(Property::new("reg-offset", 0x40_u32.to_be_bytes().to_vec()));
        assert_eq!(register_range(&fdt), Some((0x1040, 0xc0)));

        fdt.node_mut(serial)
            .unwrap()
            .set_property(Property::new("reg-offset", vec![0, 0, 0, 0x40, 0]));
        assert_eq!(register_range(&fdt), None);

        fdt.node_mut(serial).unwrap().set_property(Property::new(
            "reg-offset",
            0x100_u32.to_be_bytes().to_vec(),
        ));
        assert_eq!(register_range(&fdt), None);
    }

    #[test]
    fn fdt_uart_register_layout_preserves_legacy_spacing_without_reg_shift() {
        let mut fdt = Fdt::new();
        let serial = fdt.add_node(fdt.root_id(), Node::new("serial@1000"));
        fdt.node_mut(serial)
            .unwrap()
            .set_property(Property::new("reg-io-width", 4_u32.to_be_bytes().to_vec()));
        let layout = |fdt: &Fdt| {
            let bytes = fdt.encode();
            let fdt = fdt_raw::Fdt::from_bytes(bytes.as_ref()).unwrap();
            let node = fdt.find_by_path("/serial@1000").unwrap();
            ns16550_register_layout(&node, ns16550::UartVariant::Standard)
        };

        assert_eq!(layout(&fdt), Some(ns16550::MmioLayout::new(4, 4).unwrap()));

        fdt.node_mut(serial)
            .unwrap()
            .set_property(Property::new("reg-shift", 3_u32.to_be_bytes().to_vec()));
        assert_eq!(layout(&fdt), Some(ns16550::MmioLayout::new(8, 4).unwrap()));

        fdt.node_mut(serial)
            .unwrap()
            .set_property(Property::new("reg-shift", vec![2]));
        assert_eq!(layout(&fdt), None);
    }
}
