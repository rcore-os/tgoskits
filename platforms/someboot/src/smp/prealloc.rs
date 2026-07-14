use core::{alloc::Layout, mem::size_of, ops::Range};

use super::{
    __cpu_id_list, PerCpuLayoutError, PerCpuMeta, alloc_percpu_region, allocated_cpu_count,
    checked_align_up_pow2, checked_allocation_layout, cpu_count, meta_align, percpu_data_range,
    percpu_link_range, percpu_link_size, percpu_region_align, publish_runtime_percpu,
    set_percpu_range,
};
use crate::mem::{__kimage_va, __percpu, phys_to_virt, stack_size, virt_to_phys};

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
    println!("Initializing per-CPU data");
    let cpu_count = cpu_count();
    let layout = layout_info(cpu_count)
        .unwrap_or_else(|error| panic!("invalid firmware per-CPU layout: {error}"));
    let link_range = percpu_link_range();
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

    unsafe {
        core::ptr::write_bytes(phys_to_virt(percpu_data), 0, total_size);
    }

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

    let link_phys_start = virt_to_phys(link_range.start as *const u8);
    let entry_phys = virt_to_phys(super::super::entry::secondary_entry as *const () as *const u8);
    let entry_virt = __kimage_va(entry_phys);

    for (cpu_index, hardware_id) in __cpu_id_list().enumerate() {
        let cpu_data_start =
            cpu_data_start(cpu_index).expect("validated per-CPU data slot must remain addressable");
        let meta_start =
            cpu_meta_start(cpu_index).expect("validated metadata slot must remain addressable");
        let stack_start =
            cpu_stack_start(cpu_index).expect("validated stack slot must remain addressable");
        debug_assert_eq!(meta_start % meta_align(), 0);
        debug_assert_eq!(stack_start % crate::mem::page_size(), 0);
        debug_assert_eq!(cpu_data_start % layout.allocation_layout.align(), 0);
        println!(
            "Initializing per-CPU RAM for CPU{cpu_index} - hard id {hardware_id:#x}, meta @ \
             {meta_start:#x}, stack @ {stack_start:#x}, percpu @ {cpu_data_start:#x}"
        );
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_to_virt(link_phys_start) as *const u8,
                phys_to_virt(cpu_data_start),
                link_size,
            );
        }

        let meta_va = phys_to_virt(meta_start);
        debug_assert_eq!((meta_va as usize) % meta_align(), 0);

        let stack_top = stack_start
            .checked_add(stack_size())
            .expect("validated stack extent must not overflow");
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
    cpu_meta_start(cpu_index)
}

pub(crate) fn percpu_data_phys(cpu_index: usize) -> Option<usize> {
    cpu_data_start(cpu_index)
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
