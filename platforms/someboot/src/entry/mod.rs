use page_table_generic::{PhysAddr, VirtAddr};

use crate::{ArchTrait, smp::PerCpuMeta};

pub struct PrimaryCpuInitInfo {
    pub kernel_start: PhysAddr,
    pub kernel_end: PhysAddr,
    pub kernel_start_link: VirtAddr,
}

pub fn primary_init_early(params: PrimaryCpuInitInfo) {
    setup_primary_memory(&params);
    setup_primary_console();
    crate::entropy::capture();
    #[cfg(efi)]
    if crate::efi_stub::is_uefi_available() {
        crate::efi_stub::exit_boot_services();
    }
    log_primary_init(&params);
    crate::mem::early_init();
}

pub(crate) fn setup_primary_memory(params: &PrimaryCpuInitInfo) {
    crate::mem::setup_entry(
        params.kernel_start,
        params.kernel_end,
        params.kernel_start_link,
    );
}

pub(crate) fn setup_primary_console() {
    crate::fdt::setup_earlycon();
    let _ = crate::acpi::earlycon::acpi_setup_earlycon();
}

pub(crate) fn log_primary_init(params: &PrimaryCpuInitInfo) {
    if let Some(cmdline) = crate::cmdline::cmdline() {
        println!("{cmdline}");
    }
    println!("VM Load @{:#x}", params.kernel_start);
    println!("VM Load Offset: {:#x}", crate::mem::vm_load_offset());
}

pub(crate) fn secondary_entry(cpu_meta_paddr: usize) {
    crate::arch::Arch::per_cpu_trap_init(false);
    let cpu_meta = unsafe {
        let virt = crate::mem::phys_to_virt(cpu_meta_paddr);
        &*(virt as *const crate::smp::PerCpuMeta)
    };
    crate::smp::synchronize_secondary_boot(cpu_meta.cpu_idx);

    unsafe extern "Rust" {
        fn __someboot_secondary(cpu_meta: &PerCpuMeta);
    }
    unsafe { __someboot_secondary(cpu_meta) };
}
