use core::{alloc::Layout, mem::size_of};

use super::{
    __cpu_id_list, PerCpuLayoutError, PerCpuMeta, alloc_percpu_region, allocated_cpu_count,
    checked_align_up_pow2, checked_allocation_layout, cpu_count, meta_align, percpu_data_range,
    percpu_link_range, percpu_link_size, percpu_region_align, publish_runtime_percpu,
    set_percpu_range,
};
use crate::mem::{__kimage_va, __percpu, phys_to_virt, stack_size, virt_to_phys};

#[derive(Clone, Copy, Debug)]
struct LayoutRequirements {
    data_size: usize,
    metadata_size: usize,
    metadata_alignment: usize,
    stack_size: usize,
    page_alignment: usize,
    region_alignment: usize,
}

#[derive(Clone, Copy, Debug)]
struct LayoutInfo {
    meta_offset: usize,
    stack_offset: usize,
    area_stride: usize,
    allocation_layout: Layout,
}

fn layout_info(cpu_count: usize) -> Result<LayoutInfo, PerCpuLayoutError> {
    calculate_layout(
        cpu_count,
        LayoutRequirements {
            data_size: percpu_link_size()?,
            metadata_size: size_of::<PerCpuMeta>(),
            metadata_alignment: meta_align(),
            stack_size: stack_size(),
            page_alignment: crate::mem::page_size(),
            region_alignment: percpu_region_align()?,
        },
    )
}

fn calculate_layout(
    cpu_count: usize,
    requirements: LayoutRequirements,
) -> Result<LayoutInfo, PerCpuLayoutError> {
    if cpu_count == 0 {
        return Err(PerCpuLayoutError::EmptyCpuSet);
    }
    let meta_offset =
        checked_align_up_pow2(requirements.data_size, requirements.metadata_alignment)?;
    let metadata_end = meta_offset
        .checked_add(requirements.metadata_size)
        .ok_or(PerCpuLayoutError::AddressOverflow)?;
    let stack_offset = checked_align_up_pow2(metadata_end, requirements.page_alignment)?;
    let stack_end = stack_offset
        .checked_add(requirements.stack_size)
        .ok_or(PerCpuLayoutError::AddressOverflow)?;
    let area_stride = checked_align_up_pow2(stack_end, requirements.region_alignment)?;
    let total_size = area_stride
        .checked_mul(cpu_count)
        .ok_or(PerCpuLayoutError::AddressOverflow)?;
    let allocation_layout = checked_allocation_layout(total_size, requirements.region_alignment)?;

    Ok(LayoutInfo {
        meta_offset,
        stack_offset,
        area_stride,
        allocation_layout,
    })
}

pub(crate) fn percpu_data_stride() -> usize {
    layout_info(allocated_cpu_count())
        .expect("published per-CPU count must preserve its validated layout")
        .area_stride
}

fn cpu_area_start(cpu_index: usize) -> Option<usize> {
    let cpu_count = allocated_cpu_count();
    if cpu_index >= cpu_count {
        return None;
    }
    let layout = layout_info(cpu_count).ok()?;
    let allocation = percpu_data_range();
    let area_offset = layout.area_stride.checked_mul(cpu_index)?;
    let area_start = allocation.start.checked_add(area_offset)?;
    let area_end = area_start.checked_add(layout.area_stride)?;
    (area_end <= allocation.end).then_some(area_start)
}

pub fn alloc_percpu() {
    println!("Initializing per-CPU data");
    let cpu_count = cpu_count();
    let layout = layout_info(cpu_count)
        .unwrap_or_else(|error| panic!("invalid firmware per-CPU layout: {error}"));
    let link_range = percpu_link_range();
    let link_size = percpu_link_size().expect("validated linker template range must stay ordered");
    let total_size = layout.allocation_layout.size();

    println!("Per-CPU data one cpu size: {:#x} bytes", layout.area_stride);
    println!(
        "Total per-CPU data size for secondary CPUs: {total_size:#x} bytes ({cpu_count} CPUs)"
    );

    let percpu_data = alloc_percpu_region(layout.allocation_layout);
    set_percpu_range(percpu_data, total_size, cpu_count);

    unsafe {
        core::ptr::write_bytes(phys_to_virt(percpu_data), 0, total_size);
    }

    println!(
        "Per-CPU data allocated at {:#x} - {:#x}",
        percpu_data_range().start,
        percpu_data_range().end
    );

    let link_phys_start = virt_to_phys(link_range.start as *const u8);
    let entry_phys = virt_to_phys(super::super::entry::secondary_entry as *const () as *const u8);
    let entry_virt = __kimage_va(entry_phys);

    for (cpu_index, hardware_id) in __cpu_id_list().enumerate() {
        let cpu_percpu_start =
            cpu_area_start(cpu_index).expect("validated per-CPU area must remain addressable");
        println!(
            "Initializing per-CPU RAM for CPU{cpu_index} - hard id {hardware_id:#x} @ \
             {cpu_percpu_start:#x}"
        );
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_to_virt(link_phys_start) as *const u8,
                phys_to_virt(cpu_percpu_start),
                link_size,
            );
        }
        let meta_start = cpu_percpu_start
            .checked_add(layout.meta_offset)
            .expect("validated metadata offset must fit its CPU area");
        let meta_va = phys_to_virt(meta_start);
        debug_assert_eq!(meta_start % meta_align(), 0);
        debug_assert_eq!((meta_va as usize) % meta_align(), 0);

        let stack_top = cpu_percpu_start
            .checked_add(layout.stack_offset)
            .and_then(|stack_start| stack_start.checked_add(stack_size()))
            .expect("validated stack extent must fit its CPU area");
        let stack_top_virt = __percpu(stack_top);

        let meta = PerCpuMeta {
            stack_top,
            cpu_id: hardware_id,
            cpu_idx: cpu_index,
            stack_top_virt: stack_top_virt as _,
            entry_virt: entry_virt as _,
            boot_table_paddr: 0,
            primary_table_paddr: 0,
        };
        unsafe {
            *meta_va.cast::<PerCpuMeta>() = meta;
        }
    }

    publish_runtime_percpu(cpu_count);

    for meta in super::cpu_meta_list() {
        println!(
            "CPU{} - hard id {:#x}, stack top @{:#x}, stack top virt @{:#x}, entry virt @{:#x}",
            meta.cpu_idx, meta.cpu_id, meta.stack_top, meta.stack_top_virt, meta.entry_virt
        );
    }
}

pub(crate) fn cpu_meta_addr(cpu_index: usize) -> Option<usize> {
    let layout = layout_info(allocated_cpu_count()).ok()?;
    cpu_area_start(cpu_index)?.checked_add(layout.meta_offset)
}

pub(crate) fn percpu_data_phys(cpu_index: usize) -> Option<usize> {
    cpu_area_start(cpu_index)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_REQUIREMENTS: LayoutRequirements = LayoutRequirements {
        data_size: 128,
        metadata_size: 64,
        metadata_alignment: 64,
        stack_size: 4096,
        page_alignment: 4096,
        region_alignment: 4096,
    };

    #[test]
    fn extreme_firmware_cpu_count_returns_overflow_without_wrapping() {
        assert!(matches!(
            calculate_layout(usize::MAX, TEST_REQUIREMENTS),
            Err(PerCpuLayoutError::AddressOverflow)
        ));
    }

    #[test]
    fn empty_firmware_cpu_set_is_rejected_before_allocation() {
        assert!(matches!(
            calculate_layout(0, TEST_REQUIREMENTS),
            Err(PerCpuLayoutError::EmptyCpuSet)
        ));
    }

    #[test]
    fn ordinary_layout_keeps_metadata_and_stack_inside_each_area() {
        let layout = calculate_layout(4, TEST_REQUIREMENTS).unwrap();
        assert_eq!(layout.meta_offset, 128);
        assert_eq!(layout.stack_offset, 4096);
        assert_eq!(layout.area_stride, 8192);
        assert_eq!(layout.allocation_layout.size(), 4 * 8192);
        assert_eq!(layout.allocation_layout.align(), 4096);
    }
}
