use core::{alloc::Layout, mem::size_of, ops::Range};

use super::{
    PerCpuLayoutError, PerCpuMeta, alloc_percpu_region, allocated_cpu_count, checked_align_up_pow2,
    checked_allocation_layout, cpu_count, meta_align, percpu_data_range, percpu_link_size,
    percpu_region_align, set_percpu_range,
};
use crate::mem::stack_size;

#[derive(Clone, Copy, Debug)]
struct LayoutRequirements {
    metadata_size: usize,
    metadata_alignment: usize,
    stack_size: usize,
    page_alignment: usize,
    data_size: usize,
    region_alignment: usize,
}

#[derive(Clone, Copy, Debug)]
struct LayoutInfo {
    meta_region_offset: usize,
    meta_stride: usize,
    stack_region_offset: usize,
    stack_stride: usize,
    data_region_offset: usize,
    data_stride: usize,
    data_size: usize,
    allocation_layout: Layout,
}

#[derive(Clone, Copy)]
struct RegionSlots {
    offset: usize,
    stride: usize,
    occupied_size: usize,
}

fn layout_info(cpu_count: usize) -> Result<LayoutInfo, PerCpuLayoutError> {
    calculate_layout(
        cpu_count,
        LayoutRequirements {
            metadata_size: size_of::<PerCpuMeta>(),
            metadata_alignment: meta_align(),
            stack_size: stack_size(),
            page_alignment: crate::mem::page_size(),
            data_size: percpu_link_size()?,
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
    let meta_stride =
        aligned_slot_size(requirements.metadata_size, requirements.metadata_alignment)?;
    let meta_region_size = meta_stride
        .checked_mul(cpu_count)
        .ok_or(PerCpuLayoutError::AddressOverflow)?;

    let stack_stride = aligned_slot_size(requirements.stack_size, requirements.page_alignment)?;
    let stack_region_offset = checked_align_up_pow2(meta_region_size, requirements.page_alignment)?;
    let stack_region_size = stack_stride
        .checked_mul(cpu_count)
        .ok_or(PerCpuLayoutError::AddressOverflow)?;
    let stack_region_end = stack_region_offset
        .checked_add(stack_region_size)
        .ok_or(PerCpuLayoutError::AddressOverflow)?;

    let data_stride = aligned_slot_size(requirements.data_size, requirements.region_alignment)?;
    let data_region_offset =
        checked_align_up_pow2(stack_region_end, requirements.region_alignment)?;
    let data_region_size = data_stride
        .checked_mul(cpu_count)
        .ok_or(PerCpuLayoutError::AddressOverflow)?;
    let data_region_end = data_region_offset
        .checked_add(data_region_size)
        .ok_or(PerCpuLayoutError::AddressOverflow)?;
    let total_size = checked_align_up_pow2(data_region_end, requirements.region_alignment)?;
    let allocation_layout = checked_allocation_layout(total_size, requirements.region_alignment)?;

    Ok(LayoutInfo {
        meta_region_offset: 0,
        meta_stride,
        stack_region_offset,
        stack_stride,
        data_region_offset,
        data_stride,
        data_size: requirements.data_size,
        allocation_layout,
    })
}

fn aligned_slot_size(size: usize, alignment: usize) -> Result<usize, PerCpuLayoutError> {
    checked_align_up_pow2(size.max(1), alignment)
}

pub(crate) fn percpu_data_stride() -> usize {
    layout_info(allocated_cpu_count())
        .expect("published per-CPU count must preserve its validated layout")
        .data_stride
}

fn cpu_meta_start(cpu_index: usize) -> Option<usize> {
    let cpu_count = allocated_cpu_count();
    if cpu_index >= cpu_count {
        return None;
    }
    let layout = layout_info(cpu_count).ok()?;
    region_slot_start(
        &percpu_data_range(),
        RegionSlots {
            offset: layout.meta_region_offset,
            stride: layout.meta_stride,
            occupied_size: size_of::<PerCpuMeta>(),
        },
        cpu_index,
    )
}

fn cpu_stack_start(cpu_index: usize) -> Option<usize> {
    let cpu_count = allocated_cpu_count();
    if cpu_index >= cpu_count {
        return None;
    }
    let layout = layout_info(cpu_count).ok()?;
    region_slot_start(
        &percpu_data_range(),
        RegionSlots {
            offset: layout.stack_region_offset,
            stride: layout.stack_stride,
            occupied_size: stack_size(),
        },
        cpu_index,
    )
}

fn cpu_data_start(cpu_index: usize) -> Option<usize> {
    let cpu_count = allocated_cpu_count();
    if cpu_index >= cpu_count {
        return None;
    }
    let layout = layout_info(cpu_count).ok()?;
    region_slot_start(
        &percpu_data_range(),
        RegionSlots {
            offset: layout.data_region_offset,
            stride: layout.data_stride,
            occupied_size: layout.data_stride,
        },
        cpu_index,
    )
}

fn region_slot_start(
    allocation: &Range<usize>,
    region: RegionSlots,
    cpu_index: usize,
) -> Option<usize> {
    let region_base = allocation.start.checked_add(region.offset)?;
    let slot_offset = region.stride.checked_mul(cpu_index)?;
    let slot_start = region_base.checked_add(slot_offset)?;
    let slot_end = slot_start.checked_add(region.occupied_size)?;
    (slot_end <= allocation.end).then_some(slot_start)
}

pub fn alloc_percpu() {
    println!("Reserving per-CPU data");
    let cpu_count = cpu_count();
    let layout = layout_info(cpu_count)
        .unwrap_or_else(|error| panic!("invalid firmware per-CPU layout: {error}"));
    let link_size = layout.data_size;
    let total_size = layout.allocation_layout.size();

    debug_assert_eq!(layout.meta_region_offset % meta_align(), 0);
    debug_assert_eq!(layout.meta_stride % meta_align(), 0);
    debug_assert_eq!(layout.stack_region_offset % crate::mem::page_size(), 0);
    debug_assert_eq!(layout.stack_stride % crate::mem::page_size(), 0);
    debug_assert_eq!(
        layout.data_region_offset % layout.allocation_layout.align(),
        0
    );
    debug_assert_eq!(layout.data_stride % layout.allocation_layout.align(), 0);

    println!("Per-CPU linker template size: {link_size:#x} bytes");
    println!("Per-CPU metadata stride: {:#x} bytes", layout.meta_stride);
    println!("Per-CPU stack stride: {:#x} bytes", layout.stack_stride);
    println!("Per-CPU data stride: {:#x} bytes", layout.data_stride);
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
    let meta_region_start = percpu_data
        .checked_add(layout.meta_region_offset)
        .expect("validated metadata region must fit its allocation");
    let stack_region_start = percpu_data
        .checked_add(layout.stack_region_offset)
        .expect("validated stack region must fit its allocation");
    let data_region_start = percpu_data
        .checked_add(layout.data_region_offset)
        .expect("validated data region must fit its allocation");
    println!(
        "Per-CPU prealloc layout: meta @ {meta_region_start:#x}, stack @ {stack_region_start:#x}, \
         data @ {data_region_start:#x}"
    );
}

pub(crate) fn cpu_meta_addr(cpu_index: usize) -> Option<usize> {
    cpu_meta_start(cpu_index)
}

pub(crate) fn percpu_data_phys(cpu_index: usize) -> Option<usize> {
    cpu_data_start(cpu_index)
}

pub(crate) fn cpu_stack_top(cpu_index: usize) -> Option<usize> {
    cpu_stack_start(cpu_index)?.checked_add(stack_size())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_REQUIREMENTS: LayoutRequirements = LayoutRequirements {
        metadata_size: 64,
        metadata_alignment: 64,
        stack_size: 4096,
        page_alignment: 4096,
        data_size: 128,
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
    fn ordinary_layout_keeps_regions_disjoint_and_aligned() {
        let layout = calculate_layout(4, TEST_REQUIREMENTS).unwrap();
        assert_eq!(layout.meta_region_offset, 0);
        assert_eq!(layout.meta_stride, 64);
        assert_eq!(layout.stack_region_offset, 4096);
        assert_eq!(layout.stack_stride, 4096);
        assert!(layout.data_region_offset >= layout.stack_region_offset + 4 * 4096);
        assert_eq!(layout.data_region_offset % 4096, 0);
        assert_eq!(layout.data_stride, 4096);
        assert_eq!(layout.allocation_layout.align(), 4096);
    }
}
