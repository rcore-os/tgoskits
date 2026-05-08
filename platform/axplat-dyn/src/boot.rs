use core::ptr::NonNull;

use ax_errno::AxError;
use ax_memory_addr::VirtAddr;
use ax_plat::mem::phys_to_virt;
use somehal::{KernelOp, setup::*};

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
        let virt = match axklib::mem::iomap(addr.as_usize().into(), size) {
            Ok(v) => v,
            Err(AxError::AlreadyExists) => {
                // If the region is already mapped, just return the existing mapping.
                phys_to_virt(addr.as_usize().into())
            }
            Err(e) => {
                error!("Failed to map MMIO region at {addr:?} with size {size:#x}: {e:?}");
                return Err(MapError::Invalid);
            }
        };
        Ok(unsafe { MmioRaw::new(addr, NonNull::new(virt.as_mut_ptr()).unwrap(), size) })
    }

    fn iounmap(&self, _mmio: &MmioRaw) {}
}
