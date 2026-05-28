use core::ptr::NonNull;

pub fn rdrive_setup() {
    if let Some(addr) = someboot::fdt_addr() {
        info!("Initializing rdrive with FDT at {:?}", addr);
        rdrive::init(rdrive::Platform::Fdt {
            addr: NonNull::new(addr).unwrap(),
        })
        .unwrap();
    } else if let Some(rsdp) = someboot::rsdp_addr_phys() {
        info!("Initializing rdrive with ACPI RSDP at {:#x}", rsdp);
        if let Err(err) = rdrive::init(rdrive::Platform::Acpi(rdrive::probe::acpi::AcpiRoot {
            rsdp,
        })) {
            warn!(
                "failed to initialize rdrive with ACPI RSDP {:#x}: {:?}",
                rsdp, err
            );
        }
    } else {
        warn!("No FDT or ACPI RSDP found; skip rdrive initialization");
    }
}
