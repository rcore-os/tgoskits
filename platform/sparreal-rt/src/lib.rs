#![no_std]
#![no_main]
#![cfg(not(any(windows, unix)))]

extern crate alloc;
extern crate somehal;

use somehal::setup::*;
pub use sparreal_kernel::*;

mod hal_impl;

#[somehal::entry(Kernel)]
fn main() -> ! {
    sparreal_kernel::run_kernel()
}

pub struct Kernel;

impl KernelOp for Kernel {}

impl MmioOp for Kernel {
    fn ioremap(&self, addr: MmioAddr, size: usize) -> Result<MmioRaw, MapError> {
        sparreal_kernel::os::mem::mmio::kernel_mmio_op().ioremap(addr, size)
    }

    fn iounmap(&self, mmio: &MmioRaw) {
        sparreal_kernel::os::mem::mmio::kernel_mmio_op().iounmap(mmio)
    }
}
