#[cfg(not(feature = "dyn-plat"))]
compile_error!("riscv64 Axvisor requires the dyn-plat feature");

use axvisor_api::{
    arch::{ArchIf, CacheOp},
    memory::VirtAddr,
    types::InterruptVector,
};

pub(super) fn init_platform_irq_injector() {
    axplat_dyn::register_virtual_irq_injector(crate::hal::arch::inject_interrupt);
}

struct ArchImpl;

#[axvisor_api::api_impl]
impl ArchIf for ArchImpl {
    fn inject_virtual_interrupt(vector: InterruptVector) {
        crate::hal::arch::inject_interrupt(vector.into());
    }

    fn dcache_range(op: CacheOp, addr: VirtAddr, size: usize) {
        crate::hal::arch::cache::dcache_range(op, addr, size);
    }
}
