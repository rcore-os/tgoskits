use axvisor_api::{
    arch::{ArchIf, CacheOp},
    memory::{PhysAddr, VirtAddr},
    types::InterruptVector,
};

struct ArchIfImpl;

#[axvisor_api::api_impl]
impl ArchIf for ArchIfImpl {
    fn inject_virtual_interrupt(vector: InterruptVector) {
        crate::hal::arch::inject_interrupt(vector.into());
    }

    fn dcache_range(op: CacheOp, addr: VirtAddr, size: usize) {
        crate::hal::arch::cache::dcache_range(op, addr, size);
    }

    fn host_fdt_paddr() -> Option<PhysAddr> {
        let bootarg = ax_hal::dtb::get_bootarg();
        (bootarg != 0).then(|| bootarg.into())
    }
}
