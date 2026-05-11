mod earlycon;
mod memory;

pub use earlycon::setup_earlycon;
use kernutil::StaticCell;
#[allow(unused)]
pub use memory::{init_memory_map, memories};

use crate::mem::phys_to_virt;

pub(crate) static mut FDT_ADDR: usize = 0;
static FDT: StaticCell<fdt_edit::Fdt> = StaticCell::uninit();

pub fn fdt_addr() -> Option<*mut u8> {
    let fdt_addr = unsafe { FDT_ADDR };
    if fdt_addr == 0 {
        return None;
    }
    Some(phys_to_virt(fdt_addr))
}

pub fn fdt_addr_phys() -> Option<usize> {
    let fdt_addr = unsafe { FDT_ADDR };
    if fdt_addr == 0 {
        return None;
    }
    Some(fdt_addr)
}

fn fdt_base() -> Option<fdt_raw::Fdt<'static>> {
    let fdt_addr = fdt_addr()?;
    let fdt = unsafe { fdt_raw::Fdt::from_ptr(fdt_addr).ok()? };
    Some(fdt)
}

pub(crate) fn init_with_alloc() -> Option<()> {
    let fdt_addr = fdt_addr()?;
    let fdt = unsafe { fdt_edit::Fdt::from_ptr(fdt_addr).ok()? };
    FDT.init(fdt);
    Some(())
}
#[allow(dead_code)]
pub(crate) fn fdt() -> Option<&'static fdt_edit::Fdt> {
    fdt_addr()?;
    Some(&FDT)
}

pub fn set_cmdline() -> Option<()> {
    let fdt = fdt_base()?;
    let chosen = fdt.chosen()?;
    let cmdline = chosen.bootargs()?;
    crate::cmdline::set_cmdline(cmdline);
    Some(())
}

pub(crate) fn save_fdt() {
    let Some(fdt) = fdt_base() else {
        return;
    };

    let size = fdt.header().totalsize as usize;
    let slice = unsafe { core::slice::from_raw_parts(FDT_ADDR as *const u8, size) };

    let fdt_buff = unsafe {
        crate::mem::ram::alloc(core::alloc::Layout::from_size_align(size, 8).unwrap()).unwrap()
    };

    unsafe {
        core::ptr::copy_nonoverlapping(slice.as_ptr(), fdt_buff as _, size);
        FDT_ADDR = fdt_buff;
    }
}

fn cpu_nodes() -> Option<impl Iterator<Item = fdt_raw::Node<'static>>> {
    let fdt = fdt_base()?;
    let iter = fdt.find_children_by_path("/cpus");
    Some(iter.filter(|n| n.name().starts_with("cpu@")))
}

pub fn cpu_id_list() -> Option<impl Iterator<Item = usize>> {
    Some(cpu_nodes()?.map(|node| {
        node.reg()
            .and_then(|mut r| r.next())
            .map(|reg| reg.address as usize)
            .unwrap_or(0)
    }))
}
