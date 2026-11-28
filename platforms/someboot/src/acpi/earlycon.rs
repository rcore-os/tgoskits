use acpi::{AcpiError, address::AddressSpace, sdt::spcr::SpcrInterfaceType};
use kernutil::acpi::{Spcr, spcr_mappings};
#[cfg(target_arch = "x86_64")]
use some_serial::ns16550::Ns16550;
use some_serial::ns16550::{MmioLayout, UartVariant};

#[cfg(target_arch = "x86_64")]
use crate::console::{EarlySerial, EarlySerialRaw};
use crate::{console::EarlyconInput, mem::_fixmap_io};

pub(crate) fn acpi_setup_earlycon(explicit_console_selected: bool) -> Result<bool, AcpiError> {
    // An explicit RISC-V console may name an XScale UART that SPCR describes
    // as a generic 16550, or select a different UART altogether.
    if cfg!(target_arch = "riscv64") && explicit_console_selected {
        return Ok(false);
    }

    let tb = crate::acpi::tables()?;

    // SAFETY: the directory retains valid table headers. The platform checks
    // each complete SPCR extent against retained firmware memory before mapping.
    for mapping in unsafe { spcr_mappings(&tb, super::AcpiHandle, super::spcr_memory_contains) } {
        if let Some(spcr) = mapping.parse()
            && deal_with_spsr(&spcr).is_some()
        {
            println!("Early console setup complete.");
            return Ok(true);
        }
    }

    Ok(false)
}

fn deal_with_spsr(spsr: &Spcr<'_>) -> Option<()> {
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

    let clock = spsr.uart_clock_frequency().unwrap_or(0);

    let vaddr = match spsr.interface_type() {
        SpcrInterfaceType::Full16550
        | SpcrInterfaceType::Full16450
        | SpcrInterfaceType::Generic16550 => match base_address.address_space {
            AddressSpace::SystemIo => {
                #[cfg(target_arch = "x86_64")]
                {
                    let port = spsr.ns16550_io_port()?;
                    let mut uart = Ns16550::new_port(port, clock);
                    uart.open();
                    crate::console::set_earlycon_serial(EarlySerial::new(
                        EarlySerialRaw::Ns16550Port(uart),
                    ));
                    None
                }
                #[cfg(not(target_arch = "x86_64"))]
                {
                    println!("SPCR I/O port early console is only supported on x86_64.");
                    return None;
                }
            }
            AddressSpace::SystemMemory => {
                let (stride, width) = spsr.ns16550_mmio_layout()?;
                let layout = MmioLayout::new(stride, width).ok()?;
                // SPCR identifies a 16550 but provides no resource size.
                let paddr = usize::try_from(base_address.address).ok()?;
                // SAFETY: the firmware SPCR names the console UART and
                // specifies the access width of its mapped registers.
                unsafe {
                    crate::console::bind_earlycon_ns16550(
                        paddr,
                        None,
                        clock,
                        layout,
                        UartVariant::Standard,
                        EarlyconInput::ArchFirst,
                    )
                }
                .ok()?;
                Some(_fixmap_io(paddr))
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
