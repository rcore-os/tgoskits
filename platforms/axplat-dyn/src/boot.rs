use ax_memory_addr::VirtAddr;
use somehal::{
    KernelOp,
    setup::{CpuBindError, CpuIndex, MapError, MmioAddr, MmioOp, MmioRaw},
};

#[somehal::entry(Kernel)]
fn main() -> ! {
    let mut args = 0;
    if let Some(fdt) = somehal::fdt_addr_phys() {
        args = fdt;
    }

    let cpu_idx = somehal::smp::early_current_cpu_idx();
    validate_frozen_percpu_layout();
    bind_current_cpu(cpu_index(cpu_idx))
        .unwrap_or_else(|error| panic!("failed to bind the primary CPU-local area: {error}"));
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
    assert_eq!(installed.runtime_base(), platform.runtime_base);
    assert_eq!(installed.area_stride(), platform.area_stride);
    assert_eq!(installed.area_count(), platform.area_count);
}

fn cpu_index(cpu_idx: usize) -> CpuIndex {
    somehal::setup::cpu_index(cpu_idx)
        .expect("platform must allocate CPU-local storage for every boot CPU")
}

fn bind_current_cpu(cpu_index: CpuIndex) -> Result<(), CpuBindError> {
    let area = ax_percpu::area(cpu_index).map_err(map_percpu_bind_error)?;
    let cpu_area = area.cpu_area().map_err(map_percpu_bind_error)?;
    // SAFETY: platform entry runs before the CPU is online, with local IRQs
    // masked and no scheduler capable of migrating this execution.
    unsafe { cpu_local::install_cpu_area(cpu_area) }.map_err(CpuBindError::from)
}

fn map_percpu_bind_error(error: ax_percpu::PerCpuError) -> CpuBindError {
    match error {
        ax_percpu::PerCpuError::LayoutNotInstalled => CpuBindError::LayoutNotInstalled,
        ax_percpu::PerCpuError::CpuOutOfRange {
            cpu_index,
            area_count,
        } => CpuBindError::CpuOutOfRange {
            cpu_index,
            area_count,
        },
        ax_percpu::PerCpuError::AddressOverflow => CpuBindError::AddressOverflow,
        ax_percpu::PerCpuError::CpuLocal(error) => CpuBindError::CpuLocal(error),
        _ => CpuBindError::LayoutMismatch,
    }
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
    fn bind_current_cpu(&self, cpu_index: CpuIndex) -> Result<(), CpuBindError> {
        bind_current_cpu(cpu_index)
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
