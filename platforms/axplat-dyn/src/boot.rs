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

    let cpu_idx = somehal::smp::early_current_cpu_idx();
    ax_percpu::init_percpu_reg(cpu_idx);
    ax_plat::call_main(cpu_idx, args)
}

#[somehal::secondary_entry]
fn secondary_main() {
    #[cfg(feature = "smp")]
    {
        ax_percpu::init_percpu_reg(meta.cpu_idx);
        ax_plat::call_secondary_main(meta.cpu_idx);
    }
}

pub fn boot_stack_bounds(cpu_idx: usize) -> (VirtAddr, usize) {
    let meta = somehal::smp::cpu_meta(cpu_idx)
        .unwrap_or_else(|| panic!("missing somehal PerCpuMeta for cpu_idx {cpu_idx}"));
    let stack_size = somehal::mem::stack_size();
    (VirtAddr::from(meta.stack_top_virt - stack_size), stack_size)
}

pub struct Kernel;

impl KernelOp for Kernel {
    fn current_cpu_idx(&self) -> Option<usize> {
        Some(ax_plat::percpu::this_cpu_id())
    }
}

impl MmioOp for Kernel {
    fn ioremap(&self, addr: MmioAddr, size: usize) -> Result<MmioRaw, MapError> {
        axklib::mmio::op().ioremap(addr, size)
    }

    fn iounmap(&self, mmio: &MmioRaw) {
        axklib::mmio::op().iounmap(mmio);
    }
}
