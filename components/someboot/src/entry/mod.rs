use page_table_generic::{PhysAddr, VirtAddr};

use crate::{ArchTrait, smp::PerCpuMeta};

pub struct PrimaryCpuInitInfo {
    pub kernel_start: PhysAddr,
    pub kernel_end: PhysAddr,
    pub kernel_start_link: VirtAddr,
}

pub fn primary_init_early(params: PrimaryCpuInitInfo) {
    crate::mem::setup_entry(
        params.kernel_start,
        params.kernel_end,
        params.kernel_start_link,
    );

    crate::fdt::setup_earlycon();
    let _ = crate::acpi::earlycon::acpi_setup_earlycon();

    #[cfg(efi)]
    crate::efi_stub::exit_boot_services();

    if let Some(cmdline) = crate::cmdline::cmdline() {
        println!("{cmdline}");
    }
    println!("VM Load @{:#x}", params.kernel_start);
    println!("VM Load Offset: {:#x}", crate::mem::vm_load_offset());

    crate::mem::early_init();
}

pub(crate) fn secondary_entry(cpu_meta: &PerCpuMeta) {
    crate::arch::Arch::per_cpu_trap_init(false);
    let cpu_meta = unsafe {
        let phys = cpu_meta as *const _ as usize;
        let virt = crate::mem::phys_to_virt(phys);
        &*(virt as *const crate::smp::PerCpuMeta)
    };

    unsafe extern "Rust" {
        fn __someboot_secondary(cpu_meta: &PerCpuMeta);
    }
    unsafe { __someboot_secondary(cpu_meta) };
}
