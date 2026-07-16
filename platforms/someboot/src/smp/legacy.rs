use core::{alloc::Layout, mem::size_of};

use super::{
    PerCpuLayoutError, PerCpuMeta, alloc_percpu_region, allocated_cpu_count, checked_align_up_pow2,
    checked_allocation_layout, cpu_count, meta_align, percpu_data_range, percpu_link_size,
    percpu_region_align, set_percpu_range,
};
use crate::mem::stack_size;

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
    println!("Reserving per-CPU data");
    let cpu_count = cpu_count();
    let layout = layout_info(cpu_count)
        .unwrap_or_else(|error| panic!("invalid firmware per-CPU layout: {error}"));
    let total_size = layout.allocation_layout.size();

    println!("Per-CPU data one cpu size: {:#x} bytes", layout.area_stride);
    println!(
        "Total per-CPU data size for secondary CPUs: {total_size:#x} bytes ({cpu_count} CPUs)"
    );

    let percpu_data = alloc_percpu_region(layout.allocation_layout);
    set_percpu_range(percpu_data, total_size, cpu_count);

    println!(
        "Per-CPU data allocated at {:#x} - {:#x}",
        percpu_data_range().start,
        percpu_data_range().end
    );
}

pub(crate) fn cpu_meta_addr(cpu_index: usize) -> Option<usize> {
    let layout = layout_info(allocated_cpu_count()).ok()?;
    cpu_area_start(cpu_index)?.checked_add(layout.meta_offset)
}

pub(crate) fn percpu_data_phys(cpu_index: usize) -> Option<usize> {
    cpu_area_start(cpu_index)
}

pub(crate) fn cpu_stack_top(cpu_index: usize) -> Option<usize> {
    let layout = layout_info(allocated_cpu_count()).ok()?;
    cpu_area_start(cpu_index)?
        .checked_add(layout.stack_offset)?
        .checked_add(stack_size())
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
