use ax_memory_addr::VirtAddr;
use somehal::{
    KernelOp,
    setup::{CpuBindError, CpuBindingV1, MapError, MmioAddr, MmioOp, MmioRaw},
};

#[somehal::entry(Kernel)]
fn main() -> ! {
    let mut args = 0;
    if let Some(fdt) = somehal::fdt_addr_phys() {
        args = fdt;
    }

    let cpu_idx = somehal::smp::early_current_cpu_idx();
    validate_frozen_percpu_layout();
    bind_current_cpu(cpu_binding(cpu_idx)).expect("failed to bind the primary CPU-local area");
    ax_plat::call_main(cpu_idx, args)
}

#[somehal::secondary_entry]
fn secondary_main() {
    #[cfg(feature = "smp")]
    {
        ax_plat::call_secondary_main(meta.cpu_idx);
    }
}

fn validate_frozen_percpu_layout() {
    let platform = somehal::smp::percpu_data_layout()
        .expect("someboot must publish CPU-local storage before platform entry");
    let installed = ax_percpu::layout()
        .expect("someboot final-high must install the CPU-local layout exactly once");
    assert_eq!(installed.runtime_base, platform.runtime_base);
    assert_eq!(installed.area_stride, platform.area_stride);
    assert_eq!(installed.area_count, platform.area_count);
    assert_eq!(installed.flags, 0);
}

fn cpu_binding(cpu_idx: usize) -> CpuBindingV1 {
    somehal::setup::cpu_register_binding(cpu_idx)
        .expect("platform must allocate CPU-local storage for every boot CPU")
}

fn bind_current_cpu(binding: CpuBindingV1) -> Result<(), CpuBindError> {
    let cpu_index = ax_percpu::CpuIndex::try_from(binding.cpu_index as usize)
        .map_err(|_| CpuBindError::InvalidCpu)?;
    let area = ax_percpu::area(cpu_index).map_err(|_| CpuBindError::LayoutMismatch)?;
    if area.binding() != binding || area.prefix().header().binding() != binding {
        return Err(CpuBindError::LayoutMismatch);
    }
    // SAFETY: platform entry runs before the CPU is online, with local IRQs
    // masked and no scheduler capable of migrating this execution.
    unsafe { cpu_local::raw::install_binding(binding) }.map_err(|_| CpuBindError::Register)?;
    if cpu_local::platform::current_cpu_binding() != Ok(binding) {
        return Err(CpuBindError::Register);
    }
    Ok(())
}

pub fn boot_stack_bounds(cpu_idx: usize) -> (VirtAddr, usize) {
    let meta = somehal::smp::cpu_meta(cpu_idx)
        .unwrap_or_else(|| panic!("missing somehal PerCpuMeta for cpu_idx {cpu_idx}"));
    let stack_size = somehal::mem::stack_size();
    (VirtAddr::from(meta.stack_top_virt - stack_size), stack_size)
}

pub fn bootargs() -> Option<&'static str> {
    somehal::bootargs()
}

pub struct Kernel;

impl KernelOp for Kernel {
    fn bind_current_cpu(&self, binding: CpuBindingV1) -> Result<(), CpuBindError> {
        bind_current_cpu(binding)
    }

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
