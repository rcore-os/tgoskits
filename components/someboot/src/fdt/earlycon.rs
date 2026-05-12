use core::ptr::NonNull;

use some_serial::*;

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
    let stdout = chosen.stdout_path()?;
    let node = fdt.find_by_path(stdout)?;
    let reg = node.reg()?.next()?;
    let address = fdt.translate_address(stdout, reg.address);

    let addr = NonNull::new(_fixmap_io(address as usize))?;
    // let clock = stdout.clock_frequency().unwrap_or(0);

    for com in node.compatibles() {
        match com {
            "arm,pl011" | "arm,primecell" => {
                let mut serial = pl011::Pl011::new(addr, 0);
                serial.open();
                let tx = serial.take_tx()?;
                let rx = serial.take_rx()?;

                crate::console::set_earlycon_sender(tx);
                crate::console::set_earlycon_reciever(rx);
                break;
            }
            _ => {
                continue;
            }
        }
    }
    unsafe {
        DEBUG_BASE = address as usize;
        DEBUG_IS_MMIO = true;
    }
    Some(())
}
