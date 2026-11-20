use core::{arch::asm, ops::Range};

use page_table_generic::MapConfig;

use crate::{
    console::print_mapping,
    mem::{__kimage_va, MemAttributes, NoMapError, PageTableInfo, PteConfig},
    smp::PerCpuMeta,
};

pub fn enable_mmu() -> ! {
    if let Err(err) = setup_page_table() {
        panic!("failed to setup riscv64 page table: {err:?}");
    }

    let mmu_entry_phys = super::entry::mmu_entry as *const () as usize;
    // The primary record remains on the dedicated linker boot stack. The
    // runtime stack is a distinct allocation, so its existing top-of-stack ABI
    // does not need a boot-record reservation.
    let v_sp = crate::smp::primary_stack_top_virtual(crate::smp::early_current_cpu_idx())
        .expect("primary reserved stack must be addressable before final per-CPU initialization");
    let v_entry = __kimage_va(mmu_entry_phys) as usize;

    println!("MMU Entry point at physical address: {:#x}", mmu_entry_phys);
    println!("Enabling MMU...");

    super::write_satp(crate::mem::mmu::boot_table_addr());

    println!("MMU enabled, jumping to {v_entry:#x}, sp={v_sp:#x}");

    unsafe {
        asm!(
            "mv sp, {sp}",
            "jr {entry}",
            sp = in(reg) v_sp,
            entry = in(reg) v_entry,
            options(noreturn)
        );
    }
}

pub fn enable_mmu_secondary(cpu_boot_info_paddr: usize) -> ! {
    // SAFETY: `_secondary_entry` constructs this record in its reserved stack
    // slot before entering Rust, and the early identity mapping keeps the
    // physical address readable across the SATP transition. Read the scalar
    // field directly because this function still runs with MMU disabled;
    // calling the validated Rust record parser would access the virtual stack
    // protector guard before the identity page table is installed.
    let cpu_meta_paddr = unsafe {
        core::ptr::read((cpu_boot_info_paddr + super::boot::CPU_META_PADDR_OFFSET) as *const usize)
    };
    let meta = unsafe { &*(cpu_meta_paddr as *const PerCpuMeta) };
    let v_sp = meta.stack_top_virt - super::boot::STACK_SIZE;
    let v_entry = meta.entry_virt;
    let trampoline_phys = super::entry::secondary_mmu_entry as *const () as usize;
    let secondary_entry_phys = crate::entry::secondary_entry as *const () as usize;
    let v_trampoline = v_entry.wrapping_add(trampoline_phys.wrapping_sub(secondary_entry_phys));

    super::write_satp(meta.boot_table_paddr);

    // SAFETY: the boot page table maps both the virtual stack and trampoline;
    // the explicit a0/a1 operands establish the trampoline's register ABI.
    unsafe {
        asm!(
            "mv sp, {sp}",
            "jr {trampoline}",
            in("a0") cpu_boot_info_paddr,
            in("a1") v_entry,
            sp = in(reg) v_sp,
            trampoline = in(reg) v_trampoline,
            options(noreturn)
        );
    }
}

fn setup_page_table() -> anyhow::Result<()> {
    println!("Mapping early memory regions...");

    let excluded = crate::mem::NoMapRegions::current()?;
    let cpu_area_region = crate::smp::cpu_area_region();
    let kernel = crate::mem::required_boot_mappings(
        &excluded,
        crate::mem::kimage_range(),
        cpu_area_region.clone(),
    )?;
    let mut table = crate::mem::mmu::new_boot_table();
    let mut map = |kind, mapping: MapConfig<PteConfig>| {
        print_mapping(
            kind,
            mapping.vaddr.as_usize(),
            mapping.paddr.as_usize(),
            mapping.size,
        );
        table.map(&mapping)
    };
    crate::fdt::for_each_memory(|range| {
        for mapping in ram_mappings(range)? {
            map("Ram", mapping)?;
        }
        Ok(())
    })?;
    if let Some(mapping) = kernel_mapping(kernel) {
        map("KImage", mapping)?;
    }
    if let Some(mapping) = cpu_area_mapping(cpu_area_region)? {
        map("PerCpu", mapping)?;
    }

    let tb_addr = table.root_paddr();
    println!(
        "Boot page table at physical address: {:#x}",
        tb_addr.as_usize()
    );

    crate::mem::mmu::set_boot_table(table);
    crate::set_kernel_page_table_paddr(tb_addr.as_usize());
    let _ = PageTableInfo {
        asid: 0,
        addr: tb_addr.as_usize(),
    };

    Ok(())
}

fn boot_mapping(
    virtual_start: usize,
    physical: Range<usize>,
    attr: MemAttributes,
) -> MapConfig<PteConfig> {
    MapConfig {
        vaddr: virtual_start.into(),
        paddr: physical.start.into(),
        size: physical.len(),
        pte: PteConfig {
            read: true,
            writable: true,
            executable: true,
            mem_attr: attr,
            ..Default::default()
        },
        allow_huge: true,
        flush: false,
    }
}

fn ram_mappings(
    range: Range<usize>,
) -> Result<impl Iterator<Item = MapConfig<PteConfig>>, NoMapError> {
    let start = range.start;
    let high = start
        .checked_add(super::addrspace::PAGE_OFFSET)
        .ok_or(NoMapError::InvalidRange)?;
    let identity = boot_mapping(start, range.clone(), MemAttributes::Normal);
    let direct = (high != start).then(|| boot_mapping(high, range, MemAttributes::Normal));
    Ok(core::iter::once(identity).chain(direct))
}

fn kernel_mapping(range: Range<usize>) -> Option<MapConfig<PteConfig>> {
    let vaddr = __kimage_va(range.start) as usize;
    (!is_ram_alias(vaddr, range.start)).then(|| boot_mapping(vaddr, range, MemAttributes::Normal))
}

fn cpu_area_mapping(range: Range<usize>) -> Result<Option<MapConfig<PteConfig>>, NoMapError> {
    let vaddr = range
        .start
        .checked_add(super::addrspace::PERCPU_BASE)
        .ok_or(NoMapError::InvalidRange)?;
    Ok((!is_ram_alias(vaddr, range.start))
        .then(|| boot_mapping(vaddr, range, MemAttributes::PerCpu)))
}

fn is_ram_alias(vaddr: usize, paddr: usize) -> bool {
    vaddr == paddr || paddr.checked_add(super::addrspace::PAGE_OFFSET) == Some(vaddr)
}

#[cfg(efi)]
pub(crate) fn select_efi_boot_ram(
    map: &[crate::mem::MemoryDescriptor],
    excluded: &crate::mem::NoMapRegions,
    kernel: Range<usize>,
    fdt: core::alloc::Layout,
    percpu: core::alloc::Layout,
) -> Result<Range<usize>, NoMapError> {
    use page_table_generic::TableMeta;

    use crate::mem::{mmu::mapping_table_frames, ram};

    type PageTable = super::Generic;
    let page = PageTable::PAGE_SIZE;
    let mut frames = 1usize;
    let mut account = |mapping| {
        frames = frames
            .checked_add(
                mapping_table_frames::<PageTable>(&mapping).ok_or(NoMapError::InvalidRange)?,
            )
            .ok_or(NoMapError::InvalidRange)?;
        Ok(())
    };
    crate::fdt::for_each_efi_ram_region(map, excluded, |range| {
        ram_mappings(range)?.try_for_each(&mut account)
    })?;
    if let Some(mapping) = kernel_mapping(kernel) {
        account(mapping)?;
    }

    // The executable mapping walk can share an intermediate table with an
    // earlier range. Summing per-range counts bounds allocation without
    // materializing or touching page tables before firmware releases the RAM.
    ram::select_range(map, |range| {
        let fits = || {
            let fdt = ram::planned_allocation(ram::initial_cursor(range.start), fdt, page)?;
            let percpu = ram::planned_allocation(fdt.end, percpu, page)?;
            let table_start = percpu.end;
            let percpu_frames = match cpu_area_mapping(percpu).ok()? {
                Some(mapping) => mapping_table_frames::<PageTable>(&mapping)?,
                None => 0,
            };
            let table_bytes = frames.checked_add(percpu_frames)?.checked_mul(page)?;
            Some(table_start.checked_add(table_bytes)? <= range.end)
        };
        fits() == Some(true)
    })
    .ok_or(NoMapError::Capacity)
}
