mod earlycon;
mod memory;

pub use earlycon::setup_earlycon;
use kernutil::StaticCell;
#[allow(unused)]
pub use memory::{init_memory_map, memories};

use crate::mem::phys_to_virt;

pub(crate) static mut FDT_ADDR: usize = 0;
static FDT: StaticCell<fdt_edit::Fdt> = StaticCell::uninit();

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

pub(crate) fn set_fdt_addr_phys_if_valid(fdt_addr: usize) -> bool {
    if fdt_addr == 0 {
        return false;
    }

    let ptr = phys_to_virt(fdt_addr);
    // SAFETY: the candidate physical address is converted through the current
    // early mapping and is only borrowed for validation here.
    if unsafe { validated_fdt_slice(ptr) }.is_none() {
        return false;
    }

    unsafe {
        FDT_ADDR = fdt_addr;
    }
    true
}

fn fdt_base() -> Option<fdt_raw::Fdt<'static>> {
    let fdt_addr = fdt_addr()?;
    // SAFETY: the global FDT address points to firmware memory or the saved
    // early RAM copy, both of which stay valid for the boot lifetime.
    let slice = unsafe { validated_fdt_slice(fdt_addr)? };
    let fdt = fdt_raw::Fdt::from_bytes(slice).ok()?;
    Some(fdt)
}

pub(crate) fn init_with_alloc() -> Option<()> {
    let fdt_addr = fdt_addr()?;
    // SAFETY: the global FDT address points to firmware memory or the saved
    // early RAM copy, both of which stay valid for the boot lifetime.
    let slice = unsafe { validated_fdt_slice(fdt_addr)? };
    let fdt = fdt_edit::Fdt::from_bytes(slice).ok()?;
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
    // SAFETY: the current FDT address is expected to reference firmware memory
    // that is readable until we copy it into early RAM below.
    let Some(slice) = (unsafe { validated_fdt_slice(src) }) else {
        return;
    };
    let size = slice.len();

    let fdt_buff = crate::mem::ram::alloc(
        core::alloc::Layout::from_size_align(size, 8).expect("FDT allocation alignment is valid"),
    )
    .expect("early RAM must have space for the validated FDT");

    unsafe {
        core::ptr::copy_nonoverlapping(slice.as_ptr(), phys_to_virt(fdt_buff), size);
        FDT_ADDR = fdt_buff;
    }
}

/// Returns the validated firmware device-tree size copied into early RAM.
pub(crate) fn copy_size() -> usize {
    let Some(src) = fdt_addr() else {
        return 0;
    };
    // SAFETY: the firmware FDT remains readable until `save_fdt` copies it.
    unsafe { validated_fdt_slice(src) }.map_or(0, <[u8]>::len)
}

/// # Safety
///
/// `ptr` must reference a readable FDT blob that remains valid for the returned
/// slice lifetime.
unsafe fn validated_fdt_slice<'a>(ptr: *mut u8) -> Option<&'a [u8]> {
    if ptr.is_null() {
        return None;
    }

    // SAFETY: callers pass a firmware-provided candidate that is already
    // reachable through the current early address mapping. `Header::from_ptr`
    // only reads the fixed-size header and validates its FDT magic.
    let header = unsafe { fdt_raw::Header::from_ptr(ptr).ok()? };
    let total_size = header.totalsize as usize;
    if !(core::mem::size_of::<fdt_raw::Header>()..=MAX_FDT_SIZE).contains(&total_size) {
        return None;
    }

    // SAFETY: the header totalsize is bounded above before constructing the
    // slice. `Fdt::from_bytes` below then validates the whole blob structure.
    let slice = unsafe { core::slice::from_raw_parts(ptr.cast_const(), total_size) };
    fdt_raw::Fdt::from_bytes(slice).ok()?;
    Some(slice)
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
    fn arch_default_canonicalize_paddr_keeps_identity() {
        assert_eq!(
            <crate::arch::Arch as crate::ArchTrait>::canonicalize_paddr(0x1234_5678),
            0x1234_5678
        );
    }

    #[test]
    fn arch_default_ioremap_device_uses_generic_path() {
        assert_eq!(
            <crate::arch::Arch as crate::ArchTrait>::ioremap_device(0x1234_5678, 0x1000),
            None
        );
        assert!(<crate::arch::Arch as crate::ArchTrait>::user_aspace_needs_kernel_mappings());
    }

    #[test]
    fn set_fdt_addr_phys_rejects_zero() {
        assert!(!set_fdt_addr_phys_if_valid(0));
    }

    #[test]
    fn validated_fdt_slice_accepts_valid_tree() {
        let fdt = minimal_cpu_fdt();
        let fdt_data = fdt.encode();

        let slice = unsafe { validated_fdt_slice(fdt_data.as_ref().as_ptr().cast_mut()) }
            .expect("validate test fdt");

        assert_eq!(slice, fdt_data.as_ref());
    }

    #[test]
    fn validated_fdt_slice_rejects_bad_magic() {
        let fdt = minimal_cpu_fdt();
        let mut fdt_data = fdt.encode().as_ref().to_vec();
        fdt_data[0] ^= 0xff;

        assert!(unsafe { validated_fdt_slice(fdt_data.as_mut_ptr()) }.is_none());
    }

    #[test]
    fn validated_fdt_slice_rejects_oversized_total_size() {
        let fdt = minimal_cpu_fdt();
        let mut fdt_data = fdt.encode().as_ref().to_vec();
        let oversized = (MAX_FDT_SIZE as u32 + 1).to_be_bytes();
        fdt_data[4..8].copy_from_slice(&oversized);

        assert!(unsafe { validated_fdt_slice(fdt_data.as_mut_ptr()) }.is_none());
    }

    #[test]
    fn save_fdt_copies_validated_slice_length() {
        let fdt = minimal_cpu_fdt();
        let mut fdt_data = fdt.encode().as_ref().to_vec();
        let fdt_size = fdt_data.len();
        fdt_data.extend_from_slice(&[0xcc; 16]);

        let sentinel = 0xa5;
        let mut saved_memory = Vec::new();
        saved_memory.resize(fdt_size + 64, sentinel);
        let saved_start = saved_memory.as_mut_ptr() as usize;
        crate::mem::ram::init(saved_start..saved_start + saved_memory.len());

        assert!(set_fdt_addr_phys_if_valid(fdt_data.as_mut_ptr() as usize));
        save_fdt();

        let saved_ptr = fdt_addr().expect("saved fdt address");
        let saved_slice = unsafe { core::slice::from_raw_parts(saved_ptr.cast_const(), fdt_size) };
        assert_eq!(saved_slice, &fdt_data[..fdt_size]);
        assert_eq!(unsafe { saved_ptr.add(fdt_size).read() }, sentinel);

        unsafe {
            FDT_ADDR = 0;
        }
    }

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
