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
    // physical address readable across the SATP transition.
    let boot_info = unsafe { super::boot::read_at(cpu_boot_info_paddr) };
    let cpu_meta_paddr = boot_info.cpu_meta_paddr();
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

    let v_start = __kimage_va(k_start) as usize;
    let size = crate::mem::kimage_range().len().align_up(2 * MB);

    if !is_ram_alias(v_start, k_start) {
        print_mapping("KImage", v_start, k_start, size);

        table.map(&MapConfig {
            vaddr: v_start.into(),
            paddr: k_start.into(),
            size,
            pte: ram_pte,
            allow_huge: true,
            flush: false,
        })?;
    }

    let percpu = crate::smp::percpu_range();
    let percpu_vstart = __percpu(percpu.start) as usize;

    if !is_ram_alias(percpu_vstart, percpu.start) {
        print_mapping("PerCpu", percpu_vstart, percpu.start, percpu.len());

        table.map(&MapConfig {
            vaddr: percpu_vstart.into(),
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
    }

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

fn is_ram_alias(vaddr: usize, paddr: usize) -> bool {
    vaddr == paddr || vaddr == __va(paddr) as usize
}
