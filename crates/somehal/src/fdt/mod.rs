use fdt_parser::base;

mod earlycon;
mod memory;

pub use earlycon::setup_earlycon;
pub use memory::setup_memory_map;

use crate::mem::MemoryDescriptor;
use heapless::Vec;

pub static mut FDT_ADDR: usize = 0;

fn fdt_base() -> Option<base::Fdt<'static>> {
    let fdt_addr = unsafe { FDT_ADDR };
    if fdt_addr == 0 {
        return None;
    }
    let fdt = unsafe { base::Fdt::from_ptr(fdt_addr as *mut u8).ok()? };
    Some(fdt)
}

pub fn set_cmdline() -> Option<()> {
    let fdt = fdt_base()?;
    let chosen = fdt.chosen().ok()?;
    let cmdline = chosen.bootargs().ok()?;
    crate::cmdline::set_cmdline(cmdline);
    Some(())
}
