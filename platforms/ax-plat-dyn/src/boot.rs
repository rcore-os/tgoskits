use ax_memory_addr::VirtAddr;
use somehal::{
    KernelOp,
    setup::{MapError, MmioAddr, MmioOp, MmioRaw},
};

#[somehal::entry(Kernel)]
fn main() -> ! {
    let mut args = 0;
    if let Some(fdt) = somehal::fdt_addr_phys() {
        args = fdt;
    }

    ax_plat::call_main(somehal::smp::cpu_idx(), args)
}

#[somehal::secondary_entry]
fn secondary_main() {
    #[cfg(feature = "smp")]
    ax_plat::call_secondary_main(meta.cpu_idx);
}

pub fn boot_stack_bounds(cpu_id: usize) -> (VirtAddr, usize) {
    let meta = somehal::smp::cpu_meta(cpu_id)
        .unwrap_or_else(|| panic!("missing somehal PerCpuMeta for cpu_id {cpu_id}"));
    let stack_size = somehal::mem::stack_size();
    (VirtAddr::from(meta.stack_top_virt - stack_size), stack_size)
}

pub struct Kernel;

impl KernelOp for Kernel {}

impl MmioOp for Kernel {
    fn ioremap(&self, addr: MmioAddr, size: usize) -> Result<MmioRaw, MapError> {
        axklib::mmio::op().ioremap(addr, size)
    }

    fn iounmap(&self, mmio: &MmioRaw) {
        axklib::mmio::op().iounmap(mmio);
    }
}
