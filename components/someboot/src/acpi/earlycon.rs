use core::ptr::NonNull;

use acpi::{AcpiError, Handler, PhysicalMapping, address::AddressSpace, sdt::spcr::Spcr};
use some_serial::ns16550::Ns16550;

use crate::mem::_fixmap_io;

pub(crate) fn acpi_setup_earlycon() -> Result<(), AcpiError> {
    let tb = crate::acpi::tables()?;

    for spsr in tb.find_tables::<Spcr>() {
        if deal_with_spsr(&spsr).is_some() {
            println!("Early console setup complete.");
            break;
        }
    }

    Ok(())
}

fn deal_with_spsr(spsr: &PhysicalMapping<impl Handler, Spcr>) -> Option<()> {
    println!("Found {:?}", spsr.interface_type());

    let base_address = match spsr.base_address()? {
        Ok(addr) => addr,
        Err(e) => {
            println!("Failed to get base address: {:?}", e);
            return None;
        }
    };
    println!("  Base address: {:#x?}", base_address.address);
    println!("  Baud rate: {:?}", spsr.baud_rate());
    println!("  Clock frequency: {:?}", spsr.uart_clock_frequency());

    let mut clock = 0;
    if let Some(freq) = spsr.uart_clock_frequency() {
        clock = freq.into();
    }

    let (vaddr, is_mmio) = match spsr.interface_type() {
        acpi::sdt::spcr::SpcrInterfaceType::Full16550
        | acpi::sdt::spcr::SpcrInterfaceType::Generic16550 => match base_address.address_space {
            AddressSpace::SystemIo => {
                #[cfg(target_arch = "x86_64")]
                {
                    let mut uart = Ns16550::new_port(base_address.address as u16, clock);
                    uart.open();
                    crate::console::set_earlycon_serial(crate::console::EarlySerial::Ns16550Port(
                        uart,
                    ));
                    (None, false)
                }
                #[cfg(not(target_arch = "x86_64"))]
                {
                    println!("SPCR I/O port early console is only supported on x86_64.");
                    return None;
                }
            }
            AddressSpace::SystemMemory => {
                let mapped = _fixmap_io(base_address.address as _);
                let mut uart = Ns16550::new_mmio(
                    NonNull::new(mapped).unwrap(),
                    clock,
                    base_address.access_size as _,
                );
                uart.open();
                crate::console::set_earlycon_serial(crate::console::EarlySerial::Ns16550Mmio(uart));
                (Some(mapped), true)
            }
            space => {
                println!("Unsupported SPCR address space `{space:?}` for early console.");
                return None;
            }
        },
        t => {
            println!("Unsupported SPCR interface type `{t:?}` for early console.");
            return None;
        }
    };

    unsafe {
        crate::console::DEBUG_BASE = base_address.address as usize;
        crate::console::DEBUG_IS_MMIO = is_mmio;
    }

    if let Some(vaddr) = vaddr {
        println!("Early console initialized at vaddr {:#x}", vaddr as usize);
    } else {
        println!(
            "Early console initialized at I/O port {:#x}",
            base_address.address
        );
    }

    Some(())
}
