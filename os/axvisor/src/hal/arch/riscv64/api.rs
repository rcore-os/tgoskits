#[cfg(not(feature = "dyn-plat"))]
compile_error!("riscv64 Axvisor requires the dyn-plat feature");

use axvisor_api::{arch::ArchIf, memory::PhysAddr};

pub(super) fn init_platform_irq_injector() {
    axplat_dyn::register_virtual_irq_injector(
        axvisor_core::arch::riscv64::inject_current_interrupt,
    );
}

struct ArchImpl;

#[axvisor_api::api_impl]
impl ArchIf for ArchImpl {
    fn host_fdt_paddr() -> Option<PhysAddr> {
        let bootarg = ax_hal::dtb::get_bootarg();
        (bootarg != 0).then(|| bootarg.into())
    }
}
