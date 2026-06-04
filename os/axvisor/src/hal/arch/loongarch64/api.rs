use axvisor_api::{arch::ArchIf, memory::PhysAddr};

struct ArchIfImpl;

#[axvisor_api::api_impl]
impl ArchIf for ArchIfImpl {
    fn host_fdt_paddr() -> Option<PhysAddr> {
        let bootarg = ax_hal::dtb::get_bootarg();
        (bootarg != 0).then(|| bootarg.into())
    }
}
