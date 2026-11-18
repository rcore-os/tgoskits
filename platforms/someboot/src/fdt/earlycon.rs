use some_serial::ns16550;

pub fn setup_earlycon() -> Option<()> {
    let _ = super::set_cmdline();

    // An explicit MMIO console must take precedence over the architecture's
    // default console, which is always available through SBI on RISC-V.
    if crate::console::set_earlycon_by_cmdline().is_ok() {
        return Some(());
    }

    if <<crate::arch::Arch as crate::ArchTrait>::Console as crate::console::ArchConsoleOps>::init()
    {
        Some(())
    } else {
        if set_by_stdout().is_some() {
            return Some(());
        }

        Some(())
    }
}

fn set_by_stdout() -> Option<()> {
    let fdt = crate::fdt::fdt_base()?;

    let chosen = fdt.chosen()?;
    let stdout = split_stdout_options(chosen.stdout_path()?);
    let node = fdt.find_by_path(stdout)?;
    let reg = node.reg()?.next()?;
    let address = <crate::arch::Arch as crate::ArchTrait>::canonicalize_paddr(
        usize::try_from(fdt.translate_address(stdout, reg.address)).ok()?,
    );

    let clock = node
        .find_property("clock-frequency")
        .and_then(|prop| prop.as_u32())
        .unwrap_or(0);
    let reg_width = node
        .find_property("reg-io-width")
        .and_then(|prop| prop.as_u32())
        .unwrap_or(1) as usize;

    for com in node.compatibles() {
        match com {
            "arm,pl011" | "arm,primecell" => {
                crate::console::bind_earlycon_pl011(address, clock).ok()?;
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
                let shift = node
                    .find_property("reg-shift")
                    .and_then(|prop| prop.as_u32())
                    .unwrap_or(if xscale { 2 } else { 0 });
                let stride = 1usize.checked_shl(shift)?;
                let size = usize::try_from(reg.size?).ok()?;
                // SAFETY: the firmware stdout node supplies the UART range
                // and the compatible register layout for this boot stage.
                unsafe {
                    crate::console::bind_earlycon_ns16550(
                        address,
                        Some(size),
                        clock,
                        stride,
                        reg_width,
                        variant,
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

fn split_stdout_options(path: &str) -> &str {
    path.split_once(':').map_or(path, |(path, _)| path)
}
