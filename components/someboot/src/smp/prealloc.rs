use super::{
    __cpu_id_list, PerCpuMeta, align_up_pow2, alloc_percpu_region, cpu_count, meta_align,
    percpu_data_range, percpu_link_range, percpu_region_align, set_percpu_range,
};
use crate::mem::{__kimage_va, __percpu, phys_to_virt, stack_size};

struct LayoutInfo {
    meta_region_offset: usize,
    meta_stride: usize,
    stack_region_offset: usize,
    stack_stride: usize,
    data_region_offset: usize,
    data_stride: usize,
    total_size: usize,
}

fn aligned_slot_size(size: usize, align: usize) -> usize {
    let size = size.max(1);
    align_up_pow2(size, align)
}

fn layout_info(cpu_num: usize) -> LayoutInfo {
    let meta_stride = aligned_slot_size(core::mem::size_of::<PerCpuMeta>(), meta_align());
    let meta_region_size = meta_stride * cpu_num;

    let stack_stride = aligned_slot_size(stack_size(), crate::mem::page_size());
    let stack_region_offset = align_up_pow2(meta_region_size, crate::mem::page_size());
    let stack_region_size = stack_stride * cpu_num;

    let data_stride = aligned_slot_size(percpu_link_range().len(), percpu_region_align());
    let data_region_offset = align_up_pow2(
        stack_region_offset + stack_region_size,
        percpu_region_align(),
    );
    let data_region_size = data_stride * cpu_num;
    let total_size = align_up_pow2(data_region_offset + data_region_size, percpu_region_align());

    LayoutInfo {
        meta_region_offset: 0,
        meta_stride,
        stack_region_offset,
        stack_stride,
        data_region_offset,
        data_stride,
        total_size,
    }
}

fn cpu_meta_start(idx: usize) -> Option<usize> {
    if idx >= cpu_count() {
        return None;
    }
    let layout = layout_info(cpu_count());
    Some(percpu_data_range().start + layout.meta_region_offset + idx * layout.meta_stride)
}

fn cpu_stack_start(idx: usize) -> Option<usize> {
    if idx >= cpu_count() {
        return None;
    }
    let layout = layout_info(cpu_count());
    Some(percpu_data_range().start + layout.stack_region_offset + idx * layout.stack_stride)
}

fn cpu_data_start(idx: usize) -> Option<usize> {
    if idx >= cpu_count() {
        return None;
    }
    let layout = layout_info(cpu_count());
    Some(percpu_data_range().start + layout.data_region_offset + idx * layout.data_stride)
}

pub fn alloc_percpu() {
    println!("Initializing per-CPU data");
    let cpu_num = cpu_count();
    let link_range = percpu_link_range();
    let link_size = link_range.len();
    let layout = layout_info(cpu_num);

    debug_assert_eq!(layout.meta_region_offset % meta_align(), 0);
    debug_assert_eq!(layout.meta_stride % meta_align(), 0);
    debug_assert_eq!(layout.stack_region_offset % crate::mem::page_size(), 0);
    debug_assert_eq!(layout.stack_stride % crate::mem::page_size(), 0);
    debug_assert_eq!(layout.data_region_offset % percpu_region_align(), 0);
    debug_assert_eq!(layout.data_stride % percpu_region_align(), 0);

    println!("Per-CPU linker template size: {:#x} bytes", link_size);
    println!("Per-CPU metadata stride: {:#x} bytes", layout.meta_stride);
    println!("Per-CPU stack stride: {:#x} bytes", layout.stack_stride);
    println!("Per-CPU data stride: {:#x} bytes", layout.data_stride);
    println!(
        "Total per-CPU data size for secondary CPUs: {:#x} bytes ({} CPUs)",
        layout.total_size, cpu_num
    );

    let percpu_data = alloc_percpu_region(layout.total_size);
    set_percpu_range(percpu_data, percpu_data + layout.total_size);

    unsafe {
        core::ptr::write_bytes(phys_to_virt(percpu_data), 0, layout.total_size);
    }

    println!(
        "Per-CPU data allocated at {:#x} - {:#x}",
        percpu_data_range().start,
        percpu_data_range().end
    );
    println!(
        "Per-CPU prealloc layout: meta @ {:#x}, stack @ {:#x}, data @ {:#x}",
        percpu_data_range().start + layout.meta_region_offset,
        percpu_data_range().start + layout.stack_region_offset,
        percpu_data_range().start + layout.data_region_offset
    );

    let entry_virt = __kimage_va(super::super::entry::secondary_entry as *const () as usize);

    for (idx, hard_id) in __cpu_id_list().enumerate() {
        let cpu_data_start = cpu_data_start(idx).unwrap();
        let meta_start = cpu_meta_start(idx).unwrap();
        let stack_start = cpu_stack_start(idx).unwrap();
        debug_assert_eq!(meta_start % meta_align(), 0);
        debug_assert_eq!(stack_start % crate::mem::page_size(), 0);
        debug_assert_eq!(cpu_data_start % percpu_region_align(), 0);
        println!(
            "Initializing per-CPU RAM for CPU{idx} - hard id {hard_id:#x}, meta @ \
             {meta_start:#x}, stack @ {stack_start:#x}, percpu @ {cpu_data_start:#x}"
        );
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_to_virt(link_range.start) as *const u8,
                phys_to_virt(cpu_data_start),
                link_size,
            );
        }

        let meta_va = phys_to_virt(meta_start);
        debug_assert_eq!(meta_start % meta_align(), 0);
        debug_assert_eq!((meta_va as usize) % meta_align(), 0);

        let stack_top = stack_start + stack_size();
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
    cpu_meta_start(idx)
}

pub(crate) fn percpu_data_ptr(idx: usize) -> Option<*mut u8> {
    let base = cpu_data_start(idx)?;
    Some(phys_to_virt(base))
}
