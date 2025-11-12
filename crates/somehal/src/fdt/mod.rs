use fdt_parser::base;

mod earlycon;
mod memory;

pub use earlycon::setup_earlycon;
pub use memory::setup_memory_map;

use crate::mem::phys_to_virt;

pub static mut FDT_ADDR: usize = 0;

fn fdt_addr() -> Option<*mut u8> {
    let fdt_addr = unsafe { FDT_ADDR };
    if fdt_addr == 0 {
        return None;
    }
    Some(phys_to_virt(fdt_addr))
}

fn fdt_base() -> Option<base::Fdt<'static>> {
    let fdt_addr = fdt_addr()?;
    let fdt = unsafe { base::Fdt::from_ptr(fdt_addr).ok()? };
    Some(fdt)
}

pub fn set_cmdline() -> Option<()> {
    let fdt = fdt_base()?;
    let chosen = fdt.chosen().ok()?;
    let cmdline = chosen.bootargs().ok()?;
    crate::cmdline::set_cmdline(cmdline);
    Some(())
}

pub(crate) fn save_fdt() {
    let Some(fdt) = fdt_base() else {
        return;
    };

    let size = fdt.total_size();
    let slice = unsafe { core::slice::from_raw_parts(FDT_ADDR as *const u8, size) };

    let fdt_buff = crate::mem::ram::Ram
        .alloc(core::alloc::Layout::from_size_align(size, 8).unwrap())
        .unwrap();

    unsafe {
        core::ptr::copy_nonoverlapping(slice.as_ptr(), fdt_buff, size);
        FDT_ADDR = fdt_buff as usize;
    }
}
