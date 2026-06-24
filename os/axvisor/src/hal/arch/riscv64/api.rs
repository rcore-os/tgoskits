#[cfg(not(feature = "dyn-plat"))]
compile_error!("riscv64 Axvisor requires the dyn-plat feature");

use axvisor_api::{arch::ArchIf, memory::PhysAddr};

struct ArchImpl;

#[axvisor_api::api_impl]
impl ArchIf for ArchImpl {
    fn host_fdt_paddr() -> Option<PhysAddr> {
        let bootarg = ax_hal::dtb::get_bootarg();
        (bootarg != 0).then(|| bootarg.into())
    }

    fn remote_hfence_vvma_all() {
        axvisor_core::arch::riscv64::hfence_vvma_all();
    }
}
