use super::{
    __cpu_id_list, PerCpuMeta, align_up_pow2, alloc_percpu_region, cpu_count, meta_align,
    percpu_data_range, percpu_link_range, percpu_region_align, set_percpu_range,
};
use crate::mem::{__kimage_va, __percpu, phys_to_virt, stack_size};

fn meta_offset() -> usize {
    let link_size = percpu_link_range().len();
    let offset = align_up_pow2(link_size, meta_align());
    debug_assert_eq!(offset % meta_align(), 0);
    offset
}

fn stack_offset() -> usize {
    let meta_offset = meta_offset();
    let meta_size = core::mem::size_of::<PerCpuMeta>();
    align_up_pow2(meta_offset + meta_size, crate::mem::page_size())
}

fn percpu_data_size() -> usize {
    align_up_pow2(stack_offset() + stack_size(), percpu_region_align())
}

/// Per-CPU data layout:
///
/// | Linker percpu data | align to 0x8 | PerCpuMeta | align padding to page size | Stack |
pub fn alloc_percpu() {
    println!("Initializing per-CPU data");
    let cpu_num = cpu_count();
    let link_range = percpu_link_range();
    let link_size = link_range.len();

    let percpu_size = percpu_data_size();
    println!("Per-CPU data one cpu size: {:#x} bytes", percpu_size);
    let percpu_all_secondary_size = percpu_size * cpu_num;

    println!(
        "Total per-CPU data size for secondary CPUs: {:#x} bytes ({} CPUs)",
        percpu_all_secondary_size, cpu_num
    );

    let percpu_data = alloc_percpu_region(percpu_all_secondary_size);

    set_percpu_range(percpu_data, percpu_data + percpu_all_secondary_size);

    unsafe {
        core::ptr::write_bytes(phys_to_virt(percpu_data), 0, percpu_all_secondary_size);
    }

    println!(
        "Per-CPU data allocated at {:#x} - {:#x}",
        percpu_data_range().start,
        percpu_data_range().end
    );

    let entry_virt = __kimage_va(super::super::entry::secondary_entry as *const () as usize);

    for (idx, hard_id) in __cpu_id_list().enumerate() {
        let cpu_percpu_start = percpu_data_range().start + idx * percpu_size;
        println!(
            "Initializing per-CPU RAM for CPU{idx} - hard id {hard_id:#x} @ {cpu_percpu_start:#x}"
        );
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_to_virt(link_range.start) as *const u8,
                phys_to_virt(cpu_percpu_start),
                link_size,
            );
        }
        let meta_start = cpu_percpu_start + meta_offset();
        let meta_va = phys_to_virt(meta_start);
        debug_assert_eq!(meta_start % meta_align(), 0);
        debug_assert_eq!((meta_va as usize) % meta_align(), 0);

        let stack_top = cpu_percpu_start + stack_offset() + stack_size();
        let stack_top_virt = __percpu(stack_top);

        let meta = PerCpuMeta {
            stack_top,
            cpu_id: hard_id,
            cpu_idx: idx,
            stack_top_virt: stack_top_virt as _,
            entry_virt: entry_virt as _,
            boot_table_paddr: 0,
            primary_table_paddr: 0,
        };
        unsafe {
            *meta_va.cast::<PerCpuMeta>() = meta;
        }
    }

    for meta in super::cpu_meta_list() {
        println!(
            "CPU{} - hard id {:#x}, stack top @{:#x}, stack top virt @{:#x}, entry virt @{:#x}",
            meta.cpu_idx, meta.cpu_id, meta.stack_top, meta.stack_top_virt, meta.entry_virt
        );
    }
}

pub(crate) fn cpu_meta_addr(idx: usize) -> Option<usize> {
    let base = percpu_data_range().start + idx * percpu_data_size();
    if base >= percpu_data_range().end {
        return None;
    }
    Some(base + meta_offset())
}

pub(crate) fn percpu_data_ptr(idx: usize) -> Option<*mut u8> {
    let base = percpu_data_range().start + idx * percpu_data_size();
    if base >= percpu_data_range().end {
        return None;
    }
    Some(phys_to_virt(base))
}
