#![no_std]
#![no_main]
#![cfg(not(any(windows, unix)))]

extern crate alloc;
extern crate somehal;

use core::ptr::NonNull;

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
    fn ioremap(&self, addr: MmioAddr, size: usize) -> Result<Mmio, Error> {
        let res = sparreal_kernel::os::mem::ioremap(addr.as_usize().into(), size)?;
        let ptr = res.raw() as *mut u8;
        Ok(unsafe { Mmio::new(addr, NonNull::new_unchecked(ptr), size) })
    }

    fn iounmap(&self, _mmio: &Mmio) {
        // sparreal_kernel::os::mem::iounmap(mmio)
    }
}
