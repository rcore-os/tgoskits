use core::ptr::NonNull;

fn acpi_system_memory_is_direct_mapped(address: usize, size: usize) -> bool {
    let Some(end) = address.checked_add(size) else {
        return false;
    };
    someboot::mem::memory_map().iter().any(|region| {
        let region_end = region.physical_start.saturating_add(region.size_in_bytes);
        address >= region.physical_start && end <= region_end
    })
}

fn init_firmware_source(source: rdrive::PlatformSource) -> Result<(), rdrive::error::DriverError> {
    #[cfg(target_arch = "x86_64")]
    {
        rdrive::init_sources(&[rdrive::PlatformSource::Static, source])
    }

    #[cfg(not(target_arch = "x86_64"))]
    {
        rdrive::init_sources(&[source])
    }
}

pub fn rdrive_setup() {
    if let Some(addr) = someboot::fdt_addr() {
        info!("Initializing rdrive with FDT at {:?}", addr);
        init_firmware_source(rdrive::PlatformSource::Fdt(NonNull::new(addr).unwrap())).unwrap();
    } else if let Some(rsdp) = someboot::rsdp_addr_phys() {
        info!("Initializing rdrive with ACPI RSDP at {:#x}", rsdp);
        let root = rdrive::probe::acpi::AcpiRoot::with_direct_mapping(
            rsdp,
            someboot::mem::phys_to_virt,
            acpi_system_memory_is_direct_mapped,
        );
        let firmware_source = if option_env!("RDRIVE_ACPI_LOAD_AML") == Some("0") {
            info!("Initializing rdrive ACPI without AML loading");
            rdrive::PlatformSource::AcpiWithoutAml(root)
        } else {
            rdrive::PlatformSource::Acpi(root)
        };
        if let Err(err) = init_firmware_source(firmware_source) {
            warn!(
                "failed to initialize rdrive with ACPI RSDP {:#x}: {:?}",
                rsdp, err
            );
        }
    } else {
        warn!("No FDT or ACPI RSDP found; skip rdrive initialization");
    }
}
