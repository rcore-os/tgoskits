mod earlycon;
mod memory;

pub use earlycon::setup_earlycon;
use kernutil::StaticCell;
#[allow(unused)]
pub use memory::{init_memory_map, memories};

use crate::mem::phys_to_virt;

pub(crate) static mut FDT_ADDR: usize = 0;
static FDT: StaticCell<fdt_edit::Fdt> = StaticCell::uninit();

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_HEADER_SIZE: usize = 40;
const MAX_FDT_SIZE: usize = 16 * 1024 * 1024;

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

#[cfg(target_arch = "loongarch64")]
pub(crate) fn set_fdt_addr_phys_if_valid(fdt_addr: usize) -> bool {
    if fdt_addr == 0 || !fdt_addr.is_multiple_of(4) {
        return false;
    }

    let ptr = phys_to_virt(fdt_addr);
    if fdt_total_size(ptr.cast_const()).is_none() {
        return false;
    }

    unsafe {
        FDT_ADDR = fdt_addr;
    }
    true
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
    let Some(src) = fdt_addr() else {
        return;
    };
    let Some(size) = fdt_total_size(src.cast_const()) else {
        return;
    };
    let slice = unsafe { core::slice::from_raw_parts(src as *const u8, size) };

    let fdt_buff = unsafe {
        crate::mem::ram::alloc(core::alloc::Layout::from_size_align(size, 8).unwrap()).unwrap()
    };

    unsafe {
        core::ptr::copy_nonoverlapping(slice.as_ptr(), phys_to_virt(fdt_buff), size);
        FDT_ADDR = fdt_buff;
    }
}

fn fdt_total_size(ptr: *const u8) -> Option<usize> {
    if ptr.is_null() {
        return None;
    }

    let magic = unsafe { read_be_u32(ptr, 0) };
    if magic != FDT_MAGIC {
        return None;
    }

    let total_size = unsafe { read_be_u32(ptr, 4) } as usize;
    if !(FDT_HEADER_SIZE..=MAX_FDT_SIZE).contains(&total_size) {
        return None;
    }

    let off_dt_struct = unsafe { read_be_u32(ptr, 8) } as usize;
    let off_dt_strings = unsafe { read_be_u32(ptr, 12) } as usize;
    let off_mem_rsvmap = unsafe { read_be_u32(ptr, 16) } as usize;
    if !valid_fdt_block_offset(off_dt_struct, total_size)
        || !valid_fdt_block_offset(off_dt_strings, total_size)
        || !valid_fdt_block_offset(off_mem_rsvmap, total_size)
    {
        return None;
    }

    Some(total_size)
}

fn valid_fdt_block_offset(offset: usize, total_size: usize) -> bool {
    (FDT_HEADER_SIZE..total_size).contains(&offset) && offset.is_multiple_of(4)
}

unsafe fn read_be_u32(ptr: *const u8, offset: usize) -> u32 {
    let bytes = unsafe {
        [
            core::ptr::read(ptr.add(offset)),
            core::ptr::read(ptr.add(offset + 1)),
            core::ptr::read(ptr.add(offset + 2)),
            core::ptr::read(ptr.add(offset + 3)),
        ]
    };
    u32::from_be_bytes(bytes)
}

fn cpu_nodes_from_fdt<'a>(fdt: fdt_raw::Fdt<'a>) -> impl Iterator<Item = fdt_raw::Node<'a>> + 'a {
    fdt.find_children_by_path("/cpus")
        .filter(|node| is_cpu_node_available(node))
}

fn cpu_id_list_from_fdt<'a>(fdt: fdt_raw::Fdt<'a>) -> impl Iterator<Item = usize> + 'a {
    cpu_nodes_from_fdt(fdt).filter_map(|node| {
        node.reg()
            .and_then(|mut regs| regs.next())
            .map(|reg| reg.address as usize)
    })
}

pub fn cpu_id_list() -> Option<impl Iterator<Item = usize>> {
    Some(cpu_id_list_from_fdt(fdt_base()?))
}

pub fn platform_name() -> Option<&'static str> {
    platform_name_from_fdt(fdt_base()?)
}

fn platform_name_from_fdt<'a>(fdt: fdt_raw::Fdt<'a>) -> Option<&'a str> {
    let root = fdt.find_by_path("/")?;
    root.find_property_str("model")
        .or_else(|| root.compatibles().next())
}

fn is_cpu_node_available(node: &fdt_raw::Node<'_>) -> bool {
    node.name().starts_with("cpu@")
        && matches!(node.find_property_str("device_type"), None | Some("cpu"))
        && matches!(
            node.find_property_str("status"),
            None | Some("okay") | Some("ok")
        )
}

#[cfg(test)]
mod tests {
    use alloc::{format, vec::Vec};

    use fdt_edit::{Fdt, Node, NodeId, Property};

    use super::*;

    #[test]
    fn cpu_id_list_skips_disabled_cpu_nodes() {
        let fdt = minimal_cpu_fdt();
        let fdt_data = fdt.encode();
        let raw = fdt_raw::Fdt::from_bytes(fdt_data.as_ref()).expect("parse test fdt");

        let cpu_ids: Vec<_> = cpu_id_list_from_fdt(raw).collect();

        assert_eq!(cpu_ids.as_slice(), &[1, 2, 3, 4]);
    }

    #[test]
    fn platform_name_prefers_root_model() {
        let mut fdt = minimal_cpu_fdt();
        let root = fdt.root_id();
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_str("model", "QEMU Arm Virtual Machine"));
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_strs("compatible", &["linux,dummy-virt"]));
        let fdt_data = fdt.encode();
        let raw = fdt_raw::Fdt::from_bytes(fdt_data.as_ref()).expect("parse test fdt");

        assert_eq!(
            platform_name_from_fdt(raw),
            Some("QEMU Arm Virtual Machine")
        );
    }

    #[test]
    fn platform_name_falls_back_to_root_compatible() {
        let mut fdt = minimal_cpu_fdt();
        let root = fdt.root_id();
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_strs("compatible", &["qemu,virt", "linux,dummy-virt"]));
        let fdt_data = fdt.encode();
        let raw = fdt_raw::Fdt::from_bytes(fdt_data.as_ref()).expect("parse test fdt");

        assert_eq!(platform_name_from_fdt(raw), Some("qemu,virt"));
    }

    fn minimal_cpu_fdt() -> Fdt {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let cpus = fdt.add_node(root, Node::new("cpus"));
        fdt.node_mut(cpus)
            .unwrap()
            .set_property(prop_u32s("#address-cells", &[1]));
        fdt.node_mut(cpus)
            .unwrap()
            .set_property(prop_u32s("#size-cells", &[0]));

        add_cpu(&mut fdt, cpus, 0, Some("disabled"), true);
        add_cpu(&mut fdt, cpus, 1, None, true);
        add_cpu(&mut fdt, cpus, 2, Some("okay"), true);
        add_cpu(&mut fdt, cpus, 3, Some("ok"), true);
        add_cpu(&mut fdt, cpus, 4, None, true);
        add_cpu(&mut fdt, cpus, 5, None, false);
        fdt
    }

    fn add_cpu(fdt: &mut Fdt, parent: NodeId, hart_id: u32, status: Option<&str>, with_reg: bool) {
        let cpu = fdt.add_node(parent, Node::new(&format!("cpu@{hart_id}")));
        fdt.node_mut(cpu)
            .unwrap()
            .set_property(prop_str("device_type", "cpu"));
        fdt.node_mut(cpu)
            .unwrap()
            .set_property(prop_strs("compatible", &["riscv"]));
        if with_reg {
            fdt.node_mut(cpu)
                .unwrap()
                .set_property(prop_u32s("reg", &[hart_id]));
        }
        if let Some(status) = status {
            fdt.node_mut(cpu)
                .unwrap()
                .set_property(prop_str("status", status));
        }
    }

    fn prop_u32s(name: &str, values: &[u32]) -> Property {
        let mut data = Vec::new();
        for value in values {
            data.extend_from_slice(&value.to_be_bytes());
        }
        Property::new(name, data)
    }

    fn prop_str(name: &str, value: &str) -> Property {
        prop_strs(name, &[value])
    }

    fn prop_strs(name: &str, values: &[&str]) -> Property {
        let mut data = Vec::new();
        for value in values {
            data.extend_from_slice(value.as_bytes());
            data.push(0);
        }
        Property::new(name, data)
    }
}
