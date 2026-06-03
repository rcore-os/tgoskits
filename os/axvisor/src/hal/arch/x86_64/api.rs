use axvisor_api::{
    arch::{ArchIf, CacheOp},
    memory::VirtAddr,
    types::InterruptVector,
};

struct ArchImpl;

#[axvisor_api::api_impl]
impl ArchIf for ArchImpl {
    fn inject_virtual_interrupt(vector: InterruptVector) {
        crate::hal::arch::inject_interrupt(vector as u8);
    }

    fn dcache_range(op: CacheOp, addr: VirtAddr, size: usize) {
        crate::hal::arch::cache::dcache_range(op, addr, size);
    }

    fn host_tsc_frequency_mhz() -> Option<u32> {
        u32::try_from(ax_hal::time::nanos_to_ticks(1_000))
            .ok()
            .filter(|&freq| freq > 0)
    }
}
