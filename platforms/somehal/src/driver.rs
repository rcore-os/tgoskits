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
        let root = rdrive::probe::acpi::AcpiRoot::new(rsdp, someboot::mem::phys_to_virt);
        let platform = if option_env!("RDRIVE_ACPI_LOAD_AML") == Some("0") {
            info!("Initializing rdrive ACPI without AML loading");
            rdrive::Platform::AcpiWithoutAml(root)
        } else {
            rdrive::Platform::Acpi(root)
        };
        if let Err(err) = rdrive::init(platform) {
            warn!(
                "failed to initialize rdrive with ACPI RSDP {:#x}: {:?}",
                rsdp, err
            );
        }
    } else {
        warn!("No FDT or ACPI RSDP found; skip rdrive initialization");
    }
}
