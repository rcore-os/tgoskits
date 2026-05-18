use core::arch::asm;

use num_align::NumAlign;
use page_table_generic::{MapConfig, MemAttributes, PteConfig};

use crate::{
    console::print_mapping,
    mem::{__kimage_va, __percpu, __va, MB, PageTableInfo},
    smp::PerCpuMeta,
};

pub fn enable_mmu() -> ! {
    if let Err(err) = setup_page_table() {
        panic!("failed to setup riscv64 page table: {err:?}");
    }

    let mmu_entry_phys = super::entry::mmu_entry as *const () as usize;
    let meta = crate::smp::cpu_meta(crate::smp::cpu_idx()).unwrap();
    let v_sp = meta.stack_top_virt;
    let v_entry = __kimage_va(mmu_entry_phys) as usize;

    println!("MMU Entry point at physical address: {:#x}", mmu_entry_phys);
    println!("Enabling MMU...");

    super::write_satp(crate::mem::mmu::boot_table_addr());
    crate::mem::mmu::set_mmu_enabled();

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

pub fn enable_mmu_secondary(cpu_meta_paddr: usize) -> ! {
    let meta = unsafe { &*(cpu_meta_paddr as *const PerCpuMeta) };
    let v_sp = meta.stack_top_virt;
    let v_entry = meta.entry_virt;

    super::write_satp(meta.boot_table_paddr);
    crate::mem::mmu::set_mmu_enabled();

    unsafe {
        asm!(
            "mv a0, {meta}",
            "mv sp, {sp}",
            "jr {entry}",
            meta = in(reg) cpu_meta_paddr,
            sp = in(reg) v_sp,
            entry = in(reg) v_entry,
            options(noreturn)
        );
    }
}

fn setup_page_table() -> anyhow::Result<()> {
    println!("Mapping early memory regions...");

    let k_start = crate::mem::kimage_range().start;
    let mut table = crate::mem::mmu::new_boot_table();

    let ram_pte = PteConfig {
        valid: true,
        read: true,
        writable: true,
        executable: true,
        mem_attr: MemAttributes::Normal,
        ..Default::default()
    };

    for memory in crate::fdt::memories() {
        let start = memory.start;
        let size = memory.len();
        if size == 0 {
            continue;
        }

        print_mapping("Ram", start, start, size);

        table.map(&MapConfig {
            vaddr: start.into(),
            paddr: start.into(),
            size,
            pte: ram_pte,
            allow_huge: true,
            flush: false,
        })?;

        let high = __va(start) as usize;
        if high != start {
            print_mapping("Ram", high, start, size);
            table.map(&MapConfig {
                vaddr: high.into(),
                paddr: start.into(),
                size,
                pte: ram_pte,
                allow_huge: true,
                flush: false,
            })?;
        }
    }

    let v_start = __kimage_va(k_start);
    let size = crate::mem::kimage_range().len().align_up(2 * MB);

    print_mapping("KImage", v_start as _, k_start, size);

    table.map(&MapConfig {
        vaddr: v_start.into(),
        paddr: k_start.into(),
        size,
        pte: ram_pte,
        allow_huge: true,
        flush: false,
    })?;

    let percpu = crate::smp::percpu_range();
    print_mapping(
        "PerCpu",
        __percpu(percpu.start) as _,
        percpu.start,
        percpu.len(),
    );

    table.map(&MapConfig {
        vaddr: __percpu(percpu.start).into(),
        paddr: percpu.start.into(),
        size: percpu.len(),
        pte: PteConfig {
            valid: true,
            read: true,
            writable: true,
            executable: true,
            mem_attr: MemAttributes::PerCpu,
            ..Default::default()
        },
        allow_huge: true,
        flush: false,
    })?;

    let tb_addr = table.root_paddr();
    println!("Boot page table at physical address: {:#x}", tb_addr.raw());

    crate::mem::mmu::set_boot_table(table);
    crate::set_kernel_page_table_paddr(tb_addr.raw());
    let _ = PageTableInfo {
        asid: 0,
        addr: tb_addr.raw(),
    };

    Ok(())
}
