use core::ptr::NonNull;

use some_serial::*;

use crate::{console::DEBUG_BASE, mem::phys_to_virt};

pub fn setup_earlycon() -> Option<()> {
    let _ = super::set_cmdline();

    if crate::console::set_earlycon_by_cmdline().is_ok() {
        return Some(());
    }

    if set_by_stdout().is_some() {
        return Some(());
    }

    Some(())
}

fn set_by_stdout() -> Option<()> {
    let fdt = crate::fdt::fdt_base()?;

    let chosen = fdt.chosen().ok()?;
    let stdout = chosen.stdout().ok()?;
    let reg = stdout.reg().ok()?.next()?;
    let addr = NonNull::new(phys_to_virt(reg.address as usize))?;
    let clock = stdout.clock_frequency().unwrap_or(0);

    for com in stdout.compatibles_flatten().ok()? {
        match com {
            "arm,pl011" | "arm,primecell" => {
                let mut serial = pl011::Pl011::new(addr, clock);
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
        DEBUG_BASE = addr.as_ptr() as usize;
    }
    Some(())
}
