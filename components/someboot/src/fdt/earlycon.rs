use core::ptr::NonNull;

use some_serial::{ns16550, pl011};

use crate::{
    console::{DEBUG_BASE, DEBUG_IS_MMIO},
    mem::_fixmap_io,
};

pub fn setup_earlycon() -> Option<()> {
    let _ = super::set_cmdline();

    if <<crate::arch::Arch as crate::ArchTrait>::Console as crate::console::ArchConsoleOps>::init()
    {
        Some(())
    } else {
        if crate::console::set_earlycon_by_cmdline().is_ok() {
            return Some(());
        }

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
    let address = fdt.translate_address(stdout, reg.address);

    let addr = NonNull::new(_fixmap_io(address as usize))?;
    let clock = node
        .find_property("clock-frequency")
        .and_then(|prop| prop.as_u32())
        .unwrap_or(0);
    let reg_width = node
        .find_property("reg-io-width")
        .and_then(|prop| prop.as_u32())
        .unwrap_or(1) as usize;

    let mut installed = false;
    for com in node.compatibles() {
        match com {
            "arm,pl011" | "arm,primecell" => {
                let mut serial = pl011::Pl011::new(addr, clock);
                serial.open();
                crate::console::set_earlycon_serial(crate::console::EarlySerial::Pl011(serial));
                installed = true;
                break;
            }
            "snps,dw-apb-uart" | "ns16550a" | "ns16550" => {
                let mut serial = ns16550::Ns16550::new_mmio(addr, clock, reg_width);
                serial.open();
                crate::console::set_earlycon_serial(crate::console::EarlySerial::Ns16550Mmio(
                    serial,
                ));
                installed = true;
                break;
            }
            _ => {
                continue;
            }
        }
    }
    if !installed {
        return None;
    }
    unsafe {
        DEBUG_BASE = address as usize;
        DEBUG_IS_MMIO = true;
    }
    Some(())
}

fn split_stdout_options(path: &str) -> &str {
    path.split_once(':').map_or(path, |(path, _)| path)
}
