use sparreal_kernel::{hal::al::*, impl_trait};

struct MemoryImpl;

impl_trait! {
impl Memory for MemoryImpl {
    unsafe fn virt_to_phys(virt: *mut u8) -> usize {
        somehal::mem::virt_to_phys(virt)
    }

    fn phys_to_virt(phys: usize) -> *mut u8 {
        somehal::mem::phys_to_virt(phys as _)
    }
}
}

struct CpuImpl;

impl_trait! {
impl Cpu for CpuImpl {
    fn current_cpu_id() -> usize {
        todo!()
    }

    fn irq_is_enabled() -> bool {
        false
    }

    fn irq_set_enabled(enabled:bool) {

    }
}
}

struct ConsoleImpl;

impl_trait! {
impl Console for ConsoleImpl {
    fn early_write(bytes: &[u8]) -> usize {
        somehal::console::_write_bytes(bytes)
    }

    fn early_read() -> Option<u8> {
        None
    }
}
}
